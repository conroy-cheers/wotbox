use std::{
    array,
    collections::{HashMap, VecDeque},
    future::Future,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio::sync::{mpsc, oneshot};

use crate::{
    db::{Database, StoredProviderState},
    model::{
        ApiPreferences, ProviderCircuitState, ProviderPolicyOverride, ProviderQueueCounts,
        ProviderStatus,
    },
};

const MAX_QUEUE_DEPTH: usize = 1_000;
const FOREGROUND_WAIT: Duration = Duration::from_secs(40);
const PRIORITY_COUNT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestClass {
    Interactive,
    Download,
    Manual,
    Scheduled,
    Background,
}

impl RequestClass {
    fn index(self) -> usize {
        match self {
            Self::Interactive => 0,
            Self::Download => 1,
            Self::Manual => 2,
            Self::Scheduled => 3,
            Self::Background => 4,
        }
    }

    fn is_background(self) -> bool {
        self == Self::Background
    }
}

#[derive(Debug, Clone)]
pub struct ProviderDefinition {
    pub id: String,
    pub display_name: String,
    pub kind: String,
    pub safe_minimum_interval: Duration,
    pub safe_background_minimum_interval: Duration,
    pub safe_max_concurrency: u32,
}

impl ProviderDefinition {
    pub fn tracker(name: &str) -> Self {
        Self {
            id: format!("tracker:{name}"),
            display_name: name.to_ascii_uppercase(),
            kind: "tracker".into(),
            safe_minimum_interval: Duration::from_millis(2_500),
            // Gazelle trackers commonly enforce a ten-request rolling minute.
            // Leave enough margin for clock and response-time variance instead
            // of running exactly at the theoretical six-second boundary.
            safe_background_minimum_interval: Duration::from_secs(7),
            safe_max_concurrency: 1,
        }
    }

    pub fn lastfm() -> Self {
        Self {
            id: "lastfm".into(),
            display_name: "Last.fm".into(),
            kind: "recommendation_source".into(),
            safe_minimum_interval: Duration::from_secs(1),
            safe_background_minimum_interval: Duration::from_secs(1),
            safe_max_concurrency: 1,
        }
    }

    pub fn apple() -> Self {
        Self {
            id: "apple".into(),
            display_name: "Apple Music".into(),
            kind: "recommendation_source".into(),
            safe_minimum_interval: Duration::from_secs(4),
            safe_background_minimum_interval: Duration::from_secs(4),
            safe_max_concurrency: 1,
        }
    }

    pub fn plex() -> Self {
        Self {
            id: "plex".into(),
            display_name: "Plex".into(),
            kind: "media_server".into(),
            safe_minimum_interval: Duration::from_secs(5),
            safe_background_minimum_interval: Duration::from_secs(5),
            safe_max_concurrency: 1,
        }
    }

    pub fn qbittorrent(name: &str) -> Self {
        Self {
            id: format!("qbittorrent:{name}"),
            display_name: format!("qBittorrent ({name})"),
            kind: "download_client".into(),
            safe_minimum_interval: Duration::from_millis(250),
            safe_background_minimum_interval: Duration::from_millis(250),
            safe_max_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureKind {
    RateLimited,
    HardBlocked,
    Authentication,
    Transient,
    Permanent,
}

#[derive(Debug, Clone)]
pub struct ProviderFailure {
    pub kind: ProviderFailureKind,
    pub message: String,
    pub retry_after: Option<Duration>,
}

impl ProviderFailure {
    pub fn new(kind: ProviderFailureKind, message: impl ToString) -> Self {
        Self {
            kind,
            message: message.to_string(),
            retry_after: None,
        }
    }

    pub fn retry_after(mut self, retry_after: Option<Duration>) -> Self {
        self.retry_after = retry_after;
        self
    }

    pub fn from_message(message: impl ToString) -> Self {
        let message = message.to_string();
        let normalized = message.to_ascii_lowercase();
        let kind = if normalized.contains("ip address has been banned")
            || normalized.contains("api key suspended")
            || normalized.contains("key has been suspended")
        {
            ProviderFailureKind::HardBlocked
        } else if normalized.contains("rate limit")
            || normalized.contains("too many requests")
            || normalized.contains("tracker_rate_limited")
        {
            ProviderFailureKind::RateLimited
        } else {
            ProviderFailureKind::Permanent
        };
        Self::new(kind, message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderRequestError {
    #[error("{provider} is {state}: {message}")]
    Unavailable {
        provider: String,
        state: String,
        message: String,
        retry_at: Option<DateTime<Utc>>,
    },
    #[error("{provider} request queue is busy")]
    Busy { provider: String },
    #[error("{provider} background request deferred until {retry_at}")]
    Deferred {
        provider: String,
        retry_at: DateTime<Utc>,
    },
    #[error("{provider}: {failure}")]
    Upstream {
        provider: String,
        failure: String,
        kind: ProviderFailureKind,
    },
    #[error("unknown provider {0}")]
    Unknown(String),
    #[error("provider coordinator stopped for {0}")]
    Stopped(String),
}

impl ProviderRequestError {
    pub fn provider_id(&self) -> Option<&str> {
        match self {
            Self::Unavailable { provider, .. }
            | Self::Busy { provider }
            | Self::Deferred { provider, .. }
            | Self::Upstream { provider, .. }
            | Self::Unknown(provider)
            | Self::Stopped(provider) => Some(provider),
        }
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Unavailable { .. } | Self::Busy { .. } | Self::Deferred { .. } | Self::Stopped(_)
        ) || matches!(
            self,
            Self::Upstream {
                kind: ProviderFailureKind::RateLimited
                    | ProviderFailureKind::HardBlocked
                    | ProviderFailureKind::Authentication,
                ..
            }
        )
    }
}

pub fn is_provider_unavailable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ProviderRequestError>())
        .is_some_and(ProviderRequestError::is_unavailable)
}

#[derive(Clone)]
pub struct ProviderGovernor {
    providers: Arc<HashMap<String, ProviderHandle>>,
}

#[derive(Clone)]
struct ProviderHandle {
    definition: ProviderDefinition,
    sender: mpsc::UnboundedSender<Command>,
}

struct Waiter {
    sender: oneshot::Sender<std::result::Result<(), ProviderRequestError>>,
}

enum Command {
    Acquire {
        class: RequestClass,
        sender: oneshot::Sender<std::result::Result<(), ProviderRequestError>>,
    },
    Complete(Option<ProviderFailure>, oneshot::Sender<()>),
    Abandoned,
    Status(oneshot::Sender<ProviderStatus>),
    Pause(oneshot::Sender<Result<ProviderStatus>>),
    Resume(oneshot::Sender<Result<ProviderStatus>>),
    Policy {
        value: ProviderPolicyOverride,
        sender: oneshot::Sender<Result<ProviderStatus>>,
    },
}

struct Permit {
    sender: mpsc::UnboundedSender<Command>,
    completed: bool,
}

impl Permit {
    async fn complete(mut self, failure: Option<ProviderFailure>) {
        self.completed = true;
        let (sender, receiver) = oneshot::channel();
        if self.sender.send(Command::Complete(failure, sender)).is_ok() {
            let _ = receiver.await;
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.sender.send(Command::Abandoned);
        }
    }
}

struct Actor {
    db: Database,
    definition: ProviderDefinition,
    stored: StoredProviderState,
    queues: [VecDeque<Waiter>; PRIORITY_COUNT],
    active: u32,
    receiver: mpsc::UnboundedReceiver<Command>,
}

impl ProviderGovernor {
    pub async fn new(
        db: Database,
        definitions: Vec<ProviderDefinition>,
        preferences: &ApiPreferences,
    ) -> Result<Self> {
        let mut providers = HashMap::new();
        for definition in definitions {
            let override_value = preferences
                .providers
                .get(&definition.id)
                .cloned()
                .unwrap_or_default();
            validate_override(&definition, &override_value)?;
            let minimum_interval_ms = override_value
                .minimum_interval_ms
                .unwrap_or(definition.safe_minimum_interval.as_millis() as u64);
            let background_minimum_interval_ms = override_value
                .background_minimum_interval_ms
                .unwrap_or(definition.safe_background_minimum_interval.as_millis() as u64);
            let max_concurrency = override_value
                .max_concurrency
                .unwrap_or(definition.safe_max_concurrency);
            let mut stored =
                db.provider_state(&definition.id)
                    .await?
                    .unwrap_or_else(|| StoredProviderState {
                        id: definition.id.clone(),
                        display_name: definition.display_name.clone(),
                        kind: definition.kind.clone(),
                        state: ProviderCircuitState::Available,
                        reason_code: None,
                        message: None,
                        last_request_at: None,
                        last_success_at: None,
                        last_failure_at: None,
                        retry_at: None,
                        last_background_request_at: None,
                        consecutive_failures: 0,
                        minimum_interval_ms,
                        background_minimum_interval_ms,
                        max_concurrency,
                    });
            stored.display_name.clone_from(&definition.display_name);
            stored.kind.clone_from(&definition.kind);
            stored.minimum_interval_ms = minimum_interval_ms;
            stored.background_minimum_interval_ms = background_minimum_interval_ms;
            stored.max_concurrency = max_concurrency;
            db.put_provider_state(&stored).await?;

            let (sender, receiver) = mpsc::unbounded_channel();
            let actor = Actor {
                db: db.clone(),
                definition: definition.clone(),
                stored,
                queues: array::from_fn(|_| VecDeque::new()),
                active: 0,
                receiver,
            };
            tokio::spawn(actor.run());
            providers.insert(definition.id.clone(), ProviderHandle { definition, sender });
        }
        Ok(Self {
            providers: Arc::new(providers),
        })
    }

    pub async fn execute<T, F, Fut>(
        &self,
        provider: &str,
        class: RequestClass,
        operation: F,
    ) -> std::result::Result<T, ProviderRequestError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<T, ProviderFailure>>,
    {
        let permit = self.acquire(provider, class).await?;
        match operation().await {
            Ok(value) => {
                permit.complete(None).await;
                Ok(value)
            }
            Err(failure) => {
                let error = ProviderRequestError::Upstream {
                    provider: provider.into(),
                    failure: failure.message.clone(),
                    kind: failure.kind,
                };
                permit.complete(Some(failure)).await;
                Err(error)
            }
        }
    }

    async fn acquire(
        &self,
        provider: &str,
        class: RequestClass,
    ) -> std::result::Result<Permit, ProviderRequestError> {
        let handle = self
            .providers
            .get(provider)
            .ok_or_else(|| ProviderRequestError::Unknown(provider.into()))?;
        let (sender, receiver) = oneshot::channel();
        handle
            .sender
            .send(Command::Acquire { class, sender })
            .map_err(|_| ProviderRequestError::Stopped(provider.into()))?;
        let granted = if class.is_background() {
            receiver
                .await
                .map_err(|_| ProviderRequestError::Stopped(provider.into()))?
        } else {
            tokio::time::timeout(FOREGROUND_WAIT, receiver)
                .await
                .map_err(|_| ProviderRequestError::Busy {
                    provider: provider.into(),
                })?
                .map_err(|_| ProviderRequestError::Stopped(provider.into()))?
        };
        granted?;
        Ok(Permit {
            sender: handle.sender.clone(),
            completed: false,
        })
    }

    pub async fn statuses(&self) -> Vec<ProviderStatus> {
        let mut statuses = Vec::new();
        for handle in self.providers.values() {
            let (sender, receiver) = oneshot::channel();
            if handle.sender.send(Command::Status(sender)).is_ok()
                && let Ok(status) = receiver.await
            {
                statuses.push(status);
            }
        }
        statuses.sort_by(|left, right| left.id.cmp(&right.id));
        statuses
    }

    pub async fn status(&self, provider: &str) -> Option<ProviderStatus> {
        let handle = self.providers.get(provider)?;
        let (sender, receiver) = oneshot::channel();
        handle.sender.send(Command::Status(sender)).ok()?;
        receiver.await.ok()
    }

    pub async fn pause(&self, provider: &str) -> Result<ProviderStatus> {
        self.control(provider, Command::Pause).await
    }

    pub async fn resume(&self, provider: &str) -> Result<ProviderStatus> {
        self.control(provider, Command::Resume).await
    }

    async fn control(
        &self,
        provider: &str,
        command: impl FnOnce(oneshot::Sender<Result<ProviderStatus>>) -> Command,
    ) -> Result<ProviderStatus> {
        let handle = self
            .providers
            .get(provider)
            .with_context(|| format!("unknown provider {provider}"))?;
        let (sender, receiver) = oneshot::channel();
        handle
            .sender
            .send(command(sender))
            .map_err(|_| anyhow::anyhow!("provider coordinator stopped"))?;
        receiver.await.context("provider coordinator stopped")?
    }

    pub async fn apply_preferences(&self, preferences: &ApiPreferences) -> Result<()> {
        for handle in self.providers.values() {
            let value = preferences
                .providers
                .get(&handle.definition.id)
                .cloned()
                .unwrap_or_default();
            validate_override(&handle.definition, &value)?;
            let (sender, receiver) = oneshot::channel();
            handle
                .sender
                .send(Command::Policy { value, sender })
                .map_err(|_| anyhow::anyhow!("provider coordinator stopped"))?;
            receiver.await.context("provider coordinator stopped")??;
        }
        Ok(())
    }

    pub fn validate_preferences(&self, preferences: &ApiPreferences) -> Result<()> {
        for (id, value) in &preferences.providers {
            if let Some(handle) = self.providers.get(id) {
                validate_override(&handle.definition, value)?;
            } else {
                tracing::debug!(provider = %id, "retaining policy for an unconfigured provider");
            }
        }
        Ok(())
    }
}

impl Actor {
    async fn run(mut self) {
        loop {
            self.grant_ready().await;
            let wait = self.next_wait();
            let command = if let Some(wait) = wait {
                tokio::select! {
                    command = self.receiver.recv() => command,
                    () = tokio::time::sleep(wait) => continue,
                }
            } else {
                self.receiver.recv().await
            };
            let Some(command) = command else {
                break;
            };
            self.handle(command).await;
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Acquire { class, sender } => {
                if let Some(error) = self.unavailable_error() {
                    let _ = sender.send(Err(error));
                } else if self.queue_depth() >= MAX_QUEUE_DEPTH {
                    let _ = sender.send(Err(ProviderRequestError::Busy {
                        provider: self.definition.id.clone(),
                    }));
                } else if class.is_background()
                    && let Some(retry_at) = self.background_defer_until()
                {
                    let _ = sender.send(Err(ProviderRequestError::Deferred {
                        provider: self.definition.id.clone(),
                        retry_at,
                    }));
                } else {
                    self.queues[class.index()].push_back(Waiter { sender });
                }
            }
            Command::Complete(failure, sender) => {
                self.active = self.active.saturating_sub(1);
                self.record_outcome(failure).await;
                let _ = sender.send(());
            }
            Command::Abandoned => {
                self.active = self.active.saturating_sub(1);
            }
            Command::Status(sender) => {
                let _ = sender.send(self.status());
            }
            Command::Pause(sender) => {
                if !matches!(
                    self.stored.state,
                    ProviderCircuitState::Available | ProviderCircuitState::HalfOpen
                ) {
                    let _ = sender.send(Err(anyhow::anyhow!(
                        "{} cannot be paused while {}",
                        self.definition.id,
                        self.stored.state.as_str()
                    )));
                    return;
                }
                self.stored.state = ProviderCircuitState::Paused;
                self.stored.reason_code = Some("manual_pause".into());
                self.stored.message = Some("Paused by the user".into());
                self.stored.retry_at = None;
                self.reject_waiters();
                let result = self.persist().await.map(|()| self.status());
                let _ = sender.send(result);
            }
            Command::Resume(sender) => {
                if !matches!(
                    self.stored.state,
                    ProviderCircuitState::Blocked | ProviderCircuitState::Paused
                ) {
                    let _ = sender.send(Err(anyhow::anyhow!(
                        "{} is not manually resumable while {}",
                        self.definition.id,
                        self.stored.state.as_str()
                    )));
                    return;
                }
                self.stored.state = ProviderCircuitState::HalfOpen;
                self.stored.reason_code = Some("manual_resume".into());
                self.stored.message = Some("Waiting for one successful request".into());
                self.stored.retry_at = None;
                let result = self.persist().await.map(|()| self.status());
                let _ = sender.send(result);
            }
            Command::Policy { value, sender } => {
                let result = validate_override(&self.definition, &value).map(|()| {
                    self.stored.minimum_interval_ms = value
                        .minimum_interval_ms
                        .unwrap_or(self.definition.safe_minimum_interval.as_millis() as u64);
                    self.stored.background_minimum_interval_ms =
                        value.background_minimum_interval_ms.unwrap_or(
                            self.definition.safe_background_minimum_interval.as_millis() as u64,
                        );
                    self.stored.max_concurrency = value
                        .max_concurrency
                        .unwrap_or(self.definition.safe_max_concurrency);
                });
                let result = match result {
                    Ok(()) => self.persist().await.map(|()| self.status()),
                    Err(error) => Err(error),
                };
                let _ = sender.send(result);
            }
        }
    }

    async fn grant_ready(&mut self) {
        self.advance_cooldown().await;
        loop {
            if self.active >= self.stored.max_concurrency
                || self.queue_depth() == 0
                || self.unavailable_error().is_some()
                || (self.stored.state == ProviderCircuitState::HalfOpen && self.active > 0)
            {
                return;
            }
            let Some(queue) = self.best_queue() else {
                return;
            };
            if self
                .next_eligible_at(queue)
                .is_some_and(|next| next > Utc::now())
            {
                return;
            }
            let Some(waiter) = self.queues[queue].pop_front() else {
                continue;
            };
            if waiter.sender.is_closed() {
                continue;
            }
            self.active += 1;
            let granted_at = Utc::now();
            self.stored.last_request_at = Some(granted_at);
            if matches!(
                queue,
                value if value == RequestClass::Scheduled.index()
                    || value == RequestClass::Background.index()
            ) {
                self.stored.last_background_request_at = Some(granted_at);
            }
            if let Err(error) = self.persist().await {
                self.active = self.active.saturating_sub(1);
                let _ = waiter.sender.send(Err(ProviderRequestError::Stopped(
                    self.definition.id.clone(),
                )));
                tracing::error!(provider = %self.definition.id, %error, "could not persist provider request permit");
                return;
            }
            let _ = waiter.sender.send(Ok(()));
            if self.stored.minimum_interval_ms > 0 {
                return;
            }
        }
    }

    async fn advance_cooldown(&mut self) {
        if self.stored.state == ProviderCircuitState::Cooldown
            && self
                .stored
                .retry_at
                .is_some_and(|retry_at| retry_at <= Utc::now())
        {
            self.stored.state = ProviderCircuitState::HalfOpen;
            self.stored.message =
                Some("Cooldown elapsed; waiting for one successful request".into());
            self.stored.retry_at = None;
            let _ = self.persist().await;
        }
    }

    async fn record_outcome(&mut self, failure: Option<ProviderFailure>) {
        let now = Utc::now();
        if matches!(
            self.stored.state,
            ProviderCircuitState::Blocked | ProviderCircuitState::Paused
        ) {
            if failure.is_some() {
                self.stored.last_failure_at = Some(now);
                self.stored.consecutive_failures =
                    self.stored.consecutive_failures.saturating_add(1);
            } else {
                self.stored.last_success_at = Some(now);
            }
            if let Err(error) = self.persist().await {
                tracing::error!(provider = %self.definition.id, %error, "could not persist provider outcome");
            }
            return;
        }
        match failure {
            None => {
                self.stored.last_success_at = Some(now);
                if self.stored.state != ProviderCircuitState::Cooldown {
                    self.stored.state = ProviderCircuitState::Available;
                    self.stored.reason_code = None;
                    self.stored.message = None;
                    self.stored.retry_at = None;
                    self.stored.consecutive_failures = 0;
                }
            }
            Some(failure) => {
                self.stored.last_failure_at = Some(now);
                self.stored.message = Some(failure.message.chars().take(500).collect());
                match failure.kind {
                    ProviderFailureKind::RateLimited => {
                        self.stored.consecutive_failures =
                            self.stored.consecutive_failures.saturating_add(1);
                        let exponent = self.stored.consecutive_failures.saturating_sub(1).min(4);
                        let fallback_minutes = (15_i64 * (1_i64 << exponent)).min(360);
                        let delay = failure
                            .retry_after
                            .unwrap_or_else(|| Duration::from_secs((fallback_minutes * 60) as u64));
                        self.stored.state = ProviderCircuitState::Cooldown;
                        self.stored.reason_code = Some("rate_limited".into());
                        self.stored.retry_at = Some(
                            now + chrono::Duration::from_std(delay)
                                .unwrap_or_else(|_| chrono::Duration::hours(6)),
                        );
                        self.reject_waiters();
                    }
                    ProviderFailureKind::HardBlocked | ProviderFailureKind::Authentication => {
                        self.stored.consecutive_failures =
                            self.stored.consecutive_failures.saturating_add(1);
                        self.stored.state = ProviderCircuitState::Blocked;
                        self.stored.reason_code = Some(
                            if failure.kind == ProviderFailureKind::Authentication {
                                "authentication_failed"
                            } else {
                                "hard_blocked"
                            }
                            .into(),
                        );
                        self.stored.retry_at = None;
                        self.reject_waiters();
                    }
                    ProviderFailureKind::Transient => {
                        let was_half_open = self.stored.state == ProviderCircuitState::HalfOpen;
                        self.stored.consecutive_failures =
                            self.stored.consecutive_failures.saturating_add(1);
                        if was_half_open || self.stored.consecutive_failures >= 3 {
                            let exponent =
                                self.stored.consecutive_failures.saturating_sub(3).min(5);
                            let minutes = (1_i64 << exponent).min(30);
                            self.stored.state = ProviderCircuitState::Cooldown;
                            self.stored.reason_code = Some("transient_failure".into());
                            self.stored.retry_at = Some(now + chrono::Duration::minutes(minutes));
                            self.reject_waiters();
                        }
                    }
                    ProviderFailureKind::Permanent => {
                        self.stored.consecutive_failures = 0;
                        if self.stored.state == ProviderCircuitState::HalfOpen {
                            self.stored.state = ProviderCircuitState::Available;
                            self.stored.reason_code = None;
                            self.stored.message = None;
                        }
                    }
                }
            }
        }
        if let Err(error) = self.persist().await {
            tracing::error!(provider = %self.definition.id, %error, "could not persist provider outcome");
        }
    }

    fn unavailable_error(&self) -> Option<ProviderRequestError> {
        let unavailable = matches!(
            self.stored.state,
            ProviderCircuitState::Cooldown
                | ProviderCircuitState::Blocked
                | ProviderCircuitState::Paused
        ) && !matches!(
            (self.stored.state, self.stored.retry_at),
            (ProviderCircuitState::Cooldown, Some(retry_at)) if retry_at <= Utc::now()
        );
        unavailable.then(|| ProviderRequestError::Unavailable {
            provider: self.definition.id.clone(),
            state: self.stored.state.as_str().into(),
            message: self
                .stored
                .message
                .clone()
                .unwrap_or_else(|| "Provider is unavailable".into()),
            retry_at: self.stored.retry_at,
        })
    }

    fn reject_waiters(&mut self) {
        let Some(error) = self.unavailable_error() else {
            return;
        };
        let (state, message, retry_at) = match error {
            ProviderRequestError::Unavailable {
                state,
                message,
                retry_at,
                ..
            } => (state, message, retry_at),
            _ => return,
        };
        for queue in &mut self.queues {
            while let Some(waiter) = queue.pop_front() {
                let _ = waiter.sender.send(Err(ProviderRequestError::Unavailable {
                    provider: self.definition.id.clone(),
                    state: state.clone(),
                    message: message.clone(),
                    retry_at,
                }));
            }
        }
    }

    fn best_queue(&self) -> Option<usize> {
        self.queues
            .iter()
            .enumerate()
            .find_map(|(index, queue)| (!queue.is_empty()).then_some(index))
    }

    fn next_wait(&self) -> Option<Duration> {
        if self.queue_depth() == 0 || self.active >= self.stored.max_concurrency {
            return None;
        }
        if self.stored.state == ProviderCircuitState::Cooldown {
            return self
                .stored
                .retry_at
                .and_then(|value| (value - Utc::now()).to_std().ok());
        }
        self.best_queue()
            .and_then(|queue| self.next_eligible_at(queue))
            .and_then(|next| (next - Utc::now()).to_std().ok())
    }

    fn next_eligible_at(&self, queue: usize) -> Option<DateTime<Utc>> {
        let global = self.stored.last_request_at.map(|last| {
            last + chrono::Duration::milliseconds(
                i64::try_from(self.stored.minimum_interval_ms).unwrap_or(i64::MAX),
            )
        });
        let background = ([RequestClass::Scheduled, RequestClass::Background]
            .into_iter()
            .any(|class| class.index() == queue))
        .then(|| {
            self.stored.last_background_request_at.map(|last| {
                last + chrono::Duration::milliseconds(
                    i64::try_from(self.stored.background_minimum_interval_ms).unwrap_or(i64::MAX),
                )
            })
        })
        .flatten();
        global.into_iter().chain(background).max()
    }

    fn background_defer_until(&self) -> Option<DateTime<Utc>> {
        let foreground_waiting = self.queues[..RequestClass::Background.index()]
            .iter()
            .any(|queue| !queue.is_empty());
        let eligible_at = self.next_eligible_at(RequestClass::Background.index());
        if self.active >= self.stored.max_concurrency || foreground_waiting {
            return Some(
                eligible_at
                    .unwrap_or_else(|| Utc::now() + chrono::Duration::seconds(1))
                    .max(Utc::now() + chrono::Duration::seconds(1)),
            );
        }
        eligible_at.filter(|next| *next > Utc::now())
    }

    fn queue_depth(&self) -> usize {
        self.queues.iter().map(VecDeque::len).sum()
    }

    fn status(&self) -> ProviderStatus {
        let queued = ProviderQueueCounts {
            interactive: self.queues[0].len(),
            download: self.queues[1].len(),
            manual: self.queues[2].len(),
            scheduled: self.queues[3].len(),
            background: self.queues[4].len(),
        };
        ProviderStatus {
            id: self.definition.id.clone(),
            display_name: self.definition.display_name.clone(),
            kind: self.definition.kind.clone(),
            state: self.stored.state,
            reason_code: self.stored.reason_code.clone(),
            message: self.stored.message.clone(),
            last_request_at: self.stored.last_request_at,
            last_success_at: self.stored.last_success_at,
            last_failure_at: self.stored.last_failure_at,
            retry_at: self.stored.retry_at,
            last_background_request_at: self.stored.last_background_request_at,
            consecutive_failures: self.stored.consecutive_failures,
            minimum_interval_ms: self.stored.minimum_interval_ms,
            safe_minimum_interval_ms: self.definition.safe_minimum_interval.as_millis() as u64,
            background_minimum_interval_ms: self.stored.background_minimum_interval_ms,
            safe_background_minimum_interval_ms: self
                .definition
                .safe_background_minimum_interval
                .as_millis() as u64,
            max_concurrency: self.stored.max_concurrency,
            safe_max_concurrency: self.definition.safe_max_concurrency,
            queued,
            can_pause: matches!(
                self.stored.state,
                ProviderCircuitState::Available | ProviderCircuitState::HalfOpen
            ),
            can_resume: matches!(
                self.stored.state,
                ProviderCircuitState::Blocked | ProviderCircuitState::Paused
            ),
        }
    }

    async fn persist(&self) -> Result<()> {
        self.db.put_provider_state(&self.stored).await
    }
}

fn validate_override(
    definition: &ProviderDefinition,
    value: &ProviderPolicyOverride,
) -> Result<()> {
    if let Some(interval) = value.minimum_interval_ms
        && interval < definition.safe_minimum_interval.as_millis() as u64
    {
        anyhow::bail!(
            "{} minimumIntervalMs cannot be lower than {}",
            definition.id,
            definition.safe_minimum_interval.as_millis()
        );
    }
    if let Some(interval) = value.background_minimum_interval_ms
        && interval < definition.safe_background_minimum_interval.as_millis() as u64
    {
        anyhow::bail!(
            "{} backgroundMinimumIntervalMs cannot be lower than {}",
            definition.id,
            definition.safe_background_minimum_interval.as_millis()
        );
    }
    if let Some(concurrency) = value.max_concurrency
        && (concurrency == 0 || concurrency > definition.safe_max_concurrency)
    {
        anyhow::bail!(
            "{} maxConcurrency must be between 1 and {}",
            definition.id,
            definition.safe_max_concurrency
        );
    }
    Ok(())
}

pub fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let retry_at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    (retry_at.with_timezone(&Utc) - Utc::now()).to_std().ok()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn lower_only_overrides_reject_unsafe_values() {
        let definition = ProviderDefinition::tracker("ops");
        assert!(
            validate_override(
                &definition,
                &ProviderPolicyOverride {
                    minimum_interval_ms: Some(2_000),
                    background_minimum_interval_ms: None,
                    max_concurrency: None,
                }
            )
            .is_err()
        );
        assert!(
            validate_override(
                &definition,
                &ProviderPolicyOverride {
                    minimum_interval_ms: Some(3_000),
                    background_minimum_interval_ms: Some(7_000),
                    max_concurrency: Some(1),
                }
            )
            .is_ok()
        );
        assert_eq!(
            definition.safe_background_minimum_interval,
            Duration::from_secs(7)
        );
    }

    #[test]
    fn classifies_known_hard_and_rate_limit_messages() {
        assert_eq!(
            ProviderFailure::from_message("Your IP address has been banned").kind,
            ProviderFailureKind::HardBlocked
        );
        assert_eq!(
            ProviderFailure::from_message("Rate limit exceeded").kind,
            ProviderFailureKind::RateLimited
        );
    }

    #[tokio::test]
    async fn rate_limits_persist_and_reject_manual_cooldown_bypass() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("provider.sqlite"))
            .await
            .expect("database");
        let governor = ProviderGovernor::new(
            db.clone(),
            vec![ProviderDefinition {
                id: "test".into(),
                display_name: "Test".into(),
                kind: "test".into(),
                safe_minimum_interval: Duration::ZERO,
                safe_background_minimum_interval: Duration::ZERO,
                safe_max_concurrency: 1,
            }],
            &ApiPreferences::default(),
        )
        .await
        .expect("governor");

        let result = governor
            .execute("test", RequestClass::Interactive, || async {
                Err::<(), _>(
                    ProviderFailure::new(ProviderFailureKind::RateLimited, "slow down")
                        .retry_after(Some(Duration::from_secs(60))),
                )
            })
            .await;
        assert!(matches!(
            result,
            Err(ProviderRequestError::Upstream {
                kind: ProviderFailureKind::RateLimited,
                ..
            })
        ));

        let stored = db
            .provider_state("test")
            .await
            .expect("stored state")
            .expect("state exists");
        assert_eq!(stored.state, ProviderCircuitState::Cooldown);
        assert!(stored.retry_at.is_some());

        let called = Arc::new(AtomicBool::new(false));
        let operation_called = called.clone();
        let blocked = governor
            .execute("test", RequestClass::Interactive, || async move {
                operation_called.store(true, Ordering::SeqCst);
                Ok::<_, ProviderFailure>(())
            })
            .await;
        assert!(matches!(
            blocked,
            Err(ProviderRequestError::Unavailable { .. })
        ));
        assert!(!called.load(Ordering::SeqCst));

        assert!(governor.resume("test").await.is_err());
    }

    #[tokio::test]
    async fn an_in_flight_success_cannot_clear_a_manual_pause() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("paused.sqlite"))
            .await
            .expect("database");
        let governor = ProviderGovernor::new(
            db,
            vec![ProviderDefinition {
                id: "test".into(),
                display_name: "Test".into(),
                kind: "test".into(),
                safe_minimum_interval: Duration::ZERO,
                safe_background_minimum_interval: Duration::ZERO,
                safe_max_concurrency: 1,
            }],
            &ApiPreferences::default(),
        )
        .await
        .expect("governor");
        let (started_sender, started_receiver) = oneshot::channel();
        let (release_sender, release_receiver) = oneshot::channel();
        let request_governor = governor.clone();
        let request = tokio::spawn(async move {
            request_governor
                .execute("test", RequestClass::Interactive, || async move {
                    let _ = started_sender.send(());
                    let _ = release_receiver.await;
                    Ok::<_, ProviderFailure>(())
                })
                .await
        });
        started_receiver.await.expect("request started");
        governor.pause("test").await.expect("pause");
        let _ = release_sender.send(());
        request
            .await
            .expect("request task")
            .expect("request result");
        assert_eq!(
            governor.status("test").await.expect("status").state,
            ProviderCircuitState::Paused
        );
    }

    #[tokio::test]
    async fn background_requests_defer_without_queueing_or_consuming_a_request() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("background-spacing.sqlite"))
            .await
            .expect("database");
        let governor = ProviderGovernor::new(
            db,
            vec![ProviderDefinition {
                id: "test".into(),
                display_name: "Test".into(),
                kind: "test".into(),
                safe_minimum_interval: Duration::ZERO,
                safe_background_minimum_interval: Duration::from_secs(60),
                safe_max_concurrency: 1,
            }],
            &ApiPreferences::default(),
        )
        .await
        .expect("governor");

        governor
            .execute("test", RequestClass::Background, || async {
                Ok::<_, ProviderFailure>(())
            })
            .await
            .expect("first request");
        let deferred = governor
            .execute("test", RequestClass::Background, || async {
                Ok::<_, ProviderFailure>(())
            })
            .await;
        assert!(matches!(
            deferred,
            Err(ProviderRequestError::Deferred { .. })
        ));
        let status = governor.status("test").await.expect("status");
        assert_eq!(status.queued.background, 0);
        assert_eq!(status.background_minimum_interval_ms, 60_000);
        assert!(status.last_background_request_at.is_some());
    }

    #[tokio::test]
    async fn scheduled_requests_use_bulk_spacing_without_delaying_interactive_work() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("scheduled-spacing.sqlite"))
            .await
            .expect("database");
        let governor = ProviderGovernor::new(
            db,
            vec![ProviderDefinition {
                id: "test".into(),
                display_name: "Test".into(),
                kind: "test".into(),
                safe_minimum_interval: Duration::ZERO,
                safe_background_minimum_interval: Duration::from_millis(300),
                safe_max_concurrency: 1,
            }],
            &ApiPreferences::default(),
        )
        .await
        .expect("governor");

        governor
            .execute("test", RequestClass::Scheduled, || async {
                Ok::<_, ProviderFailure>(())
            })
            .await
            .expect("first scheduled request");
        let first_bulk_request = governor
            .status("test")
            .await
            .expect("first status")
            .last_background_request_at
            .expect("first bulk timestamp");

        governor
            .execute("test", RequestClass::Interactive, || async {
                Ok::<_, ProviderFailure>(())
            })
            .await
            .expect("interactive request");
        let interactive_status = governor.status("test").await.expect("interactive status");
        assert_eq!(
            interactive_status.last_background_request_at,
            Some(first_bulk_request)
        );
        assert!(interactive_status.last_request_at > Some(first_bulk_request));

        governor
            .execute("test", RequestClass::Scheduled, || async {
                Ok::<_, ProviderFailure>(())
            })
            .await
            .expect("second scheduled request");
        let second_bulk_request = governor
            .status("test")
            .await
            .expect("second status")
            .last_background_request_at
            .expect("second bulk timestamp");
        assert!(second_bulk_request - first_bulk_request >= chrono::Duration::milliseconds(300));
    }
}
