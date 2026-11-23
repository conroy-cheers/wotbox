use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use tokio::{sync::watch, task::JoinHandle, time::MissedTickBehavior};

use crate::{
    api::{AppState, refresh_library_projection, spawn_channel_scheduler},
    db::{Database, DownloadObservation},
    provider::ProviderRequestError,
    qbittorrent::{DownloadClient, DownloadClientDelta},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeScope {
    Activity,
    Assets,
    Catalog,
    Channels,
    Operations,
    Providers,
    Settings,
    Global,
}

impl ChangeScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activity => "activity",
            Self::Assets => "assets",
            Self::Catalog => "catalog",
            Self::Channels => "channels",
            Self::Operations => "operations",
            Self::Providers => "providers",
            Self::Settings => "settings",
            Self::Global => "global",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChangeSet {
    scopes: BTreeSet<ChangeScope>,
    resources: BTreeSet<String>,
    reason: String,
    payload: Option<Value>,
}

impl ChangeSet {
    pub fn new(reason: impl Into<String>, scopes: impl IntoIterator<Item = ChangeScope>) -> Self {
        Self {
            scopes: scopes.into_iter().collect(),
            resources: BTreeSet::new(),
            reason: reason.into(),
            payload: None,
        }
    }

    pub fn with_resources(
        mut self,
        resources: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.resources.extend(resources.into_iter().map(Into::into));
        self
    }

    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = Some(payload);
        self
    }

    pub fn encode_scopes(&self) -> String {
        self.scopes
            .iter()
            .map(|scope| scope.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn encode_resources(&self) -> String {
        self.resources.iter().cloned().collect::<Vec<_>>().join(",")
    }

    pub fn payload(&self) -> Option<Value> {
        self.payload.clone()
    }
}

#[derive(Clone)]
pub struct ChangeHub {
    db: Database,
    cursor: watch::Sender<i64>,
    epoch: Arc<str>,
}

impl ChangeHub {
    pub async fn new(db: Database) -> Result<Self> {
        let cursor = db.latest_change_cursor().await?;
        let (sender, _) = watch::channel(cursor);
        let epoch = db.change_epoch().await?.into();
        Ok(Self {
            db,
            cursor: sender,
            epoch,
        })
    }

    pub async fn publish(&self, change: ChangeSet) -> Result<i64> {
        let cursor = self
            .db
            .append_resource_change_event(
                &change.encode_scopes(),
                &change.encode_resources(),
                change.reason(),
                change.payload(),
            )
            .await?;
        self.cursor.send_replace(cursor);
        tracing::debug!(cursor, scopes = %change.encode_scopes(), reason = %change.reason(), "published change event");
        Ok(cursor)
    }

    pub fn wake(&self) {
        self.cursor.send_modify(|_| {});
    }

    pub fn encode_cursor(&self, cursor: i64) -> String {
        format!("{}:{cursor}", self.epoch)
    }

    pub fn decode_cursor(&self, cursor: &str) -> Option<i64> {
        let (epoch, sequence) = cursor.split_once(':')?;
        if epoch != self.epoch.as_ref() {
            return None;
        }
        sequence
            .parse::<i64>()
            .ok()
            .filter(|sequence| *sequence >= 0)
    }

    pub fn subscribe(&self) -> watch::Receiver<i64> {
        self.cursor.subscribe()
    }
}

pub struct ApplicationRuntime {
    stop: watch::Sender<bool>,
    handles: Vec<JoinHandle<()>>,
}

impl ApplicationRuntime {
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        for mut handle in self.handles {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() || tokio::time::timeout(remaining, &mut handle).await.is_err() {
                handle.abort();
            }
        }
    }
}

pub fn spawn_application_observers(state: Arc<AppState>) -> ApplicationRuntime {
    let (stop, receiver) = watch::channel(false);
    let mut handles = vec![
        tokio::spawn(provider_observer(state.clone(), receiver.clone())),
        tokio::spawn(library_projection_observer(state.clone(), receiver.clone())),
        tokio::spawn(change_retention(state.clone(), receiver.clone())),
        spawn_channel_scheduler(state.clone(), receiver.clone()),
    ];
    for (name, client) in &state.download_clients {
        handles.push(tokio::spawn(download_client_observer(
            state.clone(),
            name.clone(),
            client.clone(),
            receiver.clone(),
        )));
    }
    ApplicationRuntime { stop, handles }
}

async fn library_projection_observer(state: Arc<AppState>, mut stop: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = interval.tick() => {}
        }
        if let Err(error) = refresh_library_projection(&state).await {
            tracing::warn!(%error, "could not refresh incremental library projection");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

async fn provider_observer(state: Arc<AppState>, mut stop: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut previous = String::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = interval.tick() => {}
        }
        let fingerprint = provider_change_fingerprint(state.providers.statuses().await);
        if !previous.is_empty() && fingerprint != previous {
            state
                .publish(
                    ChangeSet::new("provider_state_changed", [ChangeScope::Providers])
                        .with_resources(["providers"]),
                )
                .await;
        }
        previous = fingerprint;
    }
}

fn provider_change_fingerprint(mut statuses: Vec<crate::model::ProviderStatus>) -> String {
    // Request/success timestamps are activity telemetry, not provider availability.
    // Background observers update them on every successful poll, so including them
    // would turn this state observer into a periodic invalidation loop.
    for status in &mut statuses {
        status.last_request_at = None;
        status.last_success_at = None;
        status.last_background_request_at = None;
        status.active_requests = 0;
    }
    serde_json::to_string(&statuses).unwrap_or_default()
}

async fn download_client_observer(
    state: Arc<AppState>,
    name: String,
    client: Arc<dyn DownloadClient>,
    mut stop: watch::Receiver<bool>,
) {
    let mut full_scan = tokio::time::Instant::now();
    let mut delay = Duration::ZERO;
    let mut failures = 0_u32;
    let mut pending = None;
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = tokio::time::sleep(delay) => {}
        }
        delay = Duration::from_secs(1);
        if pending.is_none() {
            let started_at = Utc::now();
            let reset = tokio::time::Instant::now() >= full_scan;
            let response = tokio::select! {
                _ = stop.changed() => break,
                response = client.sync_downloads(reset) => response,
            };
            match response {
                Ok(delta) => {
                    failures = 0;
                    if !delta.full_update && delta.downloads.is_empty() && delta.removed.is_empty()
                    {
                        client.acknowledge_downloads().await;
                        continue;
                    }
                    pending = Some((started_at, delta));
                }
                Err(error) => {
                    if let Some(ProviderRequestError::Deferred { retry_at, .. }) =
                        error.downcast_ref::<ProviderRequestError>()
                    {
                        delay = (*retry_at - Utc::now())
                            .to_std()
                            .unwrap_or_default()
                            .max(delay);
                        continue;
                    }
                    failures = failures.saturating_add(1);
                    delay = Duration::from_secs(2_u64.pow(failures.min(4)));
                    tracing::warn!(client = %name, failures, %error, "download client observation failed");
                    continue;
                }
            }
        }
        let Some((started_at, delta)) = &pending else {
            continue;
        };
        match apply_download_delta(&state, &name, *started_at, delta).await {
            Ok(()) => {
                client.acknowledge_downloads().await;
                if delta.full_update {
                    full_scan = tokio::time::Instant::now() + Duration::from_secs(300);
                }
                pending = None;
            }
            Err(error) => {
                // Keep the decoded batch until all local writes have succeeded. Do
                // not request a later RID while this client's work is still pending.
                tracing::warn!(client = %name, %error, "retrying download client delta");
                delay = Duration::from_secs(2);
            }
        }
    }
}

async fn apply_download_delta(
    state: &AppState,
    name: &str,
    started_at: chrono::DateTime<Utc>,
    delta: &DownloadClientDelta,
) -> Result<()> {
    let observations = delta
        .downloads
        .iter()
        .map(|download| DownloadObservation {
            torrent_name: Some(download.name.clone()),
            live: download.live.clone(),
            announce_host: download.announce_host.clone(),
            tracker: download
                .announce_host
                .as_ref()
                .and_then(|host| state.announce_hosts.get(host))
                .cloned(),
            plex_target: state
                .plex
                .as_ref()
                .and_then(|plex| plex.target_for_path(&download.live.save_path)),
        })
        .collect::<Vec<_>>();
    let profiles = state
        .profiles
        .iter()
        .filter(|(_, profile)| profile.client == name)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    state
        .db
        .publish_client_observation(name, started_at, delta, &observations, &profiles)
        .await?;
    state.background_jobs.wake();
    state.changes.wake();
    Ok(())
}

async fn change_retention(state: Arc<AppState>, mut stop: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(3600));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = interval.tick() => {
                if let Err(error) = state.db.prune_library_projection_stage(None).await {
                    tracing::warn!(%error, "could not prune abandoned Library projection stages");
                }
                if let Err(error) = state.db.prune_inactive_inventory().await {
                    tracing::warn!(%error, "could not prune inactive client inventory");
                }
                match state.db.prune_change_events(Utc::now() - chrono::Duration::hours(24)).await {
                    Ok(pruned) if pruned > 0 => tracing::debug!(pruned, "pruned change events"),
                    Ok(_) => {},
                    Err(error) => tracing::warn!(%error, "could not prune change events"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::provider_change_fingerprint;
    use crate::model::{DownloadState, ProviderCircuitState, ProviderQueueCounts, ProviderStatus};

    fn provider() -> ProviderStatus {
        ProviderStatus {
            id: "qbittorrent".into(),
            display_name: "qBittorrent".into(),
            kind: "download_client".into(),
            state: ProviderCircuitState::Available,
            reason_code: None,
            message: None,
            last_request_at: None,
            last_success_at: None,
            last_failure_at: None,
            retry_at: None,
            last_background_request_at: None,
            consecutive_failures: 0,
            minimum_interval_ms: 250,
            safe_minimum_interval_ms: 250,
            background_minimum_interval_ms: 250,
            safe_background_minimum_interval_ms: 250,
            max_concurrency: 1,
            safe_max_concurrency: 1,
            queued: ProviderQueueCounts::default(),
            active_requests: 0,
            can_pause: true,
            can_resume: false,
        }
    }

    #[tokio::test]
    async fn successful_full_snapshot_reconciles_only_confirmed_missing_jobs() {
        use crate::config::{Config, DownloadProfileConfig};

        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            database_path: dir.path().join("snapshot.sqlite"),
            ..Config::default()
        };
        config.download_profiles.insert(
            "music".into(),
            DownloadProfileConfig {
                client: "music".into(),
                save_path: "/music".into(),
                tag: String::new(),
                start_paused: false,
            },
        );
        let state = super::AppState::new(&config).await.unwrap();
        let profile = &state.profiles["music"];
        let active = state
            .db
            .accept_download("ops", 1, profile, false, None)
            .await
            .unwrap();
        let queued = state
            .db
            .accept_download("ops", 2, profile, false, None)
            .await
            .unwrap();
        for job in [&active, &queued] {
            state
                .db
                .update_job_metadata(job.id, Some(20), "aabbcc", "album")
                .await
                .unwrap();
        }
        state
            .db
            .set_job_state(active.id, DownloadState::Active, None)
            .await
            .unwrap();
        let empty_snapshot = super::DownloadClientDelta {
            full_update: true,
            inventory_changed: false,
            downloads: Vec::new(),
            removed: Vec::new(),
        };
        super::apply_download_delta(&state, "music", chrono::Utc::now(), &empty_snapshot)
            .await
            .unwrap();
        assert_eq!(
            state.db.get_job(active.id).await.unwrap().unwrap().state,
            DownloadState::Unknown
        );
        assert_eq!(
            state.db.get_job(queued.id).await.unwrap().unwrap().state,
            DownloadState::Queued
        );
        assert!(
            state
                .db
                .library_index_status()
                .await
                .unwrap()
                .last_successful_scan_at
                .is_some()
        );
    }

    #[test]
    fn provider_fingerprint_ignores_poll_telemetry_but_tracks_material_state() {
        let original = provider();
        let mut telemetry = original.clone();
        telemetry.last_request_at = Utc.timestamp_opt(1_700_000_000, 0).single();
        telemetry.last_success_at = Utc.timestamp_opt(1_700_000_001, 0).single();
        telemetry.last_background_request_at = Utc.timestamp_opt(1_700_000_002, 0).single();
        telemetry.active_requests = 1;
        assert_eq!(
            provider_change_fingerprint(vec![original.clone()]),
            provider_change_fingerprint(vec![telemetry])
        );

        let mut unavailable = original;
        unavailable.state = ProviderCircuitState::Cooldown;
        assert_ne!(
            provider_change_fingerprint(vec![provider()]),
            provider_change_fingerprint(vec![unavailable])
        );
    }
}
