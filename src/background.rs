use std::{
    collections::{HashMap, HashSet},
    path::Path,
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
    api::{AppState, cleanup_download_stage, enrich_library_artist_credits, process_download},
    db::{DownloadObservation, EnqueueBackgroundJob, StoredBackgroundJob},
    dedupe::{compute_raw_coverage, track_index_from_group},
    model::{
        ArtistCatalogRole, CanonicalTorrent, DownloadState, ImportCleanupMode, ImportTaskState,
    },
    plex::PlexScanTarget,
    provider::{ProviderFailureKind, ProviderRequestError, RequestClass},
};

pub const RESOLVE_DOWNLOAD_HASH: &str = "resolve_download_hash";
pub const INDEX_TRACKLIST: &str = "index_tracklist";
pub const COMPUTE_SINGLE_COVERAGE: &str = "compute_single_coverage";
pub const SCAN_DOWNLOAD_CLIENT: &str = "scan_download_client";
pub const CANONICAL_BACKFILL: &str = "canonical_backfill";
pub const ENRICH_LIBRARY_ARTISTS: &str = "enrich_library_artists";
pub const NOTIFY_PLEX: &str = "notify_plex";
pub const SUBMIT_DOWNLOAD: &str = "submit_download";
pub const PROCESS_IMPORT: &str = "process_import";

const WORKER_LANES: [&str; 6] = [
    "event",
    "sync",
    "sync",
    "maintenance",
    "download",
    "download",
];
const LEASE_DURATION: Duration = Duration::from_secs(120);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(1);

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

struct AbortOnDrop<T> {
    handle: JoinHandle<T>,
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.handle.abort();
    }
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
    Fail {
        code: &'static str,
        message: String,
    },
    Retry {
        delay: Duration,
        increment_attempt: bool,
        code: &'static str,
        message: String,
    },
    Wait {
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
    for (index, lane) in WORKER_LANES.into_iter().enumerate() {
        let worker_state = state.clone();
        let worker_stop = stop_receiver.clone();
        let worker_claim_lock = claim_lock.clone();
        let owner = format!("{}:{index}", std::process::id());
        owners.push(owner.clone());
        handles.push(tokio::spawn(worker_loop(
            worker_state,
            owner,
            lane.to_owned(),
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
    enqueue_hash_resolution_at(state, tracker, info_hash, None).await
}

pub async fn enqueue_import_processing(state: &AppState, import_id: Uuid) -> Result<Uuid> {
    let key = format!("process-import:{import_id}:v1");
    if state.db.retry_background_job_by_key(&key).await? {
        state.background_jobs.wake();
        return state
            .db
            .background_job_id_by_key(&key)
            .await?
            .context("reactivated import processor disappeared");
    }
    enqueue(
        state,
        EnqueueBackgroundJob {
            deduplication_key: &key,
            kind: PROCESS_IMPORT,
            payload: json!({ "importId": import_id }),
            provider_id: None,
            lane: "maintenance",
            priority: 40,
            max_attempts: 20,
            next_run_at: None,
            parent_id: None,
            recurring_interval_seconds: None,
        },
    )
    .await
}

pub async fn retry_hash_resolution(
    state: &AppState,
    tracker: &str,
    info_hash: &str,
) -> Result<Uuid> {
    let key = format!(
        "resolve-hash:{}:{}:v3",
        tracker.to_ascii_lowercase(),
        info_hash.to_ascii_lowercase()
    );
    if state.db.retry_background_job_by_key(&key).await? {
        state.background_jobs.wake();
        return state
            .db
            .background_job_id_by_key(&key)
            .await?
            .context("retried background job disappeared");
    }
    enqueue_hash_resolution(state, tracker, info_hash).await
}

async fn enqueue_hash_resolution_at(
    state: &AppState,
    tracker: &str,
    info_hash: &str,
    next_run_at: Option<chrono::DateTime<Utc>>,
) -> Result<Uuid> {
    enqueue(
        state,
        EnqueueBackgroundJob {
            deduplication_key: &format!(
                "resolve-hash:{}:{}:v3",
                tracker.to_ascii_lowercase(),
                info_hash.to_ascii_lowercase()
            ),
            kind: RESOLVE_DOWNLOAD_HASH,
            payload: json!({ "tracker": tracker, "infoHash": info_hash.to_ascii_lowercase() }),
            provider_id: Some(format!("tracker:{tracker}")),
            lane: "sync",
            priority: 20,
            max_attempts: 8,
            next_run_at,
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
                "track-index:{}:{}:v2",
                tracker.to_ascii_lowercase(),
                group_id
            ),
            kind: INDEX_TRACKLIST,
            payload: json!({ "tracker": tracker, "groupId": group_id }),
            provider_id: Some(format!("tracker:{tracker}")),
            lane: "sync",
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
                "single-coverage:{}:{}:v2",
                tracker.to_ascii_lowercase(),
                group_id
            ),
            kind: COMPUTE_SINGLE_COVERAGE,
            payload: json!({ "tracker": tracker, "groupId": group_id }),
            provider_id: None,
            lane: "sync",
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
    state.db.ensure_incomplete_download_submissions().await?;
    for name in state.download_clients.keys() {
        enqueue(
            state,
            EnqueueBackgroundJob {
                deduplication_key: &format!("scan-download-client:{name}"),
                kind: SCAN_DOWNLOAD_CLIENT,
                payload: json!({ "client": name }),
                provider_id: Some(format!("qbittorrent:{name}")),
                lane: "maintenance",
                priority: 50,
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
            provider_id: None,
            lane: "maintenance",
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
            provider_id: None,
            lane: "maintenance",
            priority: -15,
            max_attempts: 20,
            next_run_at: None,
            parent_id: None,
            recurring_interval_seconds: Some(60),
        },
    )
    .await?;
    let mut ops_offset = 0_i64;
    for link in state.db.due_links(100_000).await? {
        if let Some(tracker) = link.tracker {
            let next_run_at = tracker.eq_ignore_ascii_case("ops").then(|| {
                let value = Utc::now() + ChronoDuration::seconds(ops_offset * 5);
                ops_offset += 1;
                value
            });
            enqueue_hash_resolution_at(state, &tracker, &link.info_hash, next_run_at).await?;
        }
    }
    for job in state.db.due_track_indexes(100_000).await? {
        enqueue_track_index(state, &job.tracker, job.group_id, 10).await?;
    }
    state
        .db
        .reconcile_waiting_single_coverage_track_indexes()
        .await?;
    for (tracker, group_id) in state.db.pending_single_coverages().await? {
        enqueue_single_coverage(state, &tracker, group_id, None).await?;
    }
    state.db.reconcile_waiting_single_coverages().await?;
    Ok(())
}

async fn worker_loop(
    state: Arc<AppState>,
    owner: String,
    lane: String,
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
            match state
                .db
                .claim_background_job(&owner, &lane, LEASE_DURATION)
                .await
            {
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
    tracing::debug!(
        job_id = %job.id,
        kind = %job.kind,
        lane = %job.lane,
        provider = job.provider_id.as_deref().unwrap_or("local"),
        "running background job"
    );
    let operation_state = state.clone();
    let operation_job = job.clone();
    let mut operation = AbortOnDrop {
        handle: tokio::spawn(async move { execute_job(&operation_state, &operation_job).await }),
    };
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;
    let mut stopping = false;
    let outcome = loop {
        tokio::select! {
            outcome = &mut operation.handle => {
                break outcome.unwrap_or_else(|error| Err(anyhow!("job operation task failed: {error}")));
            }
            _ = heartbeat.tick() => {
                match tokio::time::timeout(
                    HEARTBEAT_TIMEOUT,
                    state.db.heartbeat_background_job(
                        job.id,
                        owner,
                        LEASE_DURATION,
                        0,
                        None,
                        Some("Working"),
                    ),
                ).await {
                    Ok(Ok(true)) => {}
                    Ok(Ok(false)) => {
                        operation.handle.abort();
                        let _ = (&mut operation.handle).await;
                        return;
                    }
                    Ok(Err(error)) => tracing::warn!(job_id = %job.id, %error, "job heartbeat failed"),
                    Err(_) => tracing::warn!(job_id = %job.id, "job heartbeat timed out"),
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
    let terminal_download_failure = match &outcome {
        JobOutcome::Fail { code, message } => Some((*code, message.as_str())),
        JobOutcome::Retry {
            increment_attempt: true,
            code,
            message,
            ..
        } if job.attempts.saturating_add(1) >= job.max_attempts => Some((*code, message.as_str())),
        _ => None,
    };
    if job.kind == SUBMIT_DOWNLOAD
        && let Some((code, message)) = terminal_download_failure
        && let Ok(payload) =
            serde_json::from_value::<DownloadSubmissionPayload>(job.payload.clone())
    {
        if let Err(error) = state
            .db
            .set_job_state(
                payload.job_id,
                crate::model::DownloadState::Failed,
                Some((code, message)),
            )
            .await
        {
            tracing::error!(job_id = %payload.job_id, %error, "could not persist terminal download failure");
        }
        cleanup_download_stage(state, payload.job_id).await;
    }
    let result = match outcome {
        JobOutcome::Complete => state.db.complete_background_job(job.id, owner).await,
        JobOutcome::Fail { code, message } => {
            state
                .db
                .fail_background_job(job.id, owner, code, &message)
                .await
        }
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
        JobOutcome::Wait { code, message } => {
            state
                .db
                .wait_background_job(job.id, owner, code, &message)
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
        NOTIFY_PLEX => notify_plex(state, &job.payload).await,
        SUBMIT_DOWNLOAD => submit_download(state, &job.payload).await,
        PROCESS_IMPORT => process_import(state, &job.payload).await,
        kind => Err(anyhow!("unknown background job kind {kind}")),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadSubmissionPayload {
    job_id: Uuid,
}

async fn submit_download(state: &Arc<AppState>, payload: &Value) -> Result<JobOutcome> {
    let payload: DownloadSubmissionPayload = serde_json::from_value(payload.clone())?;
    let Some(job) = state.db.get_job(payload.job_id).await? else {
        return Ok(JobOutcome::Fail {
            code: "download_job_missing",
            message: "The durable download job no longer exists".into(),
        });
    };
    if matches!(
        job.state,
        crate::model::DownloadState::Active
            | crate::model::DownloadState::Complete
            | crate::model::DownloadState::Unknown
            | crate::model::DownloadState::Failed
    ) {
        return Ok(JobOutcome::Complete);
    }
    process_download(state.clone(), job).await?;
    Ok(JobOutcome::Complete)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportProcessingPayload {
    import_id: Uuid,
}

async fn process_import(state: &Arc<AppState>, payload: &Value) -> Result<JobOutcome> {
    let payload: ImportProcessingPayload = serde_json::from_value(payload.clone())?;
    let Some((task, supersessions)) = state.db.import_task_models(payload.import_id).await? else {
        return Ok(JobOutcome::Fail {
            code: "import_missing",
            message: "The durable import task no longer exists".into(),
        });
    };
    if matches!(task.state.as_str(), "complete" | "dismissed") {
        return Ok(JobOutcome::Complete);
    }

    let mut target_client = task.client.clone();
    let mut target_hash = task.info_hash.clone();
    if (target_client.is_none() || target_hash.is_none())
        && let Some(job_id) = task
            .download_job_id
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()?
        && let Some(job) = state.db.get_job(job_id).await?
    {
        if job.state == DownloadState::Failed {
            state
                .db
                .set_import_state(
                    payload.import_id,
                    ImportTaskState::Failed,
                    Some("The replacement download failed"),
                    job.error_message.as_deref(),
                )
                .await?;
            state
                .db
                .set_superseded_source_states(
                    payload.import_id,
                    ImportTaskState::NeedsReview,
                    "The replacement download failed",
                )
                .await?;
            return Ok(JobOutcome::Fail {
                code: "replacement_failed",
                message: job
                    .error_message
                    .unwrap_or_else(|| "The replacement download failed".into()),
            });
        }
        target_client = state
            .profiles
            .get(&job.profile)
            .map(|profile| profile.client.clone());
        target_hash = job.info_hash;
    }
    let (Some(target_client), Some(target_hash)) = (target_client, target_hash) else {
        state
            .db
            .set_import_state(
                payload.import_id,
                ImportTaskState::Downloading,
                Some("Waiting for the replacement torrent to be submitted"),
                None,
            )
            .await?;
        return Ok(JobOutcome::Retry {
            delay: Duration::from_secs(15),
            increment_attempt: false,
            code: "replacement_pending",
            message: "Waiting for the replacement torrent to be submitted".into(),
        });
    };
    state
        .db
        .bind_import_target(payload.import_id, &target_client, &target_hash)
        .await?;
    let Some(target_qbit) = state.download_clients.get(&target_client) else {
        return block_import(
            state,
            payload.import_id,
            "The replacement download client is no longer configured",
        )
        .await;
    };
    let Some(target) = target_qbit
        .download_with_class(&target_hash, RequestClass::Background)
        .await?
    else {
        return Ok(JobOutcome::Retry {
            delay: Duration::from_secs(30),
            increment_attempt: false,
            code: "replacement_not_visible",
            message: "Waiting for the replacement torrent to appear in qBittorrent".into(),
        });
    };
    if target.live.progress < 1.0 {
        state
            .db
            .set_import_state(
                payload.import_id,
                ImportTaskState::Downloading,
                Some("Waiting for the replacement download to complete"),
                None,
            )
            .await?;
        return Ok(JobOutcome::Retry {
            delay: Duration::from_secs(30),
            increment_attempt: false,
            code: "replacement_downloading",
            message: format!(
                "Replacement is {}% complete",
                (target.live.progress * 100.0).round()
            ),
        });
    }
    if supersessions
        .iter()
        .all(|source| source.cleanup_mode == ImportCleanupMode::Keep.as_str())
    {
        for source in &supersessions {
            state
                .db
                .set_supersession_state(
                    payload.import_id,
                    &source.source_client,
                    &source.source_info_hash,
                    "retained",
                    Some("Retained by the import cleanup preference"),
                )
                .await?;
        }
        state
            .db
            .set_import_state(
                payload.import_id,
                ImportTaskState::Complete,
                Some("Replacement import completed; old torrents were retained"),
                None,
            )
            .await?;
        state
            .db
            .set_superseded_source_states(
                payload.import_id,
                ImportTaskState::Complete,
                "Replacement completed and the old torrent was retained by policy",
            )
            .await?;
        return Ok(JobOutcome::Complete);
    }
    let Some(plex) = state.plex.as_ref() else {
        return block_import(
            state,
            payload.import_id,
            "Cleanup is blocked because Plex integration is not configured",
        )
        .await;
    };
    let Some(target_path) = target.live.content_path.as_deref() else {
        return block_import(
            state,
            payload.import_id,
            "Cleanup is blocked because qBittorrent did not report the replacement content path",
        )
        .await;
    };
    let Some(target_scan) = plex.target_for_path(target_path) else {
        return block_import(
            state,
            payload.import_id,
            "Cleanup is blocked because the replacement is outside a configured Plex music root",
        )
        .await;
    };

    state
        .db
        .set_import_state(
            payload.import_id,
            ImportTaskState::Processing,
            Some("Verifying the superseded torrents before cleanup"),
            None,
        )
        .await?;

    struct CleanupPlan {
        client: Arc<dyn crate::qbittorrent::DownloadClient>,
        client_name: String,
        info_hash: String,
        delete_files: bool,
        scan_target: PlexScanTarget,
    }
    let mut cleanup = Vec::new();
    for source in supersessions {
        if source.cleanup_state == "removed" || source.cleanup_state == "retained" {
            continue;
        }
        let cleanup_mode = match source.cleanup_mode.as_str() {
            "keep" => ImportCleanupMode::Keep,
            "remove_torrent" => ImportCleanupMode::RemoveTorrent,
            "delete_files" => ImportCleanupMode::DeleteFiles,
            _ => {
                return block_import(
                    state,
                    payload.import_id,
                    "Cleanup is blocked by an unknown cleanup policy",
                )
                .await;
            }
        };
        if cleanup_mode == ImportCleanupMode::Keep {
            state
                .db
                .set_supersession_state(
                    payload.import_id,
                    &source.source_client,
                    &source.source_info_hash,
                    "retained",
                    Some("Retained by the import cleanup preference"),
                )
                .await?;
            continue;
        }
        if !task
            .tracker
            .as_deref()
            .is_some_and(|tracker| tracker.eq_ignore_ascii_case(&source.tracker))
        {
            return block_import(
                state,
                payload.import_id,
                "Cleanup is blocked because the old and replacement torrents are not from the same tracker",
            )
            .await;
        }
        let Some(client) = state.download_clients.get(&source.source_client).cloned() else {
            return block_import(
                state,
                payload.import_id,
                "Cleanup is blocked because an old torrent's client is not configured",
            )
            .await;
        };
        let Some(old) = client
            .download_with_class(&source.source_info_hash, RequestClass::Background)
            .await?
        else {
            state
                .db
                .set_supersession_state(
                    payload.import_id,
                    &source.source_client,
                    &source.source_info_hash,
                    "removed",
                    Some("The old torrent was already absent from qBittorrent"),
                )
                .await?;
            continue;
        };
        let statuses = client
            .tracker_statuses_with_class(&source.source_info_hash, RequestClass::Background)
            .await?;
        if !statuses.iter().any(|status| {
            status
                .message
                .as_deref()
                .is_some_and(tracker_message_reports_unregistered)
                && status
                    .announce_host
                    .as_deref()
                    .and_then(|host| state.announce_hosts.get(host))
                    .is_some_and(|tracker| tracker.eq_ignore_ascii_case(&source.tracker))
        }) {
            return block_import(
                state,
                payload.import_id,
                "Cleanup is blocked because the tracker no longer explicitly reports an old torrent as unregistered",
            )
            .await;
        }
        let Some(old_path) = old.live.content_path.as_deref() else {
            return block_import(
                state,
                payload.import_id,
                "Cleanup is blocked because qBittorrent did not report an old torrent's content path",
            )
            .await;
        };
        let Some(old_scan) = plex.target_for_path(old_path) else {
            return block_import(
                state,
                payload.import_id,
                "Cleanup is blocked because an old torrent is outside a configured Plex music root",
            )
            .await;
        };
        if paths_overlap(old_path, target_path) {
            return block_import(
                state,
                payload.import_id,
                "Cleanup is blocked because the old and replacement content paths overlap",
            )
            .await;
        }
        for (other_client_name, other_client) in &state.download_clients {
            let active = other_client
                .downloads_with_class(100_000, 0, RequestClass::Background)
                .await?;
            if active.iter().any(|other| {
                !(other_client_name == &source.source_client
                    && other
                        .live
                        .info_hash
                        .eq_ignore_ascii_case(&source.source_info_hash))
                    && other
                        .live
                        .content_path
                        .as_deref()
                        .is_some_and(|path| paths_overlap(path, old_path))
            }) {
                return block_import(
                    state,
                    payload.import_id,
                    "Cleanup is blocked because another active torrent shares the old content path",
                )
                .await;
            }
        }
        cleanup.push(CleanupPlan {
            client,
            client_name: source.source_client,
            info_hash: source.source_info_hash,
            delete_files: cleanup_mode == ImportCleanupMode::DeleteFiles,
            scan_target: old_scan,
        });
    }

    // The replacement must be visible to Plex before any old payload can be removed.
    plex.scan(&state.source_client, &state.providers, &target_scan)
        .await?;
    for plan in cleanup {
        plan.client
            .delete_torrent(&plan.info_hash, plan.delete_files)
            .await?;
        state
            .db
            .set_supersession_state(
                payload.import_id,
                &plan.client_name,
                &plan.info_hash,
                "removed",
                Some(if plan.delete_files {
                    "Removed the trumped torrent and its payload after guarded verification"
                } else {
                    "Removed the trumped torrent and retained its payload"
                }),
            )
            .await?;
        plex.scan(&state.source_client, &state.providers, &plan.scan_target)
            .await?;
    }
    state
        .db
        .set_import_state(
            payload.import_id,
            ImportTaskState::Complete,
            Some("Replacement import and supersession cleanup completed"),
            None,
        )
        .await?;
    state
        .db
        .set_superseded_source_states(
            payload.import_id,
            ImportTaskState::Complete,
            "Supersession workflow completed",
        )
        .await?;
    Ok(JobOutcome::Complete)
}

async fn block_import(state: &AppState, import_id: Uuid, reason: &str) -> Result<JobOutcome> {
    state
        .db
        .set_import_state(import_id, ImportTaskState::Blocked, Some(reason), None)
        .await?;
    state
        .db
        .set_superseded_source_states(import_id, ImportTaskState::NeedsReview, reason)
        .await?;
    Ok(JobOutcome::Complete)
}

fn paths_overlap(left: &str, right: &str) -> bool {
    let left = Path::new(left);
    let right = Path::new(right);
    left.starts_with(right) || right.starts_with(left)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlexScanPayload {
    section_id: u32,
    root: String,
}

async fn notify_plex(state: &Arc<AppState>, payload: &Value) -> Result<JobOutcome> {
    let payload: PlexScanPayload = serde_json::from_value(payload.clone())?;
    let target = PlexScanTarget {
        section_id: payload.section_id,
        root: payload.root,
    };
    let Some(plex) = state.plex.as_ref() else {
        return Ok(JobOutcome::Fail {
            code: "plex_unconfigured",
            message: "Plex is not configured".into(),
        });
    };
    if !plex.allows(&target) {
        return Ok(JobOutcome::Fail {
            code: "plex_target_unconfigured",
            message: "Plex scan target is no longer configured".into(),
        });
    }
    plex.scan(&state.source_client, &state.providers, &target)
        .await?;
    Ok(JobOutcome::Complete)
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
            let provider_message = error.to_string();
            let normalized = provider_message.to_ascii_lowercase();
            let not_found = normalized.contains("not found")
                || normalized.contains("does not exist")
                || normalized.contains("bad hash")
                || (payload.tracker.eq_ignore_ascii_case("ops")
                    && normalized.contains("bad parameters"));
            let provider_wait = provider_error_is_admission_or_blocked(&error);
            let diagnosis = if not_found {
                diagnose_missing_torrent(state, &payload, &links).await
            } else {
                MissingTorrentDiagnosis {
                    code: "tracker_error",
                    message: provider_message.clone(),
                }
            };
            for link in &links {
                if provider_wait {
                    state
                        .db
                        .defer_link_resolution(&link.client, &link.info_hash, &provider_message)
                        .await?;
                } else {
                    state
                        .db
                        .set_link_failure(
                            &link.client,
                            &link.info_hash,
                            not_found,
                            diagnosis.code,
                            &diagnosis.message,
                        )
                        .await?;
                }
            }
            if not_found {
                Ok(JobOutcome::Fail {
                    code: diagnosis.code,
                    message: diagnosis.message,
                })
            } else {
                Err(error)
            }
        }
    }
}

struct MissingTorrentDiagnosis {
    code: &'static str,
    message: String,
}

async fn diagnose_missing_torrent(
    state: &AppState,
    payload: &TrackerHashPayload,
    links: &[crate::db::DownloadReleaseLink],
) -> MissingTorrentDiagnosis {
    if !payload.tracker.eq_ignore_ascii_case("ops") {
        return MissingTorrentDiagnosis {
            code: "not_found",
            message: format!(
                "{} did not recognize this torrent hash; manual retry is available",
                payload.tracker
            ),
        };
    }

    let mut queried_clients = HashSet::new();
    for link in links {
        if !queried_clients.insert(link.client.as_str()) {
            continue;
        }
        let Some(client) = state.download_clients.get(&link.client) else {
            continue;
        };
        let expected_hosts = links
            .iter()
            .filter(|candidate| candidate.client == link.client)
            .filter_map(|candidate| candidate.announce_host.as_deref())
            .collect::<HashSet<_>>();
        match client
            .tracker_statuses_with_class(&payload.info_hash, RequestClass::Background)
            .await
        {
            Ok(statuses) => {
                if statuses.iter().any(|status| {
                    status
                        .announce_host
                        .as_deref()
                        .is_some_and(|host| expected_hosts.contains(host))
                        && status
                            .message
                            .as_deref()
                            .is_some_and(tracker_message_reports_unregistered)
                }) {
                    return MissingTorrentDiagnosis {
                        code: "torrent_unregistered",
                        message: "qBittorrent confirms that OPS reports this torrent as unregistered. It was removed from the active tracker catalogue and may have been trumped; find and add its replacement before deleting the old torrent.".into(),
                    };
                }
            }
            Err(error) => {
                tracing::warn!(
                    client = %link.client,
                    info_hash = %payload.info_hash,
                    %error,
                    "could not inspect qBittorrent tracker status for missing OPS torrent"
                );
            }
        }
    }

    MissingTorrentDiagnosis {
        code: "not_found",
        message: "OPS does not currently recognize this torrent hash. It may have been removed or trumped; qBittorrent did not return an explicit unregistered-torrent response. Manual retry is available.".into(),
    }
}

fn tracker_message_reports_unregistered(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "unregistered torrent",
        "torrent not registered",
        "torrent is not registered",
        "torrent not found",
        "unknown torrent",
        "torrent does not exist",
        "torrent has been deleted",
        "torrent has been removed",
    ]
    .iter()
    .any(|needle| message.contains(needle))
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
            if provider_error_is_admission_or_blocked(&error) {
                state
                    .db
                    .defer_track_index(&payload.tracker, payload.group_id, &error.to_string())
                    .await?;
            } else {
                state
                    .db
                    .fail_track_index(&payload.tracker, payload.group_id, &error.to_string())
                    .await?;
            }
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
                && has_primary_role(&membership.group.roles)
        })
        .map(|membership| membership.artist_id)
        .collect::<HashSet<_>>();
    if single_artists.is_empty() {
        // A Single encountered outside an artist catalog has no coverage work yet.
        // Complete this no-op; a later catalog refresh reactivates the durable job.
        return Ok(JobOutcome::Complete);
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
                && has_primary_role(&membership.group.roles)
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
        return Ok(JobOutcome::Wait {
            code: "dependencies_pending",
            message: "Waiting for the Single tracklist".into(),
        });
    };
    if single.state == "failed" {
        return Ok(JobOutcome::Wait {
            code: "dependencies_pending",
            message: "Waiting for a failed Single tracklist retry".into(),
        });
    }
    let Some(single_index) = single.index.as_ref() else {
        return Ok(JobOutcome::Wait {
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
            return Ok(JobOutcome::Wait {
                code: "dependencies_pending",
                message: "Waiting for candidate Album tracklists".into(),
            });
        };
        let Some(index) = index.index.clone() else {
            return Ok(JobOutcome::Wait {
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

fn has_primary_role(roles: &[ArtistCatalogRole]) -> bool {
    roles.contains(&ArtistCatalogRole::Primary)
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
        let observations = downloads
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
        state.db.observe_downloads(&observations).await?;
        state.background_jobs.wake();
        offset += count as u32;
        if count < PAGE_SIZE as usize || offset >= 100_000 {
            break;
        }
    }
    state
        .db
        .complete_client_scan(&payload.client, scan_started_at)
        .await?;
    state.db.sync_import_tasks().await?;
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
    let provider_error = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ProviderRequestError>());
    if let Some(provider_error) = provider_error {
        if let Some(provider_id) = provider_error.provider_id()
            && let Err(error) = state
                .db
                .set_background_job_provider(job.id, provider_id)
                .await
        {
            tracing::warn!(job_id = %job.id, %error, "could not update job provider attribution");
        }
        let message = provider_error.to_string();
        match provider_error {
            ProviderRequestError::Unavailable {
                retry_at: Some(retry_at),
                ..
            }
            | ProviderRequestError::Deferred { retry_at, .. } => {
                return JobOutcome::Retry {
                    delay: (*retry_at - Utc::now())
                        .to_std()
                        .unwrap_or(Duration::from_millis(1)),
                    increment_attempt: false,
                    code: "provider_deferred",
                    message,
                };
            }
            ProviderRequestError::Unavailable { .. } => {
                return JobOutcome::Wait {
                    code: "provider_wait",
                    message,
                };
            }
            ProviderRequestError::Busy { .. } | ProviderRequestError::Stopped(_) => {
                return JobOutcome::Retry {
                    delay: Duration::from_secs(5),
                    increment_attempt: false,
                    code: "provider_admission_deferred",
                    message,
                };
            }
            ProviderRequestError::Upstream {
                failure,
                kind: ProviderFailureKind::Permanent,
                ..
            } => {
                return JobOutcome::Fail {
                    code: "permanent_provider_failure",
                    message: failure.clone(),
                };
            }
            ProviderRequestError::Upstream {
                kind: ProviderFailureKind::Authentication | ProviderFailureKind::HardBlocked,
                ..
            } => {
                return JobOutcome::Wait {
                    code: "provider_blocked",
                    message,
                };
            }
            ProviderRequestError::Upstream {
                provider,
                kind: ProviderFailureKind::RateLimited,
                ..
            } => {
                let retry_at = state
                    .providers
                    .statuses()
                    .await
                    .into_iter()
                    .find(|status| status.id == *provider)
                    .and_then(|status| status.retry_at);
                return JobOutcome::Retry {
                    delay: retry_at
                        .and_then(|value| (value - Utc::now()).to_std().ok())
                        .unwrap_or(Duration::from_secs(60)),
                    increment_attempt: false,
                    code: "provider_rate_limited",
                    message,
                };
            }
            ProviderRequestError::Upstream {
                kind: ProviderFailureKind::Transient,
                ..
            } => {}
            ProviderRequestError::Unknown(_) => {
                return JobOutcome::Fail {
                    code: "unknown_provider",
                    message,
                };
            }
        }
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

fn provider_error_is_admission_or_blocked(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ProviderRequestError>())
        .is_some_and(|error| {
            matches!(
                error,
                ProviderRequestError::Unavailable { .. }
                    | ProviderRequestError::Busy { .. }
                    | ProviderRequestError::Deferred { .. }
                    | ProviderRequestError::Stopped(_)
                    | ProviderRequestError::Upstream {
                        kind: ProviderFailureKind::RateLimited
                            | ProviderFailureKind::Authentication
                            | ProviderFailureKind::HardBlocked,
                        ..
                    }
            )
        })
}

#[cfg(test)]
mod tests {
    use crate::model::ArtistCatalogRole;

    use super::{has_primary_role, paths_overlap, tracker_message_reports_unregistered};

    #[test]
    fn coverage_uses_only_primary_artist_catalog_groups() {
        assert!(has_primary_role(&[ArtistCatalogRole::Primary]));
        assert!(!has_primary_role(&[
            ArtistCatalogRole::Guest,
            ArtistCatalogRole::Remixer,
        ]));
    }

    #[test]
    fn recognizes_explicit_tracker_removal_messages() {
        assert!(tracker_message_reports_unregistered("Unregistered torrent"));
        assert!(tracker_message_reports_unregistered(
            "Torrent has been removed by staff"
        ));
        assert!(!tracker_message_reports_unregistered(
            "The tracker is not working"
        ));
        assert!(!tracker_message_reports_unregistered(
            "No peers are currently available"
        ));
    }

    #[test]
    fn cleanup_path_guard_rejects_parent_child_sharing_but_not_siblings() {
        assert!(paths_overlap(
            "/music/ops/Artist/Album",
            "/music/ops/Artist/Album/disc1"
        ));
        assert!(!paths_overlap(
            "/music/ops/Artist/Album",
            "/music/ops/Artist/Album Deluxe"
        ));
    }
}
