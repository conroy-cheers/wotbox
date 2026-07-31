use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    sync::{Mutex, Notify, watch},
    task::JoinHandle,
    time::Instant,
};
use uuid::Uuid;

use crate::{
    api::{AppState, enrich_library_artist_credits},
    db::{EnqueueBackgroundJob, StoredBackgroundJob},
    dedupe::{compute_raw_coverage, track_index_from_group},
    model::CanonicalTorrent,
    provider::{ProviderRequestError, RequestClass},
};

pub const RESOLVE_DOWNLOAD_HASH: &str = "resolve_download_hash";
pub const INDEX_TRACKLIST: &str = "index_tracklist";
pub const COMPUTE_SINGLE_COVERAGE: &str = "compute_single_coverage";
pub const SCAN_DOWNLOAD_CLIENT: &str = "scan_download_client";
pub const CANONICAL_BACKFILL: &str = "canonical_backfill";
pub const ENRICH_LIBRARY_ARTISTS: &str = "enrich_library_artists";

const WORKER_COUNT: usize = 2;
const LEASE_DURATION: Duration = Duration::from_secs(120);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct BackgroundJobNotifier {
    notify: Arc<Notify>,
}

impl BackgroundJobNotifier {
    pub fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn wake(&self) {
        self.notify.notify_waiters();
    }
}

pub struct BackgroundRuntime {
    stop: watch::Sender<bool>,
    handles: Vec<JoinHandle<()>>,
    notifier: BackgroundJobNotifier,
    db: crate::db::Database,
    owners: Vec<String>,
}

impl BackgroundRuntime {
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        self.notifier.wake();
        let deadline = Instant::now() + Duration::from_secs(30);
        for mut handle in self.handles {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || tokio::time::timeout(remaining, &mut handle).await.is_err() {
                handle.abort();
            }
        }
        for owner in self.owners {
            if let Err(error) = self.db.release_background_job_lease(&owner).await {
                tracing::warn!(%owner, %error, "could not release background job lease");
            }
        }
    }
}

enum JobOutcome {
    Complete,
    Retry {
        delay: Duration,
        increment_attempt: bool,
        code: &'static str,
        message: String,
    },
}

pub async fn spawn_background_workers(state: Arc<AppState>) -> Result<BackgroundRuntime> {
    state.db.recover_expired_background_jobs().await?;
    bootstrap_jobs(&state).await?;
    let (stop, stop_receiver) = watch::channel(false);
    let claim_lock = Arc::new(Mutex::new(()));
    let mut handles = Vec::new();
    let mut owners = Vec::new();
    for index in 0..WORKER_COUNT {
        let worker_state = state.clone();
        let worker_stop = stop_receiver.clone();
        let worker_claim_lock = claim_lock.clone();
        let owner = format!("{}:{index}", std::process::id());
        owners.push(owner.clone());
        handles.push(tokio::spawn(worker_loop(
            worker_state,
            owner,
            worker_claim_lock,
            worker_stop,
        )));
    }
    state.background_jobs.wake();
    Ok(BackgroundRuntime {
        stop,
        handles,
        notifier: state.background_jobs.clone(),
        db: state.db.clone(),
        owners,
    })
}

pub async fn enqueue(state: &AppState, request: EnqueueBackgroundJob<'_>) -> Result<Uuid> {
    let id = state.db.enqueue_background_job(request).await?;
    state.background_jobs.wake();
    Ok(id)
}

pub async fn enqueue_hash_resolution(
    state: &AppState,
    tracker: &str,
    info_hash: &str,
) -> Result<Uuid> {
    enqueue(
        state,
        EnqueueBackgroundJob {
            deduplication_key: &format!(
                "resolve-hash:{}:{}",
                tracker.to_ascii_lowercase(),
                info_hash.to_ascii_lowercase()
            ),
            kind: RESOLVE_DOWNLOAD_HASH,
            payload: json!({ "tracker": tracker, "infoHash": info_hash.to_ascii_lowercase() }),
            priority: 20,
            max_attempts: 30,
            next_run_at: None,
            parent_id: None,
            recurring_interval_seconds: None,
        },
    )
    .await
}

pub async fn enqueue_track_index(
    state: &AppState,
    tracker: &str,
    group_id: i64,
    priority: i64,
) -> Result<Uuid> {
    enqueue(
        state,
        EnqueueBackgroundJob {
            deduplication_key: &format!(
                "track-index:{}:{}:v1",
                tracker.to_ascii_lowercase(),
                group_id
            ),
            kind: INDEX_TRACKLIST,
            payload: json!({ "tracker": tracker, "groupId": group_id }),
            priority,
            max_attempts: 12,
            next_run_at: None,
            parent_id: None,
            recurring_interval_seconds: None,
        },
    )
    .await
}

pub async fn enqueue_single_coverage(
    state: &AppState,
    tracker: &str,
    group_id: i64,
    parent_id: Option<Uuid>,
) -> Result<Uuid> {
    enqueue(
        state,
        EnqueueBackgroundJob {
            deduplication_key: &format!(
                "single-coverage:{}:{}:v1",
                tracker.to_ascii_lowercase(),
                group_id
            ),
            kind: COMPUTE_SINGLE_COVERAGE,
            payload: json!({ "tracker": tracker, "groupId": group_id }),
            priority: 5,
            max_attempts: 20,
            next_run_at: None,
            parent_id,
            recurring_interval_seconds: None,
        },
    )
    .await
}

async fn bootstrap_jobs(state: &Arc<AppState>) -> Result<()> {
    state
        .db
        .prune_background_jobs(ChronoDuration::days(30))
        .await?;
    for name in state.download_clients.keys() {
        enqueue(
            state,
            EnqueueBackgroundJob {
                deduplication_key: &format!("scan-download-client:{name}"),
                kind: SCAN_DOWNLOAD_CLIENT,
                payload: json!({ "client": name }),
                priority: -10,
                max_attempts: 20,
                next_run_at: None,
                parent_id: None,
                recurring_interval_seconds: Some(300),
            },
        )
        .await?;
    }
    enqueue(
        state,
        EnqueueBackgroundJob {
            deduplication_key: "canonical-backfill:v1",
            kind: CANONICAL_BACKFILL,
            payload: json!({}),
            priority: -20,
            max_attempts: 20,
            next_run_at: None,
            parent_id: None,
            recurring_interval_seconds: None,
        },
    )
    .await?;
    enqueue(
        state,
        EnqueueBackgroundJob {
            deduplication_key: "enrich-library-artists:v1",
            kind: ENRICH_LIBRARY_ARTISTS,
            payload: json!({}),
            priority: -15,
            max_attempts: 20,
            next_run_at: None,
            parent_id: None,
            recurring_interval_seconds: Some(60),
        },
    )
    .await?;
    for link in state.db.due_links(100_000).await? {
        if let Some(tracker) = link.tracker {
            enqueue_hash_resolution(state, &tracker, &link.info_hash).await?;
        }
    }
    for job in state.db.due_track_indexes(100_000).await? {
        enqueue_track_index(state, &job.tracker, job.group_id, 10).await?;
    }
    for (tracker, group_id) in state.db.pending_single_coverages().await? {
        enqueue_single_coverage(state, &tracker, group_id, None).await?;
    }
    Ok(())
}

async fn worker_loop(
    state: Arc<AppState>,
    owner: String,
    claim_lock: Arc<Mutex<()>>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        if *stop.borrow() {
            return;
        }
        let job = {
            let _claim = claim_lock.lock().await;
            if let Err(error) = state.db.recover_expired_background_jobs().await {
                tracing::warn!(%error, "background worker could not recover expired leases");
            }
            match state.db.claim_background_job(&owner, LEASE_DURATION).await {
                Ok(job) => job,
                Err(error) => {
                    tracing::error!(%error, "background worker could not claim a job");
                    None
                }
            }
        };
        let Some(job) = job else {
            tokio::select! {
                _ = state.background_jobs.notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return;
                    }
                }
            }
            continue;
        };
        run_claimed_job(&state, &owner, job, &mut stop).await;
    }
}

async fn run_claimed_job(
    state: &Arc<AppState>,
    owner: &str,
    job: StoredBackgroundJob,
    stop: &mut watch::Receiver<bool>,
) {
    let mut operation = Box::pin(execute_job(state, &job));
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;
    let mut stopping = false;
    let outcome = loop {
        tokio::select! {
            outcome = &mut operation => break outcome,
            _ = heartbeat.tick() => {
                match state.db.heartbeat_background_job(
                    job.id,
                    owner,
                    LEASE_DURATION,
                    0,
                    None,
                    Some("Working"),
                ).await {
                    Ok(true) => {}
                    Ok(false) => return,
                    Err(error) => tracing::warn!(job_id = %job.id, %error, "job heartbeat failed"),
                }
            }
            changed = stop.changed(), if !stopping => {
                if changed.is_err() || *stop.borrow() {
                    // Stop claiming new work, but let this leased operation finish.
                    stopping = true;
                }
            }
        }
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => classify_error(state, &job, error).await,
    };
    let result = match outcome {
        JobOutcome::Complete => state.db.complete_background_job(job.id, owner).await,
        JobOutcome::Retry {
            delay,
            increment_attempt,
            code,
            message,
        } => {
            state
                .db
                .retry_background_job(job.id, owner, delay, increment_attempt, code, &message)
                .await
        }
    };
    if let Err(error) = result {
        tracing::error!(job_id = %job.id, %error, "could not persist background job outcome");
    }
}

async fn execute_job(state: &Arc<AppState>, job: &StoredBackgroundJob) -> Result<JobOutcome> {
    match job.kind.as_str() {
        RESOLVE_DOWNLOAD_HASH => resolve_download_hash(state, &job.payload).await,
        INDEX_TRACKLIST => index_tracklist(state, job.id, &job.payload).await,
        COMPUTE_SINGLE_COVERAGE => compute_single_coverage(state, &job.payload).await,
        SCAN_DOWNLOAD_CLIENT => scan_download_client(state, &job.payload).await,
        CANONICAL_BACKFILL => canonical_backfill(state).await,
        ENRICH_LIBRARY_ARTISTS => {
            enrich_library_artist_credits(state).await?;
            Ok(JobOutcome::Complete)
        }
        kind => Err(anyhow!("unknown background job kind {kind}")),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackerHashPayload {
    tracker: String,
    info_hash: String,
}

async fn resolve_download_hash(state: &Arc<AppState>, payload: &Value) -> Result<JobOutcome> {
    let payload: TrackerHashPayload = serde_json::from_value(payload.clone())?;
    let links = state
        .db
        .links_for_tracker_hash(&payload.tracker, &payload.info_hash)
        .await?;
    if links.is_empty() {
        return Ok(JobOutcome::Complete);
    }
    let tracker = state
        .trackers
        .get(&payload.tracker)
        .with_context(|| format!("tracker {} is not configured", payload.tracker))?;
    for link in &links {
        state
            .db
            .set_link_resolving(&link.client, &link.info_hash)
            .await?;
    }
    match tracker
        .torrent_by_hash_with_class(&payload.info_hash, RequestClass::Background)
        .await
    {
        Ok((canonical, _)) => {
            let now = Utc::now();
            state
                .db
                .put_canonical(&canonical, now, now + ChronoDuration::hours(24))
                .await?;
            for link in &links {
                state
                    .db
                    .set_linked(&link.client, &link.info_hash, &canonical)
                    .await?;
            }
            Ok(JobOutcome::Complete)
        }
        Err(error) => {
            let message = error.to_string();
            let normalized = message.to_ascii_lowercase();
            let not_found = normalized.contains("not found")
                || normalized.contains("does not exist")
                || normalized.contains("bad hash");
            for link in &links {
                state
                    .db
                    .set_link_failure(&link.client, &link.info_hash, not_found, &message)
                    .await?;
            }
            if not_found {
                Ok(JobOutcome::Retry {
                    delay: Duration::from_secs(3600),
                    increment_attempt: true,
                    code: "not_found",
                    message,
                })
            } else {
                Err(error)
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackerGroupPayload {
    tracker: String,
    group_id: i64,
}

async fn index_tracklist(
    state: &Arc<AppState>,
    job_id: Uuid,
    payload: &Value,
) -> Result<JobOutcome> {
    let payload: TrackerGroupPayload = serde_json::from_value(payload.clone())?;
    let tracker = state
        .trackers
        .get(&payload.tracker)
        .with_context(|| format!("tracker {} is not configured", payload.tracker))?;
    state
        .db
        .set_track_index_resolving(&payload.tracker, payload.group_id)
        .await?;
    let result: Result<()> = async {
        let (detail, raw) = tracker
            .group_with_class(payload.group_id, RequestClass::Background)
            .await?;
        let index = track_index_from_group(&payload.tracker, &detail, &raw);
        let now = Utc::now();
        for variant in &detail.variants {
            state
                .db
                .put_canonical(
                    &CanonicalTorrent {
                        release: detail.release.clone(),
                        variant: variant.clone(),
                        tags: detail.tags.clone(),
                        description: detail.description.clone(),
                        record_label: detail.record_label.clone(),
                    },
                    now,
                    now + ChronoDuration::hours(24),
                )
                .await?;
        }
        state
            .db
            .put_snapshot(
                &payload.tracker,
                "group",
                &payload.group_id.to_string(),
                &detail,
                &raw,
                now,
                now + ChronoDuration::hours(24),
            )
            .await?;
        state
            .db
            .put_track_index_with_parent(&index, Some(job_id))
            .await?;
        state.background_jobs.wake();
        if detail
            .release
            .release_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        {
            state
                .db
                .ensure_single_coverage(&payload.tracker, payload.group_id)
                .await?;
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Ok(JobOutcome::Complete),
        Err(error) => {
            state
                .db
                .fail_track_index(&payload.tracker, payload.group_id, &error.to_string())
                .await?;
            Err(error)
        }
    }
}

async fn compute_single_coverage(state: &Arc<AppState>, payload: &Value) -> Result<JobOutcome> {
    let payload: TrackerGroupPayload = serde_json::from_value(payload.clone())?;
    let memberships = state.db.list_catalog_memberships().await?;
    let indexes = state
        .db
        .list_track_indexes()
        .await?
        .into_iter()
        .map(|index| ((index.tracker.clone(), index.group_id), index))
        .collect::<HashMap<_, _>>();
    let single_artists = memberships
        .iter()
        .filter(|membership| {
            membership
                .group
                .release
                .tracker
                .eq_ignore_ascii_case(&payload.tracker)
                && membership.group.release.group_id == payload.group_id
        })
        .map(|membership| membership.artist_id)
        .collect::<HashSet<_>>();
    if single_artists.is_empty() {
        return Ok(JobOutcome::Retry {
            delay: Duration::from_secs(30),
            increment_attempt: false,
            code: "dependencies_pending",
            message: "Waiting for artist catalog membership".into(),
        });
    }
    let albums = memberships
        .iter()
        .filter(|membership| {
            membership
                .group
                .release
                .tracker
                .eq_ignore_ascii_case(&payload.tracker)
                && single_artists.contains(&membership.artist_id)
                && membership.group.listed_on_tracker
                && !membership.group.variants.is_empty()
                && membership
                    .group
                    .release
                    .release_type
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("album"))
        })
        .collect::<Vec<_>>();
    let single_key = (payload.tracker.clone(), payload.group_id);
    let Some(single) = indexes.get(&single_key) else {
        return Ok(JobOutcome::Retry {
            delay: Duration::from_secs(15),
            increment_attempt: false,
            code: "dependencies_pending",
            message: "Waiting for the Single tracklist".into(),
        });
    };
    if single.state == "failed" {
        return Ok(JobOutcome::Retry {
            delay: Duration::from_secs(60),
            increment_attempt: false,
            code: "dependencies_pending",
            message: "Waiting for a failed Single tracklist retry".into(),
        });
    }
    let Some(single_index) = single.index.as_ref() else {
        return Ok(JobOutcome::Retry {
            delay: Duration::from_secs(15),
            increment_attempt: false,
            code: "dependencies_pending",
            message: "Waiting for the Single tracklist".into(),
        });
    };
    let mut album_indexes = Vec::new();
    for membership in albums {
        let key = (
            membership.group.release.tracker.clone(),
            membership.group.release.group_id,
        );
        let Some(index) = indexes.get(&key) else {
            return Ok(JobOutcome::Retry {
                delay: Duration::from_secs(15),
                increment_attempt: false,
                code: "dependencies_pending",
                message: "Waiting for candidate Album tracklists".into(),
            });
        };
        let Some(index) = index.index.clone() else {
            return Ok(JobOutcome::Retry {
                delay: Duration::from_secs(30),
                increment_attempt: false,
                code: "dependencies_pending",
                message: "Waiting for candidate Album tracklists".into(),
            });
        };
        album_indexes.push((index, membership.group.clone()));
    }
    let coverage = compute_raw_coverage(single_index, &album_indexes);
    state
        .db
        .put_single_coverage(&payload.tracker, payload.group_id, "ready", Some(&coverage))
        .await?;
    Ok(JobOutcome::Complete)
}

#[derive(Deserialize)]
struct ClientPayload {
    client: String,
}

async fn scan_download_client(state: &Arc<AppState>, payload: &Value) -> Result<JobOutcome> {
    const PAGE_SIZE: u32 = 200;
    let payload: ClientPayload = serde_json::from_value(payload.clone())?;
    let client = state
        .download_clients
        .get(&payload.client)
        .with_context(|| format!("download client {} is not configured", payload.client))?;
    let scan_started_at = Utc::now();
    let mut offset = 0;
    loop {
        let downloads = client
            .downloads_with_class(PAGE_SIZE, offset, RequestClass::Background)
            .await?;
        let count = downloads.len();
        for download in &downloads {
            state
                .db
                .observe_download(
                    &download.live,
                    download.announce_host.as_deref(),
                    download
                        .announce_host
                        .as_ref()
                        .and_then(|host| state.announce_hosts.get(host))
                        .map(String::as_str),
                )
                .await?;
            state.background_jobs.wake();
        }
        offset += count as u32;
        if count < PAGE_SIZE as usize || offset >= 100_000 {
            break;
        }
    }
    state
        .db
        .complete_client_scan(&payload.client, scan_started_at)
        .await?;
    Ok(JobOutcome::Complete)
}

async fn canonical_backfill(state: &Arc<AppState>) -> Result<JobOutcome> {
    match state.db.backfill_canonical_identities(20).await? {
        0 => Ok(JobOutcome::Complete),
        _ => Ok(JobOutcome::Retry {
            delay: Duration::from_millis(500),
            increment_attempt: false,
            code: "more_work",
            message: "Continuing canonical identity backfill".into(),
        }),
    }
}

async fn classify_error(
    state: &AppState,
    job: &StoredBackgroundJob,
    error: anyhow::Error,
) -> JobOutcome {
    let provider = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ProviderRequestError>())
        .map(|provider| match provider {
            ProviderRequestError::Unavailable {
                provider, retry_at, ..
            } => (Some(provider.clone()), *retry_at, provider.to_string()),
            ProviderRequestError::Busy { provider }
            | ProviderRequestError::Upstream { provider, .. } => {
                (Some(provider.clone()), None, provider.to_string())
            }
            _ => (None, None, provider.to_string()),
        });
    if let Some((provider_id, mut retry_at, provider_message)) = provider {
        if retry_at.is_none()
            && let Some(id) = provider_id.as_deref()
        {
            retry_at = state
                .providers
                .statuses()
                .await
                .into_iter()
                .find(|status| status.id == id)
                .and_then(|status| status.retry_at);
        }
        let delay = retry_at
            .and_then(|value| (value - Utc::now()).to_std().ok())
            .unwrap_or(Duration::from_secs(30));
        return JobOutcome::Retry {
            delay,
            increment_attempt: false,
            code: "provider_wait",
            message: provider_id
                .map(|id| {
                    if provider_message == id {
                        format!("Waiting for {id}")
                    } else {
                        format!("Waiting for {id}: {provider_message}")
                    }
                })
                .unwrap_or(provider_message),
        };
    }
    let exponent = job.attempts.min(7);
    let base = 30_u64.saturating_mul(1_u64 << exponent).min(3600);
    let jitter = (u128::from_le_bytes(*job.id.as_bytes()) % 11) as u64;
    JobOutcome::Retry {
        delay: Duration::from_secs(base + jitter),
        increment_attempt: true,
        code: "job_failed",
        message: error.to_string(),
    }
}
