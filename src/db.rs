use std::{collections::HashMap, path::Path, str::FromStr, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, ConnectOptions, ConnectionTrait,
    Database as SeaDatabase, DatabaseConnection, DatabaseTransaction, EntityTrait, IntoActiveModel,
    ModelTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, SqliteTransactionMode,
    TransactionOptions, TransactionTrait, sea_query::Expr,
};
use sea_orm_migration::MigratorTrait;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    dedupe::{CatalogMembership, RawSingleCoverage, ReleaseTrackIndex},
    entity::{
        artist_source, background_job, canonical_alias, canonical_artist, canonical_backfill_state,
        canonical_release, canonical_release_artist, canonical_release_credit, canonical_torrent,
        channel_config, channel_pack, channel_pack_item, channel_run, dedupe_catalog_membership,
        download_client_scan, download_event, download_job, download_release_link,
        import_supersession, import_task, match_candidate, provider_state, release_source,
        release_track_index, runtime_preference, single_album_coverage, tracker_snapshot,
    },
    migration::Migrator,
    model::{
        ArtistCatalogPage, ArtistCreditSource, ArtistRole, BackgroundJobCounts, BackgroundJobState,
        BackgroundJobStatus, BackgroundJobsOverview, CanonicalTorrent, ChannelConfig, ChannelPack,
        ChannelPackDecision, ChannelPackItem, ChannelPackSummary, ChannelRun, ChannelRunPhase,
        ChannelRunStatus, ChannelRunTrigger, DownloadIndexCounts, DownloadJob, DownloadState,
        ImportCleanupMode, ImportSupersession, ImportTask, ImportTaskCounts, ImportTaskState,
        ImportsPage, LiveDownloadStatus, ProviderCircuitState, ReleaseDetail, ReleaseDownload,
        ReleaseSummary, RuntimePreferences, TrumpedDownloadRef,
    },
    plex::PlexScanTarget,
};

#[derive(Clone)]
pub struct Database {
    connection: DatabaseConnection,
}

pub struct Cached<T> {
    pub value: T,
    pub fetched_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct DownloadReleaseLink {
    pub client: String,
    pub info_hash: String,
    pub announce_host: Option<String>,
    pub torrent_name: Option<String>,
    pub tracker: Option<String>,
    pub torrent_id: Option<i64>,
    pub resolution_state: String,
}

pub struct LibraryRecord {
    pub release: Cached<ReleaseSummary>,
    pub variant: Option<crate::model::TorrentVariant>,
    pub client: String,
    pub info_hash: String,
    pub present: bool,
    pub library_added_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub missing_since: Option<DateTime<Utc>>,
}

pub struct IndexedDownload {
    pub release: Cached<ReleaseSummary>,
    pub variant: Option<crate::model::TorrentVariant>,
    pub client: String,
    pub info_hash: String,
    pub live: Option<LiveDownloadStatus>,
    pub observed_at: Option<DateTime<Utc>>,
}

struct ResolvedDownloadMetadata {
    canonical_by_identity: HashMap<(String, i64), canonical_torrent::Model>,
    release_by_id: HashMap<String, canonical_release::Model>,
}

pub struct DownloadObservation {
    pub torrent_name: Option<String>,
    pub live: LiveDownloadStatus,
    pub announce_host: Option<String>,
    pub tracker: Option<String>,
    pub plex_target: Option<PlexScanTarget>,
}

#[derive(Debug, Clone)]
pub struct UnlinkedDownload {
    pub torrent_name: String,
    pub tracker: Option<String>,
    pub in_library: bool,
    pub live: LiveDownloadStatus,
}

pub struct CreateReplacementImport<'a> {
    pub download_job_id: Option<Uuid>,
    pub target_client: Option<&'a str>,
    pub target_info_hash: Option<&'a str>,
    pub release_id: Option<Uuid>,
    pub tracker: &'a str,
    pub torrent_id: i64,
    pub display_name: &'a str,
    pub target_complete: bool,
    pub sources: &'a [TrumpedDownloadRef],
    pub cleanup_mode: ImportCleanupMode,
}

#[derive(Debug, Clone)]
pub struct TrackIndexJob {
    pub tracker: String,
    pub group_id: i64,
}

pub struct StoredTrackIndex {
    pub tracker: String,
    pub group_id: i64,
    pub state: String,
    pub index: Option<ReleaseTrackIndex>,
}

#[derive(Clone)]
pub struct StoredCoverage {
    pub state: String,
    pub coverage: Option<RawSingleCoverage>,
}

pub struct TrackIndexProgress {
    pub indexed: usize,
    pub pending: usize,
    pub resolving: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
pub struct StoredProviderState {
    pub id: String,
    pub display_name: String,
    pub kind: String,
    pub state: ProviderCircuitState,
    pub reason_code: Option<String>,
    pub message: Option<String>,
    pub last_request_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
    pub retry_at: Option<DateTime<Utc>>,
    pub last_background_request_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub minimum_interval_ms: u64,
    pub background_minimum_interval_ms: u64,
    pub max_concurrency: u32,
}

#[derive(Debug, Clone)]
pub struct StoredBackgroundJob {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub provider_id: Option<String>,
    pub lane: String,
    pub attempts: u32,
    pub max_attempts: u32,
}

pub struct EnqueueBackgroundJob<'a> {
    pub deduplication_key: &'a str,
    pub kind: &'a str,
    pub payload: Value,
    pub provider_id: Option<String>,
    pub lane: &'a str,
    pub priority: i64,
    pub max_attempts: u32,
    pub next_run_at: Option<DateTime<Utc>>,
    pub parent_id: Option<Uuid>,
    pub recurring_interval_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalBackfillProgress {
    pub state: String,
    pub processed: i64,
    pub total: i64,
    pub remaining: i64,
    pub last_error: Option<String>,
}

impl Database {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let url = format!("sqlite://{}?mode=rwc", path.to_string_lossy());
        let mut options = ConnectOptions::new(url);
        options
            .max_connections(8)
            .sqlx_logging(false)
            .map_sqlx_sqlite_opts(|options| {
                options
                    .journal_mode(sea_orm::sqlx::sqlite::SqliteJournalMode::Wal)
                    .busy_timeout(Duration::from_secs(30))
                    .foreign_keys(true)
            });
        let connection = SeaDatabase::connect(options)
            .await
            .context("connect to SQLite")?;
        Migrator::up(&connection, None)
            .await
            .context("run database migrations")?;
        Ok(Self { connection })
    }

    pub async fn ping(&self) -> Result<()> {
        self.connection.ping().await?;
        Ok(())
    }

    async fn begin_write(&self) -> Result<DatabaseTransaction> {
        Ok(self
            .connection
            .begin_with_options(TransactionOptions {
                sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
                ..Default::default()
            })
            .await?)
    }

    pub async fn get_snapshot<T: DeserializeOwned>(
        &self,
        tracker: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<Cached<T>>> {
        let model = tracker_snapshot::Entity::find()
            .filter(tracker_snapshot::Column::Tracker.eq(tracker))
            .filter(tracker_snapshot::Column::ResourceKind.eq(kind))
            .filter(tracker_snapshot::Column::ResourceKey.eq(key))
            .one(&self.connection)
            .await?;
        model.map(snapshot_from_model).transpose()
    }

    pub async fn get_runtime_preferences(&self) -> Result<RuntimePreferences> {
        let model = runtime_preference::Entity::find_by_id("runtime")
            .one(&self.connection)
            .await?;
        model
            .map(|model| {
                let mut preferences: RuntimePreferences = serde_json::from_value(model.value_json)?;
                preferences.release = preferences.release.migrate_legacy();
                Ok(preferences)
            })
            .unwrap_or_else(|| Ok(RuntimePreferences::default()))
    }

    pub async fn put_runtime_preferences(&self, preferences: &RuntimePreferences) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let value = serde_json::to_value(preferences)?;
        if let Some(model) = runtime_preference::Entity::find_by_id("runtime")
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.value_json = Set(value);
            active.updated_at = Set(now);
            active.update(&self.connection).await?;
        } else {
            runtime_preference::ActiveModel {
                key: Set("runtime".into()),
                value_json: Set(value),
                updated_at: Set(now),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    pub async fn enqueue_background_job(&self, request: EnqueueBackgroundJob<'_>) -> Result<Uuid> {
        let transaction = self.begin_write().await?;
        let id = self
            .enqueue_background_job_on(&transaction, request)
            .await?;
        transaction.commit().await?;
        Ok(id)
    }

    pub async fn enqueue_plex_scan(
        &self,
        target: &PlexScanTarget,
        detected_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        let transaction = self.begin_write().await?;
        let id = self
            .enqueue_plex_scan_on(&transaction, target, detected_at)
            .await?;
        transaction.commit().await?;
        Ok(id)
    }

    async fn enqueue_plex_scan_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        target: &PlexScanTarget,
        detected_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        let bucket = detected_at.timestamp().div_euclid(60);
        let run_at = DateTime::from_timestamp((bucket + 1) * 60 + 15, 0)
            .context("calculate Plex scan debounce")?;
        self.enqueue_background_job_on(
            connection,
            EnqueueBackgroundJob {
                deduplication_key: &target.key_for_bucket(bucket),
                kind: "notify_plex",
                payload: serde_json::json!({
                    "sectionId": target.section_id,
                    "root": target.root,
                }),
                provider_id: Some("plex".into()),
                lane: "event",
                priority: 30,
                max_attempts: 10,
                next_run_at: Some(run_at),
                parent_id: None,
                recurring_interval_seconds: None,
            },
        )
        .await
    }

    async fn enqueue_background_job_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        request: EnqueueBackgroundJob<'_>,
    ) -> Result<Uuid> {
        let now = Utc::now();
        if let Some(model) = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(request.deduplication_key))
            .one(connection)
            .await?
        {
            let id = Uuid::parse_str(&model.id)?;
            let mut active = model.clone().into_active_model();
            active.kind = Set(request.kind.into());
            active.payload_json = Set(request.payload);
            active.priority = Set(model.priority.max(request.priority));
            active.max_attempts = Set(i64::from(request.max_attempts.max(1)));
            active.parent_id = Set(request.parent_id.map(|value| value.to_string()));
            active.recurring_interval_seconds = Set(request
                .recurring_interval_seconds
                .map(|value| i64::try_from(value).unwrap_or(i64::MAX)));
            active.provider_id = Set(request.provider_id);
            active.lane = Set(request.lane.into());
            active.updated_at = Set(now.to_rfc3339());
            active.update(connection).await?;
            return Ok(id);
        }
        let id = Uuid::new_v4();
        background_job::ActiveModel {
            id: Set(id.to_string()),
            deduplication_key: Set(request.deduplication_key.into()),
            kind: Set(request.kind.into()),
            payload_json: Set(request.payload),
            state: Set("pending".into()),
            provider_id: Set(request.provider_id),
            lane: Set(request.lane.into()),
            priority: Set(request.priority),
            attempts: Set(0),
            deferrals: Set(0),
            max_attempts: Set(i64::from(request.max_attempts.max(1))),
            next_run_at: Set(request.next_run_at.map(|value| value.to_rfc3339())),
            lease_owner: Set(None),
            lease_until: Set(None),
            progress_completed: Set(0),
            progress_total: Set(None),
            progress_message: Set(None),
            last_error_code: Set(None),
            last_error_message: Set(None),
            parent_id: Set(request.parent_id.map(|value| value.to_string())),
            recurring_interval_seconds: Set(request
                .recurring_interval_seconds
                .map(|value| i64::try_from(value).unwrap_or(i64::MAX))),
            created_at: Set(now.to_rfc3339()),
            updated_at: Set(now.to_rfc3339()),
            started_at: Set(None),
            finished_at: Set(None),
            cancelled_at: Set(None),
        }
        .insert(connection)
        .await?;
        Ok(id)
    }

    async fn reactivate_completed_background_job_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        key: &str,
    ) -> Result<()> {
        let Some(model) = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(key))
            .filter(background_job::Column::State.eq("completed"))
            .one(connection)
            .await?
        else {
            return Ok(());
        };
        let mut active = model.into_active_model();
        active.state = Set("pending".into());
        active.attempts = Set(0);
        active.deferrals = Set(0);
        active.next_run_at = Set(None);
        active.progress_completed = Set(0);
        active.progress_total = Set(None);
        active.progress_message = Set(None);
        active.last_error_code = Set(None);
        active.last_error_message = Set(None);
        active.finished_at = Set(None);
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(connection).await?;
        Ok(())
    }

    pub async fn recover_expired_background_jobs(&self) -> Result<u64> {
        let now = Utc::now();
        let cutoff = now.to_rfc3339();
        let models = background_job::Entity::find()
            .filter(background_job::Column::State.eq("running"))
            .filter(background_job::Column::LeaseUntil.lte(cutoff.clone()))
            .all(&self.connection)
            .await?;
        let mut count = 0;
        for model in models {
            let attempts = model.attempts + 1;
            let terminal = attempts >= model.max_attempts;
            let result = background_job::Entity::update_many()
                .col_expr(
                    background_job::Column::State,
                    Expr::value(if terminal { "failed" } else { "retrying" }),
                )
                .col_expr(background_job::Column::Attempts, Expr::value(attempts))
                .col_expr(
                    background_job::Column::NextRunAt,
                    Expr::value((!terminal).then(|| now.to_rfc3339())),
                )
                .col_expr(
                    background_job::Column::LeaseOwner,
                    Expr::value(None::<String>),
                )
                .col_expr(
                    background_job::Column::LeaseUntil,
                    Expr::value(None::<String>),
                )
                .col_expr(
                    background_job::Column::LastErrorCode,
                    Expr::value(Some("worker_lease_expired")),
                )
                .col_expr(
                    background_job::Column::LastErrorMessage,
                    Expr::value(Some("Worker stopped before completing the job")),
                )
                .col_expr(
                    background_job::Column::FinishedAt,
                    Expr::value(terminal.then(|| now.to_rfc3339())),
                )
                .col_expr(
                    background_job::Column::ProgressMessage,
                    Expr::value(None::<String>),
                )
                .col_expr(
                    background_job::Column::UpdatedAt,
                    Expr::value(now.to_rfc3339()),
                )
                .filter(background_job::Column::Id.eq(model.id))
                .filter(background_job::Column::State.eq("running"))
                .filter(background_job::Column::LeaseUntil.lte(cutoff.clone()))
                .exec(&self.connection)
                .await?;
            count += result.rows_affected;
        }
        Ok(count)
    }

    pub async fn release_background_job_lease(&self, owner: &str) -> Result<u64> {
        let now = Utc::now();
        Ok(background_job::Entity::update_many()
            .col_expr(background_job::Column::State, Expr::value("retrying"))
            .col_expr(
                background_job::Column::NextRunAt,
                Expr::value(Some(now.to_rfc3339())),
            )
            .col_expr(
                background_job::Column::LeaseOwner,
                Expr::value(None::<String>),
            )
            .col_expr(
                background_job::Column::LeaseUntil,
                Expr::value(None::<String>),
            )
            .col_expr(
                background_job::Column::ProgressMessage,
                Expr::value(None::<String>),
            )
            .col_expr(
                background_job::Column::LastErrorCode,
                Expr::value(Some("service_shutdown")),
            )
            .col_expr(
                background_job::Column::LastErrorMessage,
                Expr::value(Some("Service stopped before the job completed")),
            )
            .col_expr(
                background_job::Column::UpdatedAt,
                Expr::value(now.to_rfc3339()),
            )
            .filter(background_job::Column::State.eq("running"))
            .filter(background_job::Column::LeaseOwner.eq(owner))
            .exec(&self.connection)
            .await?
            .rows_affected)
    }

    pub async fn claim_background_job(
        &self,
        owner: &str,
        lane: &str,
        lease_duration: std::time::Duration,
    ) -> Result<Option<StoredBackgroundJob>> {
        loop {
            let now = Utc::now();
            let candidates = background_job::Entity::find()
                .filter(background_job::Column::State.is_in(["pending", "retrying"]))
                .filter(background_job::Column::Lane.eq(lane))
                .filter(
                    Condition::any()
                        .add(background_job::Column::NextRunAt.is_null())
                        .add(background_job::Column::NextRunAt.lte(now.to_rfc3339())),
                )
                .order_by_desc(background_job::Column::Priority)
                .order_by_asc(background_job::Column::CreatedAt)
                .limit(2_000)
                .all(&self.connection)
                .await?;
            if candidates.is_empty() {
                return Ok(None);
            }
            let running_providers = background_job::Entity::find()
                .filter(background_job::Column::State.eq("running"))
                .filter(background_job::Column::ProviderId.is_not_null())
                .all(&self.connection)
                .await?
                .into_iter()
                .filter_map(|job| job.provider_id)
                .collect::<std::collections::HashSet<_>>();
            let provider_states = provider_state::Entity::find()
                .all(&self.connection)
                .await?
                .into_iter()
                .map(|state| (state.id.clone(), state))
                .collect::<std::collections::HashMap<_, _>>();
            let Some(model) = candidates.into_iter().find(|job| {
                background_provider_is_due(job, &provider_states, &running_providers, now)
            }) else {
                return Ok(None);
            };
            let stored = stored_background_job(&model)?;
            let result = background_job::Entity::update_many()
                .col_expr(background_job::Column::State, Expr::value("running"))
                .col_expr(
                    background_job::Column::LeaseOwner,
                    Expr::value(Some(owner.to_owned())),
                )
                .col_expr(
                    background_job::Column::LeaseUntil,
                    Expr::value(Some(
                        (now + chrono::Duration::from_std(lease_duration)?).to_rfc3339(),
                    )),
                )
                .col_expr(
                    background_job::Column::StartedAt,
                    Expr::value(Some(now.to_rfc3339())),
                )
                .col_expr(
                    background_job::Column::UpdatedAt,
                    Expr::value(now.to_rfc3339()),
                )
                .filter(background_job::Column::Id.eq(model.id))
                .filter(background_job::Column::State.is_in(["pending", "retrying"]))
                .exec(&self.connection)
                .await?;
            if result.rows_affected == 1 {
                return Ok(Some(stored));
            }
        }
    }

    pub async fn heartbeat_background_job(
        &self,
        id: Uuid,
        owner: &str,
        lease_duration: std::time::Duration,
        completed: u64,
        total: Option<u64>,
        message: Option<&str>,
    ) -> Result<bool> {
        let Some(model) = background_job::Entity::find_by_id(id.to_string())
            .filter(background_job::Column::State.eq("running"))
            .filter(background_job::Column::LeaseOwner.eq(owner))
            .one(&self.connection)
            .await?
        else {
            return Ok(false);
        };
        let now = Utc::now();
        let mut active = model.into_active_model();
        active.lease_until = Set(Some(
            (now + chrono::Duration::from_std(lease_duration)?).to_rfc3339(),
        ));
        active.progress_completed = Set(i64::try_from(completed).unwrap_or(i64::MAX));
        active.progress_total = Set(total.map(|value| i64::try_from(value).unwrap_or(i64::MAX)));
        active.progress_message = Set(message.map(|value| value.chars().take(200).collect()));
        active.updated_at = Set(now.to_rfc3339());
        active.update(&self.connection).await?;
        Ok(true)
    }

    pub async fn set_background_job_provider(&self, id: Uuid, provider_id: &str) -> Result<()> {
        background_job::Entity::update_many()
            .col_expr(
                background_job::Column::ProviderId,
                Expr::value(Some(provider_id.to_owned())),
            )
            .col_expr(
                background_job::Column::UpdatedAt,
                Expr::value(Utc::now().to_rfc3339()),
            )
            .filter(background_job::Column::Id.eq(id.to_string()))
            .filter(background_job::Column::State.eq("running"))
            .exec(&self.connection)
            .await?;
        Ok(())
    }

    pub async fn complete_background_job(&self, id: Uuid, owner: &str) -> Result<()> {
        let Some(model) = background_job::Entity::find_by_id(id.to_string())
            .filter(background_job::Column::LeaseOwner.eq(owner))
            .one(&self.connection)
            .await?
        else {
            return Ok(());
        };
        if model.state == "cancelled" {
            return Ok(());
        }
        let now = Utc::now();
        let recurring = model.recurring_interval_seconds;
        let mut active = model.into_active_model();
        if let Some(seconds) = recurring {
            active.state = Set("pending".into());
            active.attempts = Set(0);
            active.deferrals = Set(0);
            active.next_run_at = Set(Some(
                (now + chrono::Duration::seconds(seconds.max(1))).to_rfc3339(),
            ));
            active.progress_completed = Set(0);
            active.progress_total = Set(None);
            active.progress_message = Set(None);
        } else {
            active.state = Set("completed".into());
            active.next_run_at = Set(None);
        }
        active.lease_owner = Set(None);
        active.lease_until = Set(None);
        active.progress_message = Set(None);
        active.last_error_code = Set(None);
        active.last_error_message = Set(None);
        active.finished_at = Set(Some(now.to_rfc3339()));
        active.updated_at = Set(now.to_rfc3339());
        active.update(&self.connection).await?;
        Ok(())
    }

    pub async fn fail_background_job(
        &self,
        id: Uuid,
        owner: &str,
        code: &str,
        message: &str,
    ) -> Result<()> {
        let Some(model) = background_job::Entity::find_by_id(id.to_string())
            .filter(background_job::Column::LeaseOwner.eq(owner))
            .one(&self.connection)
            .await?
        else {
            return Ok(());
        };
        if model.state == "cancelled" {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let mut active = model.into_active_model();
        active.state = Set("failed".into());
        active.next_run_at = Set(None);
        active.lease_owner = Set(None);
        active.lease_until = Set(None);
        active.last_error_code = Set(Some(code.chars().take(100).collect()));
        active.last_error_message = Set(Some(message.chars().take(500).collect()));
        active.finished_at = Set(Some(now.clone()));
        active.updated_at = Set(now);
        active.update(&self.connection).await?;
        Ok(())
    }

    pub async fn wait_background_job(
        &self,
        id: Uuid,
        owner: &str,
        code: &str,
        message: &str,
    ) -> Result<()> {
        let Some(model) = background_job::Entity::find_by_id(id.to_string())
            .filter(background_job::Column::LeaseOwner.eq(owner))
            .one(&self.connection)
            .await?
        else {
            return Ok(());
        };
        if model.state == "cancelled" {
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        let deferrals = model.deferrals.saturating_add(1);
        let mut active = model.into_active_model();
        active.state = Set("waiting".into());
        active.deferrals = Set(deferrals);
        active.next_run_at = Set(None);
        active.lease_owner = Set(None);
        active.lease_until = Set(None);
        active.progress_message = Set(None);
        active.last_error_code = Set(Some(code.chars().take(100).collect()));
        active.last_error_message = Set(Some(message.chars().take(500).collect()));
        active.finished_at = Set(None);
        active.updated_at = Set(now);
        active.update(&self.connection).await?;
        Ok(())
    }

    pub async fn active_background_jobs_by_kind(&self, kind: &str) -> Result<u64> {
        Ok(background_job::Entity::find()
            .filter(background_job::Column::Kind.eq(kind))
            .filter(
                background_job::Column::State.is_in(["pending", "running", "retrying", "waiting"]),
            )
            .count(&self.connection)
            .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn retry_background_job(
        &self,
        id: Uuid,
        owner: &str,
        delay: std::time::Duration,
        increment_attempt: bool,
        code: &str,
        message: &str,
    ) -> Result<()> {
        let Some(model) = background_job::Entity::find_by_id(id.to_string())
            .filter(background_job::Column::LeaseOwner.eq(owner))
            .one(&self.connection)
            .await?
        else {
            return Ok(());
        };
        if model.state == "cancelled" {
            return Ok(());
        }
        let attempts = model.attempts + i64::from(increment_attempt);
        let deferrals = model.deferrals + i64::from(!increment_attempt);
        let terminal = increment_attempt && attempts >= model.max_attempts;
        let now = Utc::now();
        let mut active = model.into_active_model();
        active.state = Set(if terminal { "failed" } else { "retrying" }.into());
        active.attempts = Set(attempts);
        active.deferrals = Set(deferrals);
        active.next_run_at = Set((!terminal).then(|| {
            (now + chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::hours(1)))
                .to_rfc3339()
        }));
        active.lease_owner = Set(None);
        active.lease_until = Set(None);
        active.progress_message = Set(None);
        active.last_error_code = Set(Some(code.into()));
        active.last_error_message = Set(Some(message.chars().take(500).collect()));
        active.finished_at = Set(terminal.then(|| now.to_rfc3339()));
        active.updated_at = Set(now.to_rfc3339());
        active.update(&self.connection).await?;
        Ok(())
    }

    pub async fn background_jobs_overview(&self, limit: u64) -> Result<BackgroundJobsOverview> {
        let mut counts = BackgroundJobCounts::default();
        for (state, count) in [
            ("pending", &mut counts.pending),
            ("running", &mut counts.running),
            ("retrying", &mut counts.retrying),
            ("waiting", &mut counts.waiting),
            ("completed", &mut counts.completed),
            ("failed", &mut counts.failed),
            ("cancelled", &mut counts.cancelled),
        ] {
            *count = background_job::Entity::find()
                .filter(background_job::Column::State.eq(state))
                .count(&self.connection)
                .await?;
        }
        let limit = limit.clamp(1, 500);
        let mut jobs = Vec::new();
        for state in [
            "running",
            "retrying",
            "waiting",
            "failed",
            "pending",
            "completed",
            "cancelled",
        ] {
            let remaining = limit.saturating_sub(jobs.len() as u64);
            if remaining == 0 {
                break;
            }
            let mut query = background_job::Entity::find()
                .filter(background_job::Column::State.eq(state))
                .limit(remaining);
            query = if state == "pending" {
                query
                    .order_by_desc(background_job::Column::Priority)
                    .order_by_asc(background_job::Column::CreatedAt)
            } else if state == "retrying" {
                query
                    .order_by_asc(background_job::Column::NextRunAt)
                    .order_by_desc(background_job::Column::Priority)
            } else {
                query.order_by_desc(background_job::Column::UpdatedAt)
            };
            jobs.extend(
                query
                    .all(&self.connection)
                    .await?
                    .into_iter()
                    .map(background_job_status)
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        Ok(BackgroundJobsOverview { counts, jobs })
    }

    pub async fn prune_background_jobs(&self, retention: chrono::Duration) -> Result<u64> {
        let cutoff = (Utc::now() - retention).to_rfc3339();
        Ok(background_job::Entity::delete_many()
            .filter(background_job::Column::State.is_in(["completed", "cancelled"]))
            .filter(background_job::Column::UpdatedAt.lt(cutoff))
            .exec(&self.connection)
            .await?
            .rows_affected)
    }

    pub async fn cancel_background_job(&self, id: Uuid) -> Result<bool> {
        let Some(model) = background_job::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        else {
            return Ok(false);
        };
        if matches!(model.state.as_str(), "completed" | "failed" | "cancelled") {
            return Ok(false);
        }
        let now = Utc::now();
        let mut active = model.into_active_model();
        active.state = Set("cancelled".into());
        active.cancelled_at = Set(Some(now.to_rfc3339()));
        active.lease_owner = Set(None);
        active.lease_until = Set(None);
        active.progress_message = Set(None);
        active.updated_at = Set(now.to_rfc3339());
        active.update(&self.connection).await?;
        Ok(true)
    }

    pub async fn retry_failed_background_job(&self, id: Uuid) -> Result<bool> {
        let Some(model) = background_job::Entity::find_by_id(id.to_string())
            .filter(background_job::Column::State.is_in(["failed", "cancelled"]))
            .one(&self.connection)
            .await?
        else {
            return Ok(false);
        };
        let mut active = model.into_active_model();
        active.state = Set("pending".into());
        active.attempts = Set(0);
        active.next_run_at = Set(None);
        active.lease_owner = Set(None);
        active.lease_until = Set(None);
        active.progress_completed = Set(0);
        active.progress_total = Set(None);
        active.progress_message = Set(None);
        active.last_error_code = Set(None);
        active.last_error_message = Set(None);
        active.finished_at = Set(None);
        active.cancelled_at = Set(None);
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        Ok(true)
    }

    pub async fn retry_background_job_by_key(&self, key: &str) -> Result<bool> {
        let Some(model) = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(key))
            .filter(background_job::Column::State.is_in(["failed", "cancelled", "completed"]))
            .one(&self.connection)
            .await?
        else {
            return Ok(false);
        };
        let mut active = model.into_active_model();
        active.state = Set("pending".into());
        active.attempts = Set(0);
        active.deferrals = Set(0);
        active.next_run_at = Set(None);
        active.lease_owner = Set(None);
        active.lease_until = Set(None);
        active.progress_completed = Set(0);
        active.progress_total = Set(None);
        active.progress_message = Set(None);
        active.last_error_code = Set(None);
        active.last_error_message = Set(None);
        active.finished_at = Set(None);
        active.cancelled_at = Set(None);
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        Ok(true)
    }

    pub async fn background_job_id_by_key(&self, key: &str) -> Result<Option<Uuid>> {
        background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(key))
            .one(&self.connection)
            .await?
            .map(|model| Uuid::parse_str(&model.id).map_err(Into::into))
            .transpose()
    }

    pub async fn background_job_by_key(&self, key: &str) -> Result<Option<BackgroundJobStatus>> {
        background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(key))
            .one(&self.connection)
            .await?
            .map(background_job_status)
            .transpose()
    }

    pub async fn retry_completed_background_job_by_key(&self, key: &str) -> Result<bool> {
        let Some(model) = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(key))
            .filter(background_job::Column::State.eq("completed"))
            .one(&self.connection)
            .await?
        else {
            return Ok(false);
        };
        let mut active = model.into_active_model();
        active.state = Set("pending".into());
        active.attempts = Set(0);
        active.deferrals = Set(0);
        active.next_run_at = Set(None);
        active.progress_completed = Set(0);
        active.progress_total = Set(None);
        active.progress_message = Set(None);
        active.last_error_code = Set(None);
        active.last_error_message = Set(None);
        active.finished_at = Set(None);
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        Ok(true)
    }

    pub async fn resume_waiting_jobs_for_provider(&self, provider_id: &str) -> Result<u64> {
        let now = Utc::now().to_rfc3339();
        Ok(background_job::Entity::update_many()
            .col_expr(background_job::Column::State, Expr::value("pending"))
            .col_expr(
                background_job::Column::NextRunAt,
                Expr::value(Some(now.clone())),
            )
            .col_expr(
                background_job::Column::LastErrorCode,
                Expr::value(None::<String>),
            )
            .col_expr(
                background_job::Column::LastErrorMessage,
                Expr::value(None::<String>),
            )
            .col_expr(background_job::Column::UpdatedAt, Expr::value(now))
            .filter(background_job::Column::State.eq("waiting"))
            .filter(background_job::Column::ProviderId.eq(provider_id))
            .exec(&self.connection)
            .await?
            .rows_affected)
    }

    async fn resume_waiting_single_coverages(
        &self,
        tracker: &str,
        group_ids: &[i64],
    ) -> Result<u64> {
        if group_ids.is_empty() {
            return Ok(0);
        }
        let tracker = tracker.to_ascii_lowercase();
        let keys = group_ids
            .iter()
            .map(|group_id| format!("single-coverage:{tracker}:{group_id}:v2"))
            .collect::<Vec<_>>();
        let now = Utc::now().to_rfc3339();
        Ok(background_job::Entity::update_many()
            .col_expr(background_job::Column::State, Expr::value("pending"))
            .col_expr(
                background_job::Column::NextRunAt,
                Expr::value(Some(now.clone())),
            )
            .col_expr(background_job::Column::UpdatedAt, Expr::value(now))
            .filter(background_job::Column::State.eq("waiting"))
            .filter(background_job::Column::DeduplicationKey.is_in(keys))
            .exec(&self.connection)
            .await?
            .rows_affected)
    }

    pub async fn reconcile_waiting_single_coverages(&self) -> Result<u64> {
        let now = Utc::now().to_rfc3339();
        Ok(background_job::Entity::update_many()
            .col_expr(background_job::Column::State, Expr::value("pending"))
            .col_expr(
                background_job::Column::NextRunAt,
                Expr::value(Some(now.clone())),
            )
            .col_expr(
                background_job::Column::LastErrorCode,
                Expr::value(None::<String>),
            )
            .col_expr(
                background_job::Column::LastErrorMessage,
                Expr::value(None::<String>),
            )
            .col_expr(background_job::Column::UpdatedAt, Expr::value(now))
            .filter(background_job::Column::Kind.eq("compute_single_coverage"))
            .filter(background_job::Column::State.eq("waiting"))
            .exec(&self.connection)
            .await?
            .rows_affected)
    }

    pub async fn reconcile_waiting_single_coverage_track_indexes(&self) -> Result<u64> {
        let jobs = background_job::Entity::find()
            .filter(background_job::Column::Kind.eq("compute_single_coverage"))
            .filter(background_job::Column::State.eq("waiting"))
            .filter(background_job::Column::LastErrorMessage.contains("Single tracklist"))
            .all(&self.connection)
            .await?;
        let mut reconciled = 0;
        for job in jobs {
            let tracker = job
                .payload_json
                .get("tracker")
                .and_then(Value::as_str)
                .context("waiting Single coverage job is missing its tracker")?;
            let group_id = job
                .payload_json
                .get("groupId")
                .and_then(Value::as_i64)
                .context("waiting Single coverage job is missing its group id")?;
            self.enqueue_track_index(tracker, group_id).await?;
            reconciled += 1;
        }
        Ok(reconciled)
    }

    pub async fn provider_state(&self, id: &str) -> Result<Option<StoredProviderState>> {
        provider_state::Entity::find_by_id(id)
            .one(&self.connection)
            .await?
            .map(provider_state_from_model)
            .transpose()
    }

    pub async fn put_provider_state(&self, state: &StoredProviderState) -> Result<()> {
        let updated_at = Utc::now().to_rfc3339();
        if let Some(model) = provider_state::Entity::find_by_id(&state.id)
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.display_name = Set(state.display_name.clone());
            active.kind = Set(state.kind.clone());
            active.state = Set(state.state.as_str().into());
            active.reason_code = Set(state.reason_code.clone());
            active.message = Set(state.message.clone());
            active.last_request_at = Set(state.last_request_at.map(|value| value.to_rfc3339()));
            active.last_success_at = Set(state.last_success_at.map(|value| value.to_rfc3339()));
            active.last_failure_at = Set(state.last_failure_at.map(|value| value.to_rfc3339()));
            active.retry_at = Set(state.retry_at.map(|value| value.to_rfc3339()));
            active.last_background_request_at = Set(state
                .last_background_request_at
                .map(|value| value.to_rfc3339()));
            active.consecutive_failures = Set(i64::from(state.consecutive_failures));
            active.minimum_interval_ms =
                Set(i64::try_from(state.minimum_interval_ms).unwrap_or(i64::MAX));
            active.background_minimum_interval_ms =
                Set(i64::try_from(state.background_minimum_interval_ms).unwrap_or(i64::MAX));
            active.max_concurrency = Set(i64::from(state.max_concurrency));
            active.updated_at = Set(updated_at);
            active.update(&self.connection).await?;
        } else {
            provider_state::ActiveModel {
                id: Set(state.id.clone()),
                display_name: Set(state.display_name.clone()),
                kind: Set(state.kind.clone()),
                state: Set(state.state.as_str().into()),
                reason_code: Set(state.reason_code.clone()),
                message: Set(state.message.clone()),
                last_request_at: Set(state.last_request_at.map(|value| value.to_rfc3339())),
                last_success_at: Set(state.last_success_at.map(|value| value.to_rfc3339())),
                last_failure_at: Set(state.last_failure_at.map(|value| value.to_rfc3339())),
                retry_at: Set(state.retry_at.map(|value| value.to_rfc3339())),
                last_background_request_at: Set(state
                    .last_background_request_at
                    .map(|value| value.to_rfc3339())),
                consecutive_failures: Set(i64::from(state.consecutive_failures)),
                minimum_interval_ms: Set(
                    i64::try_from(state.minimum_interval_ms).unwrap_or(i64::MAX)
                ),
                background_minimum_interval_ms: Set(i64::try_from(
                    state.background_minimum_interval_ms,
                )
                .unwrap_or(i64::MAX)),
                max_concurrency: Set(i64::from(state.max_concurrency)),
                updated_at: Set(updated_at),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    pub async fn ensure_default_channels(&self) -> Result<()> {
        let now = Utc::now();
        for channel in [
            ChannelConfig::country_chart_default(now),
            ChannelConfig::lastfm_default(now),
            ChannelConfig::trumped_downloads_default(now),
        ] {
            if channel_config::Entity::find_by_id(channel.id.clone())
                .one(&self.connection)
                .await?
                .is_none()
            {
                self.put_channel(&channel).await?;
            }
        }
        Ok(())
    }

    pub async fn list_channels(&self) -> Result<Vec<ChannelConfig>> {
        channel_config::Entity::find()
            .order_by_asc(channel_config::Column::Id)
            .all(&self.connection)
            .await?
            .into_iter()
            .map(|model| serde_json::from_value(model.config_json).map_err(Into::into))
            .collect()
    }

    pub async fn get_channel(&self, id: &str) -> Result<Option<ChannelConfig>> {
        channel_config::Entity::find_by_id(id)
            .one(&self.connection)
            .await?
            .map(|model| serde_json::from_value(model.config_json).map_err(Into::into))
            .transpose()
    }

    pub async fn put_channel(&self, channel: &ChannelConfig) -> Result<()> {
        let value = serde_json::to_value(channel)?;
        let kind = match channel.kind {
            crate::model::ChannelKind::CountryChart => "country_chart",
            crate::model::ChannelKind::Lastfm => "lastfm",
            crate::model::ChannelKind::TrumpedDownloads => "trumped_downloads",
        };
        if let Some(model) = channel_config::Entity::find_by_id(channel.id.clone())
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.kind = Set(kind.into());
            active.enabled = Set(channel.enabled);
            active.config_json = Set(value);
            active.last_successful_at =
                Set(channel.last_successful_at.map(|value| value.to_rfc3339()));
            active.last_error = Set(channel.last_error.clone());
            active.updated_at = Set(channel.updated_at.to_rfc3339());
            active.update(&self.connection).await?;
        } else {
            channel_config::ActiveModel {
                id: Set(channel.id.clone()),
                kind: Set(kind.into()),
                enabled: Set(channel.enabled),
                config_json: Set(value),
                last_successful_at: Set(channel.last_successful_at.map(|value| value.to_rfc3339())),
                last_error: Set(channel.last_error.clone()),
                updated_at: Set(channel.updated_at.to_rfc3339()),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    pub async fn active_channel_run(&self, channel_id: &str) -> Result<Option<ChannelRun>> {
        channel_run::Entity::find()
            .filter(channel_run::Column::ChannelId.eq(channel_id))
            .filter(channel_run::Column::Status.eq("running"))
            .order_by_desc(channel_run::Column::StartedAt)
            .one(&self.connection)
            .await?
            .map(channel_run_from_model)
            .transpose()
    }

    pub async fn recover_channel_runs(&self) -> Result<()> {
        for model in channel_run::Entity::find()
            .filter(channel_run::Column::Status.eq("running"))
            .all(&self.connection)
            .await?
        {
            let channel_id = model.channel_id.clone();
            let message = "Service restarted during channel refresh";
            let mut active = model.into_active_model();
            active.status = Set("failed".into());
            active.error = Set(Some(message.into()));
            active.retry_at = Set(None);
            let now = Utc::now().to_rfc3339();
            active.updated_at = Set(now.clone());
            active.finished_at = Set(Some(now));
            active.update(&self.connection).await?;
            if let Some(mut channel) = self.get_channel(&channel_id).await? {
                channel.last_error = Some(message.into());
                channel.failure_count = channel.failure_count.saturating_add(1);
                channel.updated_at = Utc::now();
                self.put_channel(&channel).await?;
            }
        }
        Ok(())
    }

    pub async fn create_channel_run(
        &self,
        channel_id: &str,
        trigger: ChannelRunTrigger,
    ) -> Result<Option<ChannelRun>> {
        let transaction = self.begin_write().await?;
        if channel_run::Entity::find()
            .filter(channel_run::Column::ChannelId.eq(channel_id))
            .filter(channel_run::Column::Status.eq("running"))
            .one(&transaction)
            .await?
            .is_some()
        {
            transaction.rollback().await?;
            return Ok(None);
        }
        let now = Utc::now();
        let run = ChannelRun {
            id: Uuid::new_v4(),
            channel_id: channel_id.into(),
            trigger,
            status: ChannelRunStatus::Running,
            phase: Some(ChannelRunPhase::Discovering),
            progress_completed: 0,
            progress_total: None,
            progress_message: Some("Contacting recommendation source".into()),
            retry_at: None,
            pack_id: None,
            error: None,
            started_at: now,
            updated_at: now,
            finished_at: None,
        };
        channel_run::ActiveModel {
            id: Set(run.id.to_string()),
            channel_id: Set(run.channel_id.clone()),
            trigger: Set(channel_run_trigger(&run.trigger).into()),
            status: Set("running".into()),
            phase: Set(Some("discovering".into())),
            progress_completed: Set(0),
            progress_total: Set(None),
            progress_message: Set(run.progress_message.clone()),
            retry_at: Set(None),
            pack_id: Set(None),
            error: Set(None),
            started_at: Set(run.started_at.to_rfc3339()),
            updated_at: Set(run.updated_at.to_rfc3339()),
            finished_at: Set(None),
        }
        .insert(&transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some(run))
    }

    pub async fn get_channel_run(&self, id: Uuid) -> Result<Option<ChannelRun>> {
        channel_run::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
            .map(channel_run_from_model)
            .transpose()
    }

    pub async fn finish_channel_run(
        &self,
        id: Uuid,
        status: ChannelRunStatus,
        pack_id: Option<Uuid>,
        error: Option<&str>,
    ) -> Result<()> {
        if let Some(model) = channel_run::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.status = Set(channel_run_status(&status).into());
            active.pack_id = Set(pack_id.map(|value| value.to_string()));
            active.error = Set(error.map(|value| value.chars().take(500).collect()));
            active.retry_at = Set(None);
            let now = Utc::now().to_rfc3339();
            active.updated_at = Set(now.clone());
            active.finished_at = Set(Some(now));
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn update_channel_run_progress(
        &self,
        id: Uuid,
        phase: ChannelRunPhase,
        completed: u32,
        total: Option<u32>,
        message: Option<&str>,
    ) -> Result<()> {
        if let Some(model) = channel_run::Entity::find_by_id(id.to_string())
            .filter(channel_run::Column::Status.eq("running"))
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.phase = Set(Some(channel_run_phase(&phase).into()));
            active.progress_completed = Set(completed.min(i32::MAX as u32) as i32);
            active.progress_total = Set(total.map(|value| value.min(i32::MAX as u32) as i32));
            active.progress_message = Set(message.map(|value| value.chars().take(200).collect()));
            active.retry_at = Set(None);
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn wait_channel_run_for_provider(
        &self,
        id: Uuid,
        completed: u32,
        total: u32,
        message: &str,
        retry_at: DateTime<Utc>,
    ) -> Result<()> {
        if let Some(model) = channel_run::Entity::find_by_id(id.to_string())
            .filter(channel_run::Column::Status.eq("running"))
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.phase = Set(Some(
                channel_run_phase(&ChannelRunPhase::WaitingProvider).into(),
            ));
            active.progress_completed = Set(completed.min(i32::MAX as u32) as i32);
            active.progress_total = Set(Some(total.min(i32::MAX as u32) as i32));
            active.progress_message = Set(Some(message.chars().take(200).collect()));
            active.retry_at = Set(Some(retry_at.to_rfc3339()));
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn create_channel_pack(
        &self,
        channel_id: &str,
        source_title: &str,
        partial: bool,
        preference_fingerprint: &str,
        items: &[ChannelPackItem],
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let transaction = self.begin_write().await?;
        channel_pack::ActiveModel {
            id: Set(id.to_string()),
            channel_id: Set(channel_id.into()),
            decision: Set("open".into()),
            partial: Set(partial),
            source_title: Set(source_title.into()),
            plan_version: Set(1),
            preference_fingerprint: Set(preference_fingerprint.into()),
            created_at: Set(Utc::now().to_rfc3339()),
            decided_at: Set(None),
        }
        .insert(&transaction)
        .await?;
        for item in items {
            channel_pack_item::ActiveModel {
                pack_id: Set(id.to_string()),
                ordinal: Set(item.ordinal as i32),
                item_json: Set(serde_json::to_value(item)?),
            }
            .insert(&transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(id)
    }

    pub async fn get_channel_pack(
        &self,
        id: Uuid,
        current_fingerprint: &str,
    ) -> Result<Option<ChannelPack>> {
        let Some(model) = channel_pack::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        else {
            return Ok(None);
        };
        let items = channel_pack_item::Entity::find()
            .filter(channel_pack_item::Column::PackId.eq(id.to_string()))
            .order_by_asc(channel_pack_item::Column::Ordinal)
            .all(&self.connection)
            .await?
            .into_iter()
            .map(|model| serde_json::from_value(model.item_json).map_err(Into::into))
            .collect::<Result<Vec<ChannelPackItem>>>()?;
        Ok(Some(channel_pack_from_model(
            model,
            items,
            current_fingerprint,
        )?))
    }

    pub async fn list_channel_packs(
        &self,
        channel_id: &str,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<ChannelPackSummary>> {
        let models = channel_pack::Entity::find()
            .filter(channel_pack::Column::ChannelId.eq(channel_id))
            .order_by_desc(channel_pack::Column::CreatedAt)
            .limit(limit.min(100))
            .offset(offset)
            .all(&self.connection)
            .await?;
        let mut result = Vec::with_capacity(models.len());
        for model in models {
            let items = channel_pack_item::Entity::find()
                .filter(channel_pack_item::Column::PackId.eq(model.id.clone()))
                .all(&self.connection)
                .await?
                .into_iter()
                .map(|item| serde_json::from_value(item.item_json).map_err(Into::into))
                .collect::<Result<Vec<ChannelPackItem>>>()?;
            let pack = channel_pack_from_model(model, items, "")?;
            result.push(ChannelPackSummary {
                id: pack.id,
                channel_id: pack.channel_id,
                decision: pack.decision,
                partial: pack.partial,
                source_title: pack.source_title,
                plan_version: pack.plan_version,
                summary: pack.summary,
                created_at: pack.created_at,
            });
        }
        Ok(result)
    }

    pub async fn recent_channel_sources(
        &self,
        channel_id: &str,
        pack_limit: u64,
    ) -> Result<Vec<crate::model::RecommendationSource>> {
        let packs = channel_pack::Entity::find()
            .filter(channel_pack::Column::ChannelId.eq(channel_id))
            .order_by_desc(channel_pack::Column::CreatedAt)
            .limit(pack_limit)
            .all(&self.connection)
            .await?;
        let mut sources = Vec::new();
        for pack in packs {
            for item in channel_pack_item::Entity::find()
                .filter(channel_pack_item::Column::PackId.eq(pack.id))
                .all(&self.connection)
                .await?
            {
                let item: ChannelPackItem = serde_json::from_value(item.item_json)?;
                sources.push(item.source);
            }
        }
        Ok(sources)
    }

    pub async fn handled_channel_sources(
        &self,
        channel_id: &str,
    ) -> Result<Vec<crate::model::RecommendationSource>> {
        let pack_ids = channel_pack::Entity::find()
            .filter(channel_pack::Column::ChannelId.eq(channel_id))
            .filter(channel_pack::Column::Decision.eq("accepted"))
            .all(&self.connection)
            .await?
            .into_iter()
            .map(|pack| pack.id)
            .collect::<Vec<_>>();
        if pack_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut sources_by_job = HashMap::new();
        let mut handled_directly = Vec::new();
        for item in channel_pack_item::Entity::find()
            .filter(channel_pack_item::Column::PackId.is_in(pack_ids))
            .all(&self.connection)
            .await?
        {
            let item: ChannelPackItem = serde_json::from_value(item.item_json)?;
            if let Some(job_id) = item.job_id {
                sources_by_job.insert(job_id.to_string(), item.source);
            } else if item.plan_state == crate::model::PackItemPlanState::Submitted {
                handled_directly.push(item.source);
            }
        }
        if sources_by_job.is_empty() {
            return Ok(handled_directly);
        }
        handled_directly.extend(
            download_job::Entity::find()
                .filter(download_job::Column::Id.is_in(sources_by_job.keys().cloned()))
                .filter(download_job::Column::State.ne("failed"))
                .all(&self.connection)
                .await?
                .into_iter()
                .filter_map(|job| sources_by_job.remove(&job.id)),
        );
        Ok(handled_directly)
    }

    pub async fn replace_channel_plan(
        &self,
        id: Uuid,
        fingerprint: &str,
        items: &[ChannelPackItem],
    ) -> Result<i32> {
        let transaction = self.begin_write().await?;
        let model = channel_pack::Entity::find_by_id(id.to_string())
            .one(&transaction)
            .await?
            .context("channel pack disappeared")?;
        let version = model.plan_version + 1;
        let mut active = model.into_active_model();
        active.plan_version = Set(version);
        active.preference_fingerprint = Set(fingerprint.into());
        active.update(&transaction).await?;
        channel_pack_item::Entity::delete_many()
            .filter(channel_pack_item::Column::PackId.eq(id.to_string()))
            .exec(&transaction)
            .await?;
        for item in items {
            channel_pack_item::ActiveModel {
                pack_id: Set(id.to_string()),
                ordinal: Set(item.ordinal as i32),
                item_json: Set(serde_json::to_value(item)?),
            }
            .insert(&transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(version)
    }

    pub async fn update_channel_pack_item(
        &self,
        pack_id: Uuid,
        item: &ChannelPackItem,
    ) -> Result<()> {
        if let Some(model) =
            channel_pack_item::Entity::find_by_id((pack_id.to_string(), item.ordinal as i32))
                .one(&self.connection)
                .await?
        {
            let mut active = model.into_active_model();
            active.item_json = Set(serde_json::to_value(item)?);
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn decide_channel_pack(&self, id: Uuid, decision: ChannelPackDecision) -> Result<()> {
        if let Some(model) = channel_pack::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.decision = Set(channel_pack_decision(&decision).into());
            active.decided_at = Set(Some(Utc::now().to_rfc3339()));
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn release_download_flags(&self, release_id: Uuid) -> Result<(bool, bool)> {
        let links = download_release_link::Entity::find()
            .filter(download_release_link::Column::ReleaseId.eq(release_id.to_string()))
            .all(&self.connection)
            .await?;
        let owned = links.iter().any(|link| link.library_added_at.is_some());
        let downloading = links
            .iter()
            .any(|link| link.present && link.library_added_at.is_none());
        Ok((owned, downloading))
    }

    pub async fn list_unlinked_downloads(&self) -> Result<Vec<UnlinkedDownload>> {
        let links = download_release_link::Entity::find()
            .filter(download_release_link::Column::Present.eq(true))
            .filter(download_release_link::Column::ReleaseId.is_null())
            .filter(download_release_link::Column::TorrentName.is_not_null())
            .filter(download_release_link::Column::ObservedJson.is_not_null())
            .all(&self.connection)
            .await?;
        links
            .into_iter()
            .filter_map(|link| {
                let name = link.torrent_name?;
                let observed = link.observed_json?;
                Some((
                    name,
                    link.tracker,
                    link.library_added_at.is_some(),
                    observed,
                ))
            })
            .map(|(torrent_name, tracker, in_library, observed)| {
                Ok(UnlinkedDownload {
                    torrent_name,
                    tracker,
                    in_library,
                    live: serde_json::from_value(observed)?,
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub async fn downloaded_torrent_ids(
        &self,
        tracker: &str,
        torrent_ids: &[i64],
    ) -> Result<Vec<i64>> {
        if torrent_ids.is_empty() {
            return Ok(Vec::new());
        }
        let links = download_release_link::Entity::find()
            .filter(download_release_link::Column::Tracker.eq(tracker.to_ascii_lowercase()))
            .filter(download_release_link::Column::TorrentId.is_in(torrent_ids.iter().copied()))
            .filter(
                Condition::any()
                    .add(download_release_link::Column::Present.eq(true))
                    .add(download_release_link::Column::LibraryAddedAt.is_not_null()),
            )
            .all(&self.connection)
            .await?;
        let mut ids = links
            .into_iter()
            .filter_map(|link| link.torrent_id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    pub async fn torrent_downloads(
        &self,
        tracker: &str,
        torrent_id: i64,
    ) -> Result<Vec<ReleaseDownload>> {
        let links = download_release_link::Entity::find()
            .filter(download_release_link::Column::Tracker.eq(tracker.to_ascii_lowercase()))
            .filter(download_release_link::Column::TorrentId.eq(torrent_id))
            .filter(download_release_link::Column::Present.eq(true))
            .all(&self.connection)
            .await?;
        release_downloads_from_links(links)
    }

    pub async fn downloads_for_refs(
        &self,
        refs: &[TrumpedDownloadRef],
    ) -> Result<Vec<ReleaseDownload>> {
        if refs.is_empty() {
            return Ok(Vec::new());
        }
        let clients = refs
            .iter()
            .map(|value| value.client.clone())
            .collect::<Vec<_>>();
        let hashes = refs
            .iter()
            .map(|value| value.info_hash.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let wanted = refs
            .iter()
            .map(|value| (value.client.as_str(), value.info_hash.to_ascii_lowercase()))
            .collect::<std::collections::HashSet<_>>();
        let links = download_release_link::Entity::find()
            .filter(download_release_link::Column::Client.is_in(clients))
            .filter(download_release_link::Column::InfoHash.is_in(hashes))
            .all(&self.connection)
            .await?
            .into_iter()
            .filter(|link| {
                wanted.contains(&(link.client.as_str(), link.info_hash.to_ascii_lowercase()))
            })
            .collect();
        release_downloads_from_links(links)
    }

    /// Ensure every observed torrent has a durable logical import record. Existing completed,
    /// linked torrents are retained as baseline history and are never cleanup candidates.
    pub async fn sync_import_tasks(&self) -> Result<()> {
        let transaction = self.begin_write().await?;
        let supersessions = import_supersession::Entity::find()
            .all(&transaction)
            .await?;
        let explicit = supersessions
            .iter()
            .map(|value| value.import_task_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let superseded_downloads = supersessions
            .iter()
            .map(|value| {
                (
                    value.source_client.clone(),
                    value.source_info_hash.to_ascii_lowercase(),
                )
            })
            .collect::<std::collections::HashSet<_>>();
        let links = download_release_link::Entity::find()
            .order_by_asc(download_release_link::Column::FirstSeenAt)
            .all(&transaction)
            .await?;
        let mut existing_by_download = import_task::Entity::find()
            .filter(import_task::Column::Client.is_not_null())
            .filter(import_task::Column::InfoHash.is_not_null())
            .all(&transaction)
            .await?
            .into_iter()
            .filter_map(|task| Some(((task.client.clone()?, task.info_hash.clone()?), task)))
            .collect::<HashMap<_, _>>();
        for link in links {
            let existing =
                existing_by_download.remove(&(link.client.clone(), link.info_hash.clone()));
            if existing
                .as_ref()
                .is_some_and(|task| explicit.contains(&task.id))
                || (existing.is_some()
                    && superseded_downloads
                        .contains(&(link.client.clone(), link.info_hash.to_ascii_lowercase())))
            {
                continue;
            }
            let live = link
                .observed_json
                .clone()
                .map(serde_json::from_value::<LiveDownloadStatus>)
                .transpose()?;
            let (state, reason) = import_state_for_link(&link, live.as_ref());
            let now = Utc::now().to_rfc3339();
            if let Some(model) = existing {
                let mut active = model.into_active_model();
                active.release_id = Set(link.release_id.clone());
                active.tracker = Set(link.tracker.clone());
                active.torrent_id = Set(link.torrent_id);
                active.display_name = Set(link
                    .torrent_name
                    .clone()
                    .unwrap_or_else(|| link.info_hash.clone()));
                active.state = Set(import_task_state_name(&state).into());
                active.reason = Set(reason);
                active.error_message = Set(link.error_message.clone());
                active.updated_at = Set(now);
                active.completed_at = Set(matches!(state, ImportTaskState::Complete).then(|| {
                    link.completed_at
                        .clone()
                        .unwrap_or_else(|| Utc::now().to_rfc3339())
                }));
                active.update(&transaction).await?;
            } else {
                let baseline = matches!(state, ImportTaskState::Complete)
                    && link.release_id.is_some()
                    && link.completed_at.is_some();
                import_task::ActiveModel {
                    id: Set(Uuid::new_v4().to_string()),
                    client: Set(Some(link.client)),
                    info_hash: Set(Some(link.info_hash.clone())),
                    download_job_id: Set(None),
                    release_id: Set(link.release_id),
                    tracker: Set(link.tracker),
                    torrent_id: Set(link.torrent_id),
                    display_name: Set(link.torrent_name.unwrap_or(link.info_hash)),
                    state: Set(import_task_state_name(&state).into()),
                    reason: Set(reason),
                    error_message: Set(link.error_message),
                    baseline: Set(baseline),
                    created_at: Set(link.first_seen_at),
                    updated_at: Set(now),
                    completed_at: Set(matches!(state, ImportTaskState::Complete)
                        .then(|| link.completed_at.unwrap_or_else(|| Utc::now().to_rfc3339()))),
                }
                .insert(&transaction)
                .await?;
            }
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn create_replacement_import(
        &self,
        request: CreateReplacementImport<'_>,
    ) -> Result<Uuid> {
        let existing = if let Some(job_id) = request.download_job_id {
            import_task::Entity::find()
                .filter(import_task::Column::DownloadJobId.eq(job_id.to_string()))
                .one(&self.connection)
                .await?
        } else if let (Some(client), Some(info_hash)) =
            (request.target_client, request.target_info_hash)
        {
            import_task::Entity::find()
                .filter(import_task::Column::Client.eq(client))
                .filter(import_task::Column::InfoHash.eq(info_hash.to_ascii_lowercase()))
                .one(&self.connection)
                .await?
        } else {
            None
        };
        let now = Utc::now().to_rfc3339();
        let state = if request.target_complete {
            ImportTaskState::Ready
        } else {
            ImportTaskState::Downloading
        };
        let id = if let Some(model) = existing {
            let id = Uuid::parse_str(&model.id)?;
            let mut active = model.into_active_model();
            active.client = Set(request.target_client.map(str::to_owned));
            active.info_hash = Set(request
                .target_info_hash
                .map(|value| value.to_ascii_lowercase()));
            active.download_job_id = Set(request.download_job_id.map(|value| value.to_string()));
            active.release_id = Set(request.release_id.map(|value| value.to_string()));
            active.tracker = Set(Some(request.tracker.to_ascii_lowercase()));
            active.torrent_id = Set(Some(request.torrent_id));
            active.display_name = Set(request.display_name.into());
            active.state = Set(import_task_state_name(&state).into());
            active.reason = Set(None);
            active.error_message = Set(None);
            active.baseline = Set(false);
            active.updated_at = Set(now.clone());
            active.completed_at = Set(None);
            active.update(&self.connection).await?;
            id
        } else {
            let id = Uuid::new_v4();
            import_task::ActiveModel {
                id: Set(id.to_string()),
                client: Set(request.target_client.map(str::to_owned)),
                info_hash: Set(request
                    .target_info_hash
                    .map(|value| value.to_ascii_lowercase())),
                download_job_id: Set(request.download_job_id.map(|value| value.to_string())),
                release_id: Set(request.release_id.map(|value| value.to_string())),
                tracker: Set(Some(request.tracker.to_ascii_lowercase())),
                torrent_id: Set(Some(request.torrent_id)),
                display_name: Set(request.display_name.into()),
                state: Set(import_task_state_name(&state).into()),
                reason: Set(None),
                error_message: Set(None),
                baseline: Set(false),
                created_at: Set(now.clone()),
                updated_at: Set(now.clone()),
                completed_at: Set(None),
            }
            .insert(&self.connection)
            .await?;
            id
        };
        for source in request.sources {
            let key = (
                id.to_string(),
                source.client.clone(),
                source.info_hash.to_ascii_lowercase(),
            );
            if let Some(model) = import_supersession::Entity::find_by_id(key.clone())
                .one(&self.connection)
                .await?
            {
                let mut active = model.into_active_model();
                active.cleanup_mode = Set(request.cleanup_mode.as_str().into());
                active.cleanup_state = Set("pending".into());
                active.reason = Set(None);
                active.updated_at = Set(now.clone());
                active.update(&self.connection).await?;
            } else {
                import_supersession::ActiveModel {
                    import_task_id: Set(key.0),
                    source_client: Set(key.1),
                    source_info_hash: Set(key.2),
                    tracker: Set(source.tracker.to_ascii_lowercase()),
                    source_name: Set(source.name.clone()),
                    cleanup_mode: Set(request.cleanup_mode.as_str().into()),
                    cleanup_state: Set("pending".into()),
                    reason: Set(None),
                    updated_at: Set(now.clone()),
                }
                .insert(&self.connection)
                .await?;
            }
            import_task::Entity::update_many()
                .col_expr(import_task::Column::State, Expr::value("processing"))
                .col_expr(
                    import_task::Column::Reason,
                    Expr::value(Some(format!("Superseded by replacement import {id}"))),
                )
                .col_expr(import_task::Column::UpdatedAt, Expr::value(now.clone()))
                .filter(import_task::Column::Client.eq(&source.client))
                .filter(import_task::Column::InfoHash.eq(source.info_hash.to_ascii_lowercase()))
                .filter(import_task::Column::Id.ne(id.to_string()))
                .exec(&self.connection)
                .await?;
        }
        Ok(id)
    }

    pub async fn set_superseded_source_states(
        &self,
        import_id: Uuid,
        state: ImportTaskState,
        reason: &str,
    ) -> Result<()> {
        let sources = import_supersession::Entity::find()
            .filter(import_supersession::Column::ImportTaskId.eq(import_id.to_string()))
            .all(&self.connection)
            .await?;
        let now = Utc::now().to_rfc3339();
        for source in sources {
            import_task::Entity::update_many()
                .col_expr(
                    import_task::Column::State,
                    Expr::value(import_task_state_name(&state)),
                )
                .col_expr(
                    import_task::Column::Reason,
                    Expr::value(Some(reason.to_owned())),
                )
                .col_expr(import_task::Column::UpdatedAt, Expr::value(now.clone()))
                .col_expr(
                    import_task::Column::CompletedAt,
                    Expr::value(matches!(state, ImportTaskState::Complete).then(|| now.clone())),
                )
                .filter(import_task::Column::Client.eq(source.source_client))
                .filter(import_task::Column::InfoHash.eq(source.source_info_hash))
                .filter(import_task::Column::Id.ne(import_id.to_string()))
                .exec(&self.connection)
                .await?;
        }
        Ok(())
    }

    pub async fn bind_import_target(&self, id: Uuid, client: &str, info_hash: &str) -> Result<()> {
        if let Some(model) = import_task::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.client = Set(Some(client.into()));
            active.info_hash = Set(Some(info_hash.to_ascii_lowercase()));
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn set_import_state(
        &self,
        id: Uuid,
        state: ImportTaskState,
        reason: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        if let Some(model) = import_task::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.state = Set(import_task_state_name(&state).into());
            active.reason = Set(reason.map(str::to_owned));
            active.error_message = Set(error.map(str::to_owned));
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.completed_at =
                Set(matches!(state, ImportTaskState::Complete).then(|| Utc::now().to_rfc3339()));
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn set_supersession_state(
        &self,
        import_id: Uuid,
        client: &str,
        info_hash: &str,
        state: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        if let Some(model) = import_supersession::Entity::find_by_id((
            import_id.to_string(),
            client.to_owned(),
            info_hash.to_ascii_lowercase(),
        ))
        .one(&self.connection)
        .await?
        {
            let mut active = model.into_active_model();
            active.cleanup_state = Set(state.into());
            active.reason = Set(reason.map(str::to_owned));
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn import_task_models(
        &self,
        id: Uuid,
    ) -> Result<Option<(import_task::Model, Vec<import_supersession::Model>)>> {
        let Some(task) = import_task::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        else {
            return Ok(None);
        };
        let sources = import_supersession::Entity::find()
            .filter(import_supersession::Column::ImportTaskId.eq(id.to_string()))
            .all(&self.connection)
            .await?;
        Ok(Some((task, sources)))
    }

    pub async fn list_imports(&self, limit: u64, offset: u64) -> Result<ImportsPage> {
        let total = import_task::Entity::find().count(&self.connection).await? as i64;
        let models = import_task::Entity::find()
            .order_by_desc(import_task::Column::UpdatedAt)
            .limit(limit.min(500))
            .offset(offset)
            .all(&self.connection)
            .await?;
        let ids = models
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let supersessions = if ids.is_empty() {
            Vec::new()
        } else {
            import_supersession::Entity::find()
                .filter(import_supersession::Column::ImportTaskId.is_in(ids.clone()))
                .all(&self.connection)
                .await?
        };
        let mut supersessions_by_task = HashMap::<String, Vec<_>>::new();
        for value in supersessions {
            supersessions_by_task
                .entry(value.import_task_id.clone())
                .or_default()
                .push(value);
        }
        let mut observed_clients = Vec::new();
        let mut observed_hashes = Vec::new();
        for task in &models {
            if let (Some(client), Some(info_hash)) = (&task.client, &task.info_hash) {
                observed_clients.push(client.clone());
                observed_hashes.push(info_hash.clone());
            }
        }
        for sources in supersessions_by_task.values() {
            for source in sources {
                observed_clients.push(source.source_client.clone());
                observed_hashes.push(source.source_info_hash.clone());
            }
        }
        observed_clients.sort();
        observed_clients.dedup();
        observed_hashes.sort();
        observed_hashes.dedup();
        let mut observed_by_ref = if observed_clients.is_empty() || observed_hashes.is_empty() {
            HashMap::new()
        } else {
            download_release_link::Entity::find()
                .filter(download_release_link::Column::Client.is_in(observed_clients))
                .filter(download_release_link::Column::InfoHash.is_in(observed_hashes))
                .filter(download_release_link::Column::ObservedJson.is_not_null())
                .all(&self.connection)
                .await?
                .into_iter()
                .filter_map(|link| {
                    let live = serde_json::from_value(link.observed_json?).ok()?;
                    Some(((link.client, link.info_hash), live))
                })
                .collect::<HashMap<_, LiveDownloadStatus>>()
        };
        let release_ids = models
            .iter()
            .filter_map(|task| task.release_id.as_deref())
            .filter_map(|value| Uuid::parse_str(value).ok())
            .collect::<Vec<_>>();
        let release_details = self.get_release_details(&release_ids).await?;
        let mut items = Vec::with_capacity(models.len());
        for task in models {
            let download = task.client.as_ref().zip(task.info_hash.as_ref()).and_then(
                |(client, info_hash)| {
                    observed_by_ref
                        .get(&(client.clone(), info_hash.clone()))
                        .cloned()
                },
            );
            let mut task_supersessions = Vec::new();
            for source in supersessions_by_task.remove(&task.id).unwrap_or_default() {
                let source_download = observed_by_ref.remove(&(
                    source.source_client.clone(),
                    source.source_info_hash.clone(),
                ));
                task_supersessions.push(ImportSupersession {
                    source_client: source.source_client,
                    source_info_hash: source.source_info_hash,
                    tracker: source.tracker,
                    source_name: source.source_name,
                    cleanup_mode: parse_cleanup_mode(&source.cleanup_mode)?,
                    cleanup_state: source.cleanup_state,
                    reason: source.reason,
                    download: source_download,
                });
            }
            let release = task
                .release_id
                .as_deref()
                .and_then(|value| Uuid::parse_str(value).ok())
                .and_then(|id| release_details.get(&id))
                .map(|detail| detail.release.clone());
            items.push(ImportTask {
                id: Uuid::parse_str(&task.id)?,
                state: parse_import_task_state(&task.state)?,
                display_name: task.display_name,
                client: task.client,
                info_hash: task.info_hash,
                download_job_id: task
                    .download_job_id
                    .as_deref()
                    .map(Uuid::parse_str)
                    .transpose()?,
                tracker: task.tracker,
                torrent_id: task.torrent_id,
                release,
                reason: task.reason,
                error: task.error_message,
                baseline: task.baseline,
                download,
                supersessions: task_supersessions,
                created_at: parse_timestamp(&task.created_at)?,
                updated_at: parse_timestamp(&task.updated_at)?,
                completed_at: task
                    .completed_at
                    .as_deref()
                    .map(parse_timestamp)
                    .transpose()?,
            });
        }
        let review = import_task::Entity::find()
            .filter(import_task::Column::State.is_in(["needs_review", "blocked", "failed"]))
            .count(&self.connection)
            .await? as i64;
        let complete = import_task::Entity::find()
            .filter(import_task::Column::State.is_in(["complete", "dismissed"]))
            .count(&self.connection)
            .await? as i64;
        Ok(ImportsPage {
            items,
            total,
            counts: ImportTaskCounts {
                active: total.saturating_sub(review).saturating_sub(complete),
                review,
                complete,
            },
        })
    }

    pub async fn enqueue_track_index(&self, tracker: &str, group_id: i64) -> Result<()> {
        self.enqueue_track_index_with_priority(tracker, group_id, 10)
            .await
    }

    pub async fn enqueue_track_index_with_priority(
        &self,
        tracker: &str,
        group_id: i64,
        priority: i64,
    ) -> Result<()> {
        let transaction = self.begin_write().await?;
        self.ensure_track_index_on(&transaction, tracker, group_id, priority)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn ensure_track_index_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        tracker: &str,
        group_id: i64,
        priority: i64,
    ) -> Result<()> {
        let now = Utc::now();
        let mut should_enqueue = false;
        let mut refresh_completed = false;
        if let Some(model) = release_track_index::Entity::find_by_id((tracker.to_owned(), group_id))
            .one(connection)
            .await?
        {
            let current_priority = model.priority;
            let current_state = model.state.clone();
            let expired = model
                .expires_at
                .as_deref()
                .map(parse_timestamp)
                .transpose()?
                .is_some_and(|expires_at| expires_at <= now);
            let mut active = model.into_active_model();
            active.priority = Set(current_priority.max(priority));
            if expired {
                active.state = Set("pending".into());
                active.next_retry_at = Set(None);
                active.updated_at = Set(now.to_rfc3339());
                should_enqueue = true;
                refresh_completed = current_state == "indexed";
            }
            should_enqueue |= current_state != "indexed";
            active.update(connection).await?;
        } else {
            should_enqueue = true;
            release_track_index::ActiveModel {
                tracker: Set(tracker.into()),
                group_id: Set(group_id),
                state: Set("pending".into()),
                index_json: Set(None),
                attempts: Set(0),
                next_retry_at: Set(None),
                error_message: Set(None),
                fetched_at: Set(None),
                expires_at: Set(None),
                updated_at: Set(now.to_rfc3339()),
                priority: Set(priority),
            }
            .insert(connection)
            .await?;
        }
        if should_enqueue {
            let key = format!("track-index:{}:{group_id}:v2", tracker.to_ascii_lowercase());
            if refresh_completed {
                self.reactivate_completed_background_job_on(connection, &key)
                    .await?;
            }
            self.enqueue_background_job_on(
                connection,
                EnqueueBackgroundJob {
                    deduplication_key: &key,
                    kind: "index_tracklist",
                    payload: serde_json::json!({ "tracker": tracker, "groupId": group_id }),
                    provider_id: Some(format!("tracker:{tracker}")),
                    lane: "sync",
                    priority,
                    max_attempts: 12,
                    next_run_at: None,
                    parent_id: None,
                    recurring_interval_seconds: None,
                },
            )
            .await?;
        }
        Ok(())
    }

    pub async fn ensure_single_coverage(&self, tracker: &str, group_id: i64) -> Result<()> {
        let transaction = self.begin_write().await?;
        self.ensure_single_coverage_on(&transaction, tracker, group_id)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn ensure_single_coverage_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        tracker: &str,
        group_id: i64,
    ) -> Result<()> {
        let existing = single_album_coverage::Entity::find_by_id((tracker.to_owned(), group_id))
            .one(connection)
            .await?;
        let should_enqueue = existing.as_ref().is_none_or(|model| model.state != "ready");
        if existing.is_none() {
            single_album_coverage::ActiveModel {
                tracker: Set(tracker.into()),
                single_group_id: Set(group_id),
                state: Set("pending".into()),
                coverage_json: Set(None),
                updated_at: Set(Utc::now().to_rfc3339()),
            }
            .insert(connection)
            .await?;
        }
        if should_enqueue {
            let key = format!(
                "single-coverage:{}:{group_id}:v2",
                tracker.to_ascii_lowercase()
            );
            self.enqueue_background_job_on(
                connection,
                EnqueueBackgroundJob {
                    deduplication_key: &key,
                    kind: "compute_single_coverage",
                    payload: serde_json::json!({ "tracker": tracker, "groupId": group_id }),
                    provider_id: None,
                    lane: "sync",
                    priority: 5,
                    max_attempts: 20,
                    next_run_at: None,
                    parent_id: None,
                    recurring_interval_seconds: None,
                },
            )
            .await?;
        }
        Ok(())
    }

    pub async fn seed_single_deduplications(&self, singles: &[(String, i64)]) -> Result<()> {
        if singles.is_empty() {
            return Ok(());
        }
        let mut singles = singles.to_vec();
        singles.sort_unstable();
        singles.dedup();
        let transaction = self.begin_write().await?;
        for (tracker, group_id) in singles {
            self.ensure_track_index_on(&transaction, &tracker, group_id, 10)
                .await?;
            self.ensure_single_coverage_on(&transaction, &tracker, group_id)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn ensure_artist_catalog_refreshes(&self, artists: &[(String, i64)]) -> Result<()> {
        if artists.is_empty() {
            return Ok(());
        }
        let mut artists = artists.to_vec();
        artists.sort_unstable();
        artists.dedup();
        let keys = artists
            .iter()
            .map(|(tracker, artist_id)| {
                format!(
                    "refresh-artist-catalog:{}:{artist_id}:v1",
                    tracker.to_ascii_lowercase()
                )
            })
            .collect::<Vec<_>>();
        let transaction = self.begin_write().await?;
        let existing = background_job::Entity::find()
            .select_only()
            .column(background_job::Column::DeduplicationKey)
            .filter(background_job::Column::DeduplicationKey.is_in(keys))
            .into_tuple::<String>()
            .all(&transaction)
            .await?
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        for (tracker, artist_id) in artists {
            let key = format!(
                "refresh-artist-catalog:{}:{artist_id}:v1",
                tracker.to_ascii_lowercase()
            );
            if existing.contains(&key) {
                continue;
            }
            self.enqueue_background_job_on(
                &transaction,
                EnqueueBackgroundJob {
                    deduplication_key: &key,
                    kind: "refresh_artist_catalog",
                    payload: serde_json::json!({
                        "tracker": tracker,
                        "artistId": artist_id,
                        "interactive": false,
                    }),
                    provider_id: Some(format!("tracker:{tracker}")),
                    lane: "sync",
                    priority: 5,
                    max_attempts: 8,
                    next_run_at: None,
                    parent_id: None,
                    recurring_interval_seconds: None,
                },
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn due_track_indexes(&self, limit: i64) -> Result<Vec<TrackIndexJob>> {
        let now = Utc::now().to_rfc3339();
        let models = release_track_index::Entity::find()
            .filter(release_track_index::Column::State.eq("pending"))
            .filter(
                Condition::any()
                    .add(release_track_index::Column::NextRetryAt.is_null())
                    .add(release_track_index::Column::NextRetryAt.lte(now)),
            )
            .order_by_desc(release_track_index::Column::Priority)
            .order_by_asc(release_track_index::Column::UpdatedAt)
            .limit(limit.max(0) as u64)
            .all(&self.connection)
            .await?;
        Ok(models
            .into_iter()
            .map(|model| TrackIndexJob {
                tracker: model.tracker,
                group_id: model.group_id,
            })
            .collect())
    }

    pub async fn set_track_index_resolving(&self, tracker: &str, group_id: i64) -> Result<()> {
        if let Some(model) = release_track_index::Entity::find_by_id((tracker.to_owned(), group_id))
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.state = Set("resolving".into());
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn put_track_index(&self, index: &ReleaseTrackIndex) -> Result<()> {
        self.put_track_index_with_parent(index, None).await
    }

    pub async fn put_track_index_with_parent(
        &self,
        index: &ReleaseTrackIndex,
        parent_id: Option<Uuid>,
    ) -> Result<()> {
        let transaction = self.begin_write().await?;
        let index_json = serde_json::to_value(index)?;
        let mut changed = false;
        if let Some(model) =
            release_track_index::Entity::find_by_id((index.tracker.clone(), index.group_id))
                .one(&transaction)
                .await?
        {
            changed = model.index_json.as_ref() != Some(&index_json);
            let now = Utc::now();
            let mut active = model.into_active_model();
            active.state = Set("indexed".into());
            active.index_json = Set(Some(index_json));
            active.attempts = Set(0);
            active.next_retry_at = Set(None);
            active.error_message = Set(None);
            active.fetched_at = Set(Some(now.to_rfc3339()));
            active.expires_at = Set(Some((now + chrono::Duration::hours(24)).to_rfc3339()));
            active.updated_at = Set(now.to_rfc3339());
            active.update(&transaction).await?;
        }
        transaction.commit().await?;
        let mut coverage_groups = vec![index.group_id];
        if changed {
            let affected = self
                .affected_single_groups_on(&self.connection, &index.tracker, index.group_id)
                .await?;
            for group_id in &affected {
                self.invalidate_single_coverage(&index.tracker, *group_id, parent_id)
                    .await?;
            }
            coverage_groups.extend(affected);
        }
        coverage_groups.sort_unstable();
        coverage_groups.dedup();
        self.resume_waiting_single_coverages(&index.tracker, &coverage_groups)
            .await?;
        Ok(())
    }

    async fn invalidate_single_coverage(
        &self,
        tracker: &str,
        group_id: i64,
        parent_id: Option<Uuid>,
    ) -> Result<()> {
        let transaction = self.begin_write().await?;
        let now = Utc::now().to_rfc3339();
        if let Some(model) =
            single_album_coverage::Entity::find_by_id((tracker.to_owned(), group_id))
                .one(&transaction)
                .await?
        {
            let mut active = model.into_active_model();
            active.state = Set("pending".into());
            active.updated_at = Set(now.clone());
            active.update(&transaction).await?;
        } else {
            single_album_coverage::ActiveModel {
                tracker: Set(tracker.into()),
                single_group_id: Set(group_id),
                state: Set("pending".into()),
                coverage_json: Set(None),
                updated_at: Set(now),
            }
            .insert(&transaction)
            .await?;
        }
        let key = format!(
            "single-coverage:{}:{group_id}:v2",
            tracker.to_ascii_lowercase()
        );
        self.reactivate_completed_background_job_on(&transaction, &key)
            .await?;
        self.enqueue_background_job_on(
            &transaction,
            EnqueueBackgroundJob {
                deduplication_key: &key,
                kind: "compute_single_coverage",
                payload: serde_json::json!({ "tracker": tracker, "groupId": group_id }),
                provider_id: None,
                lane: "sync",
                priority: 5,
                max_attempts: 20,
                next_run_at: None,
                parent_id,
                recurring_interval_seconds: None,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn fail_track_index(
        &self,
        tracker: &str,
        group_id: i64,
        message: &str,
    ) -> Result<()> {
        if let Some(model) = release_track_index::Entity::find_by_id((tracker.to_owned(), group_id))
            .one(&self.connection)
            .await?
        {
            let attempts = model.attempts;
            let delay = chrono::Duration::seconds((30_i64 * (1_i64 << attempts.min(7))).min(3600));
            let mut active = model.into_active_model();
            active.state = Set("failed".into());
            active.attempts = Set(attempts + 1);
            active.next_retry_at = Set(Some((Utc::now() + delay).to_rfc3339()));
            active.error_message = Set(Some(message.chars().take(500).collect()));
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn recover_track_indexes(&self) -> Result<()> {
        let models = release_track_index::Entity::find()
            .filter(release_track_index::Column::State.eq("resolving"))
            .all(&self.connection)
            .await?;
        for model in models {
            let mut active = model.into_active_model();
            active.state = Set("pending".into());
            active.next_retry_at = Set(None);
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn replace_catalog_memberships(
        &self,
        tracker: &str,
        catalog: &ArtistCatalogPage,
    ) -> Result<()> {
        let transaction = self.begin_write().await?;
        let mut old_groups = std::collections::HashMap::new();
        let mut old_singles = std::collections::HashSet::new();
        for model in dedupe_catalog_membership::Entity::find()
            .filter(dedupe_catalog_membership::Column::Tracker.eq(tracker))
            .filter(dedupe_catalog_membership::Column::ArtistId.eq(catalog.artist.artist_id))
            .all(&transaction)
            .await?
        {
            old_groups.insert(model.group_id, model.group_json.clone());
            let group: crate::model::ArtistCatalogRelease =
                serde_json::from_value(model.group_json)?;
            if group
                .release
                .release_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
            {
                old_singles.insert(group.release.group_id);
            }
        }
        dedupe_catalog_membership::Entity::delete_many()
            .filter(dedupe_catalog_membership::Column::Tracker.eq(tracker))
            .filter(dedupe_catalog_membership::Column::ArtistId.eq(catalog.artist.artist_id))
            .exec(&transaction)
            .await?;
        let now = Utc::now().to_rfc3339();
        let mut seen_groups = std::collections::HashSet::new();
        let mut new_groups = std::collections::HashMap::new();
        let mut new_singles = std::collections::HashSet::new();
        for group in &catalog.groups {
            if !seen_groups.insert(group.release.group_id) {
                continue;
            }
            let group_json = serde_json::to_value(group)?;
            new_groups.insert(group.release.group_id, group_json.clone());
            dedupe_catalog_membership::ActiveModel {
                tracker: Set(tracker.into()),
                artist_id: Set(catalog.artist.artist_id),
                group_id: Set(group.release.group_id),
                group_json: Set(group_json),
                updated_at: Set(now.clone()),
            }
            .insert(&transaction)
            .await?;
            let release_type = group.release.release_type.as_deref();
            let is_album = release_type.is_some_and(|kind| kind.eq_ignore_ascii_case("album"));
            let is_single = release_type.is_some_and(|kind| kind.eq_ignore_ascii_case("single"));
            if group
                .roles
                .contains(&crate::model::ArtistCatalogRole::Primary)
                && group.listed_on_tracker
                && !group.variants.is_empty()
                && (is_album || is_single)
            {
                self.ensure_track_index_on(
                    &transaction,
                    tracker,
                    group.release.group_id,
                    if is_album { 20 } else { 0 },
                )
                .await?;
            }
            if is_single {
                new_singles.insert(group.release.group_id);
            }
        }
        let mut dirty_singles = std::collections::HashSet::new();
        if old_groups != new_groups {
            dirty_singles.extend(old_singles);
            dirty_singles.extend(new_singles);
        }
        let dirty_singles = dirty_singles.into_iter().collect::<Vec<_>>();
        for group_id in &dirty_singles {
            if single_album_coverage::Entity::find_by_id((tracker.to_owned(), *group_id))
                .one(&transaction)
                .await?
                .is_none()
            {
                single_album_coverage::ActiveModel {
                    tracker: Set(tracker.into()),
                    single_group_id: Set(*group_id),
                    state: Set("pending".into()),
                    coverage_json: Set(None),
                    updated_at: Set(now.clone()),
                }
                .insert(&transaction)
                .await?;
            } else if let Some(model) =
                single_album_coverage::Entity::find_by_id((tracker.to_owned(), *group_id))
                    .one(&transaction)
                    .await?
            {
                let mut active = model.into_active_model();
                active.state = Set("pending".into());
                active.updated_at = Set(now.clone());
                active.update(&transaction).await?;
            }
            let key = format!(
                "single-coverage:{}:{group_id}:v2",
                tracker.to_ascii_lowercase()
            );
            self.reactivate_completed_background_job_on(&transaction, &key)
                .await?;
            self.enqueue_background_job_on(
                &transaction,
                EnqueueBackgroundJob {
                    deduplication_key: &key,
                    kind: "compute_single_coverage",
                    payload: serde_json::json!({ "tracker": tracker, "groupId": *group_id }),
                    provider_id: None,
                    lane: "sync",
                    priority: 5,
                    max_attempts: 20,
                    next_run_at: None,
                    parent_id: None,
                    recurring_interval_seconds: None,
                },
            )
            .await?;
        }
        transaction.commit().await?;
        self.resume_waiting_single_coverages(tracker, &dirty_singles)
            .await?;
        Ok(())
    }

    pub async fn list_catalog_memberships(&self) -> Result<Vec<CatalogMembership>> {
        dedupe_catalog_membership::Entity::find()
            .all(&self.connection)
            .await?
            .into_iter()
            .map(|model| {
                Ok(CatalogMembership {
                    artist_id: model.artist_id,
                    group: serde_json::from_value(model.group_json)?,
                })
            })
            .collect()
    }

    async fn affected_single_groups_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        tracker: &str,
        changed_group_id: i64,
    ) -> Result<Vec<i64>> {
        let artist_ids = dedupe_catalog_membership::Entity::find()
            .filter(dedupe_catalog_membership::Column::Tracker.eq(tracker))
            .filter(dedupe_catalog_membership::Column::GroupId.eq(changed_group_id))
            .all(connection)
            .await?
            .into_iter()
            .map(|model| model.artist_id)
            .collect::<std::collections::HashSet<_>>();
        if artist_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut singles = std::collections::HashSet::new();
        for model in dedupe_catalog_membership::Entity::find()
            .filter(dedupe_catalog_membership::Column::Tracker.eq(tracker))
            .filter(dedupe_catalog_membership::Column::ArtistId.is_in(artist_ids))
            .all(connection)
            .await?
        {
            let group: crate::model::ArtistCatalogRelease =
                serde_json::from_value(model.group_json)?;
            if group
                .release
                .release_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
            {
                singles.insert(group.release.group_id);
            }
        }
        Ok(singles.into_iter().collect())
    }

    pub async fn list_track_indexes(&self) -> Result<Vec<StoredTrackIndex>> {
        release_track_index::Entity::find()
            .all(&self.connection)
            .await?
            .into_iter()
            .map(|model| {
                Ok(StoredTrackIndex {
                    tracker: model.tracker,
                    group_id: model.group_id,
                    state: model.state,
                    index: model.index_json.map(serde_json::from_value).transpose()?,
                })
            })
            .collect()
    }

    pub async fn track_index_progress(&self) -> Result<TrackIndexProgress> {
        let count = |state: &'static str| {
            release_track_index::Entity::find()
                .filter(release_track_index::Column::State.eq(state))
                .count(&self.connection)
        };
        let (indexed, pending, resolving, failed) = tokio::try_join!(
            count("indexed"),
            count("pending"),
            count("resolving"),
            count("failed"),
        )?;
        Ok(TrackIndexProgress {
            indexed: indexed as usize,
            pending: pending as usize,
            resolving: resolving as usize,
            failed: failed as usize,
        })
    }

    pub async fn pending_single_coverages(&self) -> Result<Vec<(String, i64)>> {
        Ok(single_album_coverage::Entity::find()
            .filter(single_album_coverage::Column::State.is_in(["pending", "failed"]))
            .all(&self.connection)
            .await?
            .into_iter()
            .map(|model| (model.tracker, model.single_group_id))
            .collect())
    }

    pub async fn put_single_coverage(
        &self,
        tracker: &str,
        group_id: i64,
        state: &str,
        coverage: Option<&RawSingleCoverage>,
    ) -> Result<()> {
        let coverage_json = coverage.map(serde_json::to_value).transpose()?;
        if let Some(model) =
            single_album_coverage::Entity::find_by_id((tracker.to_owned(), group_id))
                .one(&self.connection)
                .await?
        {
            let mut active = model.into_active_model();
            active.state = Set(state.into());
            active.coverage_json = Set(coverage_json);
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        } else {
            single_album_coverage::ActiveModel {
                tracker: Set(tracker.into()),
                single_group_id: Set(group_id),
                state: Set(state.into()),
                coverage_json: Set(coverage_json),
                updated_at: Set(Utc::now().to_rfc3339()),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    pub async fn get_single_coverage(
        &self,
        tracker: &str,
        group_id: i64,
    ) -> Result<Option<StoredCoverage>> {
        single_album_coverage::Entity::find_by_id((tracker.to_owned(), group_id))
            .one(&self.connection)
            .await?
            .map(|model| {
                Ok(StoredCoverage {
                    state: model.state,
                    coverage: model
                        .coverage_json
                        .map(serde_json::from_value)
                        .transpose()?,
                })
            })
            .transpose()
    }

    pub async fn get_single_coverages(
        &self,
        releases: &[(String, i64)],
    ) -> Result<HashMap<(String, i64), StoredCoverage>> {
        let mut coverages = HashMap::new();
        for chunk in releases.chunks(300) {
            if chunk.is_empty() {
                continue;
            }
            let condition = chunk
                .iter()
                .fold(Condition::any(), |condition, (tracker, id)| {
                    condition.add(
                        Condition::all()
                            .add(single_album_coverage::Column::Tracker.eq(tracker.clone()))
                            .add(single_album_coverage::Column::SingleGroupId.eq(*id)),
                    )
                });
            for model in single_album_coverage::Entity::find()
                .filter(condition)
                .all(&self.connection)
                .await?
            {
                coverages.insert(
                    (model.tracker, model.single_group_id),
                    StoredCoverage {
                        state: model.state,
                        coverage: model
                            .coverage_json
                            .map(serde_json::from_value)
                            .transpose()?,
                    },
                );
            }
        }
        Ok(coverages)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn put_snapshot<T: Serialize>(
        &self,
        tracker: &str,
        kind: &str,
        key: &str,
        value: &T,
        raw: &Value,
        fetched_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        let existing = tracker_snapshot::Entity::find()
            .filter(tracker_snapshot::Column::Tracker.eq(tracker))
            .filter(tracker_snapshot::Column::ResourceKind.eq(kind))
            .filter(tracker_snapshot::Column::ResourceKey.eq(key))
            .one(&self.connection)
            .await?;
        if let Some(model) = existing {
            let mut active = model.into_active_model();
            active.normalized_json = Set(serde_json::to_value(value)?);
            active.sanitized_raw_json = Set(raw.clone());
            active.fetched_at = Set(fetched_at.to_rfc3339());
            active.expires_at = Set(expires_at.to_rfc3339());
            active.schema_version = Set(1);
            active.update(&self.connection).await?;
        } else {
            tracker_snapshot::ActiveModel {
                id: Default::default(),
                tracker: Set(tracker.into()),
                resource_kind: Set(kind.into()),
                resource_key: Set(key.into()),
                normalized_json: Set(serde_json::to_value(value)?),
                sanitized_raw_json: Set(raw.clone()),
                fetched_at: Set(fetched_at.to_rfc3339()),
                expires_at: Set(expires_at.to_rfc3339()),
                schema_version: Set(1),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    pub async fn create_job(
        &self,
        tracker: &str,
        torrent_id: i64,
        profile: &str,
        use_token: bool,
        idempotency_key: Option<&str>,
    ) -> Result<(DownloadJob, bool)> {
        let transaction = self.begin_write().await?;
        if let Some(idempotency_key) = idempotency_key
            && let Some(model) = download_job::Entity::find()
                .filter(download_job::Column::IdempotencyKey.eq(idempotency_key))
                .one(&transaction)
                .await?
        {
            let existing = job_from_model(model)?;
            if matches!(
                existing.state,
                DownloadState::Queued | DownloadState::FetchingMetadata | DownloadState::Submitting
            ) {
                self.ensure_download_submission_on(&transaction, &existing, false)
                    .await?;
            }
            transaction.commit().await?;
            return Ok((existing, false));
        }
        if let Some(model) = download_job::Entity::find()
            .filter(download_job::Column::Tracker.eq(tracker))
            .filter(download_job::Column::TorrentId.eq(torrent_id))
            .filter(download_job::Column::Profile.eq(profile))
            .one(&transaction)
            .await?
        {
            let existing = job_from_model(model.clone())?;
            if existing.state == DownloadState::Failed {
                let now = Utc::now();
                let mut active = model.into_active_model();
                active.idempotency_key = Set(idempotency_key.map(str::to_owned));
                active.use_token = Set(use_token);
                active.group_id = Set(None);
                active.info_hash = Set(None);
                active.name = Set(None);
                active.state = Set("queued".into());
                active.progress = Set(0.0);
                active.download_speed = Set(0);
                active.upload_speed = Set(0);
                active.eta = Set(None);
                active.error_code = Set(None);
                active.error_message = Set(None);
                active.updated_at = Set(now.to_rfc3339());
                active.update(&transaction).await?;
                self.add_event_on(&transaction, existing.id, &DownloadState::Queued, None)
                    .await?;
                let job = DownloadJob {
                    use_token,
                    group_id: None,
                    info_hash: None,
                    name: None,
                    state: DownloadState::Queued,
                    progress: 0.0,
                    download_speed: 0,
                    upload_speed: 0,
                    eta: None,
                    error_code: None,
                    error_message: None,
                    updated_at: now,
                    ..existing
                };
                self.ensure_download_submission_on(&transaction, &job, true)
                    .await?;
                transaction.commit().await?;
                return Ok((job, true));
            }
            if matches!(
                existing.state,
                DownloadState::Queued | DownloadState::FetchingMetadata | DownloadState::Submitting
            ) {
                self.ensure_download_submission_on(&transaction, &existing, false)
                    .await?;
            }
            transaction.commit().await?;
            return Ok((existing, false));
        }
        let now = Utc::now();
        let job = DownloadJob {
            id: Uuid::new_v4(),
            tracker: tracker.into(),
            torrent_id,
            group_id: None,
            profile: profile.into(),
            use_token,
            info_hash: None,
            name: None,
            state: DownloadState::Queued,
            progress: 0.0,
            download_speed: 0,
            upload_speed: 0,
            eta: None,
            error_code: None,
            error_message: None,
            created_at: now,
            updated_at: now,
        };
        download_job::ActiveModel {
            id: Set(job.id.to_string()),
            idempotency_key: Set(idempotency_key.map(str::to_owned)),
            tracker: Set(tracker.into()),
            torrent_id: Set(torrent_id),
            group_id: Set(None),
            profile: Set(profile.into()),
            use_token: Set(use_token),
            info_hash: Set(None),
            name: Set(None),
            state: Set(job.state.as_str().into()),
            progress: Set(0.0),
            download_speed: Set(0),
            upload_speed: Set(0),
            eta: Set(None),
            error_code: Set(None),
            error_message: Set(None),
            created_at: Set(now.to_rfc3339()),
            updated_at: Set(now.to_rfc3339()),
        }
        .insert(&transaction)
        .await?;
        self.add_event_on(&transaction, job.id, &job.state, None)
            .await?;
        self.ensure_download_submission_on(&transaction, &job, true)
            .await?;
        transaction.commit().await?;
        Ok((job, true))
    }

    async fn ensure_download_submission_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        job: &DownloadJob,
        rearm: bool,
    ) -> Result<Uuid> {
        let key = format!("submit-download:{}:v1", job.id);
        if let Some(model) = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(&key))
            .one(connection)
            .await?
        {
            let id = Uuid::parse_str(&model.id)?;
            let should_rearm =
                rearm || matches!(model.state.as_str(), "completed" | "failed" | "cancelled");
            let mut active = model.into_active_model();
            active.provider_id = Set(Some(format!("tracker:{}", job.tracker)));
            active.lane = Set("download".into());
            active.priority = Set(100);
            active.max_attempts = Set(20);
            active.updated_at = Set(Utc::now().to_rfc3339());
            if should_rearm {
                active.state = Set("pending".into());
                active.attempts = Set(0);
                active.deferrals = Set(0);
                active.next_run_at = Set(None);
                active.lease_owner = Set(None);
                active.lease_until = Set(None);
                active.last_error_code = Set(None);
                active.last_error_message = Set(None);
                active.finished_at = Set(None);
                active.cancelled_at = Set(None);
            }
            active.update(connection).await?;
            return Ok(id);
        }
        self.enqueue_background_job_on(
            connection,
            EnqueueBackgroundJob {
                deduplication_key: &key,
                kind: "submit_download",
                payload: serde_json::json!({ "jobId": job.id }),
                provider_id: Some(format!("tracker:{}", job.tracker)),
                lane: "download",
                priority: 100,
                max_attempts: 20,
                next_run_at: None,
                parent_id: None,
                recurring_interval_seconds: None,
            },
        )
        .await
    }

    pub async fn ensure_incomplete_download_submissions(&self) -> Result<u64> {
        let transaction = self.begin_write().await?;
        let models = download_job::Entity::find()
            .filter(download_job::Column::State.is_in([
                "queued",
                "fetching_metadata",
                "submitting",
            ]))
            .all(&transaction)
            .await?;
        let count = models.len() as u64;
        for model in models {
            let job = job_from_model(model)?;
            self.ensure_download_submission_on(&transaction, &job, false)
                .await?;
        }
        transaction.commit().await?;
        Ok(count)
    }

    pub async fn update_job_metadata(
        &self,
        id: Uuid,
        group_id: Option<i64>,
        info_hash: &str,
        name: &str,
    ) -> Result<()> {
        if let Some(model) = download_job::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.group_id = Set(group_id);
            active.info_hash = Set(Some(info_hash.into()));
            active.name = Set(Some(name.into()));
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn set_job_state(
        &self,
        id: Uuid,
        state: DownloadState,
        error: Option<(&str, &str)>,
    ) -> Result<()> {
        if let Some(model) = download_job::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.state = Set(state.as_str().into());
            active.error_code = Set(error.map(|(code, _)| code.to_owned()));
            active.error_message = Set(error.map(|(_, message)| message.to_owned()));
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        self.add_event(id, &state, error.map(|(_, message)| message))
            .await?;
        Ok(())
    }

    pub async fn update_progress(
        &self,
        id: Uuid,
        state: DownloadState,
        progress: f64,
        download_speed: i64,
        upload_speed: i64,
        eta: Option<i64>,
    ) -> Result<()> {
        if let Some(model) = download_job::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.state = Set(state.as_str().into());
            active.progress = Set(progress);
            active.download_speed = Set(download_speed);
            active.upload_speed = Set(upload_speed);
            active.eta = Set(eta);
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn list_jobs(&self) -> Result<Vec<DownloadJob>> {
        download_job::Entity::find()
            .order_by_desc(download_job::Column::CreatedAt)
            .all(&self.connection)
            .await?
            .into_iter()
            .map(job_from_model)
            .collect()
    }

    pub async fn get_job(&self, id: Uuid) -> Result<Option<DownloadJob>> {
        download_job::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
            .map(job_from_model)
            .transpose()
    }

    pub async fn put_canonical(
        &self,
        canonical: &CanonicalTorrent,
        fetched_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        let mut canonical = canonical.clone();
        let existing_torrent = canonical_torrent::Entity::find_by_id((
            canonical.release.tracker.clone(),
            canonical.variant.torrent_id,
        ))
        .one(&self.connection)
        .await?;
        if let Some(model) = &existing_torrent {
            let previous: CanonicalTorrent = serde_json::from_value(model.canonical_json.clone())?;
            if canonical.variant.info_hash.is_none() {
                canonical.variant.info_hash = previous.variant.info_hash;
            }
            if !canonical.variant.token_eligibility_known
                && previous.variant.token_eligibility_known
            {
                canonical.variant.can_use_token = previous.variant.can_use_token;
                canonical.variant.token_eligibility_known = true;
            }
            if canonical.release.artist.is_none() {
                canonical.release.artist = previous.release.artist;
            }
            if canonical.release.artwork.is_none() {
                canonical.release.artwork = previous.release.artwork;
            }
            if canonical.release.release_type.is_none() {
                canonical.release.release_type = previous.release.release_type;
            }
            if canonical.release.year.is_none() {
                canonical.release.year = previous.release.year;
            }
            if canonical.description.is_none() {
                canonical.description = previous.description;
            }
            if canonical.record_label.is_none() {
                canonical.record_label = previous.record_label;
            }
        }
        let incoming_is_structured = canonical
            .release
            .artists
            .iter()
            .any(|artist| artist.source == ArtistCreditSource::Structured);
        if !incoming_is_structured {
            let previous_model = match existing_torrent.as_ref() {
                Some(model) => Some(model.clone()),
                None => {
                    canonical_torrent::Entity::find()
                        .filter(canonical_torrent::Column::Tracker.eq(&canonical.release.tracker))
                        .filter(canonical_torrent::Column::GroupId.eq(canonical.release.group_id))
                        .one(&self.connection)
                        .await?
                }
            };
            if let Some(model) = previous_model {
                let previous: CanonicalTorrent = serde_json::from_value(model.canonical_json)?;
                if previous
                    .release
                    .artists
                    .iter()
                    .any(|artist| artist.source == ArtistCreditSource::Structured)
                {
                    canonical.release.artists = previous.release.artists;
                    if canonical.release.artist.is_none() {
                        canonical.release.artist = previous.release.artist;
                    }
                }
            }
        }

        let release_id = self
            .ensure_release_identity(&mut canonical.release, fetched_at, expires_at)
            .await?;
        let canonical_json = serde_json::to_value(&canonical)?;
        if let Some(model) = existing_torrent {
            let mut active = model.into_active_model();
            active.group_id = Set(canonical.release.group_id);
            active.release_id = Set(Some(release_id.to_string()));
            active.info_hash = Set(canonical.variant.info_hash.clone());
            active.canonical_json = Set(canonical_json);
            active.fetched_at = Set(fetched_at.to_rfc3339());
            active.expires_at = Set(expires_at.to_rfc3339());
            active.update(&self.connection).await?;
        } else {
            canonical_torrent::ActiveModel {
                tracker: Set(canonical.release.tracker.clone()),
                torrent_id: Set(canonical.variant.torrent_id),
                group_id: Set(canonical.release.group_id),
                release_id: Set(Some(release_id.to_string())),
                info_hash: Set(canonical.variant.info_hash.clone()),
                canonical_json: Set(canonical_json),
                fetched_at: Set(fetched_at.to_rfc3339()),
                expires_at: Set(expires_at.to_rfc3339()),
            }
            .insert(&self.connection)
            .await?;
        }
        self.replace_release_artists(&canonical).await?;
        Ok(())
    }

    async fn ensure_release_identity(
        &self,
        release: &mut ReleaseSummary,
        fetched_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Uuid> {
        let tracker = release.tracker.to_ascii_lowercase();
        let existing = release_source::Entity::find_by_id((tracker.clone(), release.group_id))
            .one(&self.connection)
            .await?;
        let mut review_match = None;
        let release_id = if let Some(existing) = &existing {
            Uuid::parse_str(&existing.release_id)?
        } else {
            let normalized_title = crate::release_matcher::normalized(&release.title);
            let candidates = release_source::Entity::find()
                .filter(release_source::Column::NormalizedTitle.eq(&normalized_title))
                .filter(release_source::Column::Tracker.ne(&tracker))
                .all(&self.connection)
                .await?;
            let mut best: Option<(Uuid, f64)> = None;
            for candidate in candidates {
                let Ok(summary) =
                    serde_json::from_value::<ReleaseSummary>(candidate.source_json.clone())
                else {
                    continue;
                };
                let score = crate::release_matcher::summary_score(release, &summary);
                if best.is_none_or(|(_, known)| score > known) {
                    best = Some((Uuid::parse_str(&candidate.release_id)?, score));
                }
            }
            match best {
                Some((id, score)) if score >= crate::release_matcher::AUTO_MERGE_THRESHOLD => {
                    self.put_match_candidate("release", id, id, score, "accepted_auto", release)
                        .await?;
                    id
                }
                Some((other, score)) if score >= 0.80 => {
                    let id = Uuid::new_v4();
                    review_match = Some((id, other, score));
                    id
                }
                _ => Uuid::new_v4(),
            }
        };
        release.id = Some(release_id);

        if canonical_release::Entity::find_by_id(release_id.to_string())
            .one(&self.connection)
            .await?
            .is_none()
        {
            let now = Utc::now().to_rfc3339();
            canonical_release::ActiveModel {
                id: Set(release_id.to_string()),
                title: Set(release.title.clone()),
                normalized_title: Set(crate::release_matcher::normalized(&release.title)),
                artist: Set(release.artist.clone()),
                year: Set(release.year),
                release_type: Set(release.release_type.clone()),
                artwork: Set(release.artwork.clone()),
                metadata_json: Set(serde_json::to_value(&*release)?),
                provenance_json: Set(serde_json::json!({})),
                overrides_json: Set(serde_json::json!({})),
                created_at: Set(now.clone()),
                updated_at: Set(now),
            }
            .insert(&self.connection)
            .await?;
        }

        let source_json = serde_json::to_value(&*release)?;
        let normalized_artist = release
            .artist
            .as_deref()
            .map(crate::release_matcher::normalized)
            .unwrap_or_default();
        if let Some(model) = existing {
            let mut active = model.into_active_model();
            active.release_id = Set(release_id.to_string());
            active.normalized_title = Set(crate::release_matcher::normalized(&release.title));
            active.normalized_artist = Set(normalized_artist);
            active.year = Set(release.year);
            active.release_type = Set(release.release_type.clone());
            active.source_json = Set(source_json);
            active.fetched_at = Set(fetched_at.to_rfc3339());
            active.expires_at = Set(expires_at.to_rfc3339());
            active.last_error = Set(None);
            active.update(&self.connection).await?;
        } else {
            release_source::ActiveModel {
                tracker: Set(tracker),
                group_id: Set(release.group_id),
                release_id: Set(release_id.to_string()),
                normalized_title: Set(crate::release_matcher::normalized(&release.title)),
                normalized_artist: Set(normalized_artist),
                year: Set(release.year),
                release_type: Set(release.release_type.clone()),
                source_json: Set(source_json),
                fetched_at: Set(fetched_at.to_rfc3339()),
                expires_at: Set(expires_at.to_rfc3339()),
                last_error: Set(None),
            }
            .insert(&self.connection)
            .await?;
        }
        if let Some((left, right, score)) = review_match {
            self.put_match_candidate("release", left, right, score, "pending", release)
                .await?;
        }
        self.ensure_release_artists(release_id, release, fetched_at, expires_at)
            .await?;
        if let Some(model) = release_source::Entity::find_by_id((
            release.tracker.to_ascii_lowercase(),
            release.group_id,
        ))
        .one(&self.connection)
        .await?
        {
            let mut active = model.into_active_model();
            active.source_json = Set(serde_json::to_value(&*release)?);
            active.update(&self.connection).await?;
        }
        self.rebuild_release_metadata(release_id).await?;
        download_release_link::Entity::update_many()
            .col_expr(
                download_release_link::Column::ReleaseId,
                sea_orm::sea_query::Expr::value(Some(release_id.to_string())),
            )
            .filter(download_release_link::Column::Tracker.eq(&release.tracker))
            .filter(download_release_link::Column::GroupId.eq(release.group_id))
            .exec(&self.connection)
            .await?;
        Ok(release_id)
    }

    async fn ensure_release_artists(
        &self,
        release_id: Uuid,
        release: &mut ReleaseSummary,
        fetched_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        for credit in &mut release.artists {
            let tracker = credit.tracker.to_ascii_lowercase();
            let source_key = credit.key.clone();
            let normalized_name = crate::release_matcher::normalized(&credit.name);
            let existing = artist_source::Entity::find_by_id((tracker.clone(), source_key.clone()))
                .one(&self.connection)
                .await?;
            let artist_id = if let Some(source) = &existing {
                Uuid::parse_str(&source.canonical_artist_id)?
            } else {
                let same_name = artist_source::Entity::find()
                    .filter(artist_source::Column::NormalizedName.eq(&normalized_name))
                    .filter(artist_source::Column::Tracker.ne(&tracker))
                    .all(&self.connection)
                    .await?;
                let corroborated = if same_name.is_empty() {
                    None
                } else {
                    let credited = canonical_release_credit::Entity::find()
                        .filter(
                            canonical_release_credit::Column::ReleaseId.eq(release_id.to_string()),
                        )
                        .all(&self.connection)
                        .await?;
                    same_name.iter().find_map(|source| {
                        credited
                            .iter()
                            .any(|known| known.artist_id == source.canonical_artist_id)
                            .then(|| Uuid::parse_str(&source.canonical_artist_id))
                    })
                };
                match corroborated.transpose()? {
                    Some(id) => id,
                    None => {
                        let id = Uuid::new_v4();
                        if let Some(other) = same_name.first() {
                            self.put_match_candidate(
                                "artist",
                                id,
                                Uuid::parse_str(&other.canonical_artist_id)?,
                                0.80,
                                "pending",
                                credit,
                            )
                            .await?;
                        }
                        id
                    }
                }
            };
            credit.canonical_id = Some(artist_id);
            if canonical_artist::Entity::find_by_id(artist_id.to_string())
                .one(&self.connection)
                .await?
                .is_none()
            {
                let now = Utc::now().to_rfc3339();
                canonical_artist::ActiveModel {
                    id: Set(artist_id.to_string()),
                    name: Set(credit.name.clone()),
                    normalized_name: Set(normalized_name.clone()),
                    artwork: Set(None),
                    metadata_json: Set(serde_json::to_value(&*credit)?),
                    provenance_json: Set(serde_json::json!({
                        "name": { "tracker": credit.tracker, "sourceKey": credit.key }
                    })),
                    overrides_json: Set(serde_json::json!({})),
                    created_at: Set(now.clone()),
                    updated_at: Set(now),
                }
                .insert(&self.connection)
                .await?;
            }
            let source_json = serde_json::to_value(&*credit)?;
            if let Some(model) = existing {
                let mut active = model.into_active_model();
                active.canonical_artist_id = Set(artist_id.to_string());
                active.name = Set(credit.name.clone());
                active.normalized_name = Set(normalized_name);
                active.source_json = Set(source_json);
                active.fetched_at = Set(fetched_at.to_rfc3339());
                active.expires_at = Set(expires_at.to_rfc3339());
                active.last_error = Set(None);
                active.update(&self.connection).await?;
            } else {
                artist_source::ActiveModel {
                    tracker: Set(tracker),
                    source_key: Set(source_key),
                    artist_id: Set(credit.artist_id),
                    canonical_artist_id: Set(artist_id.to_string()),
                    name: Set(credit.name.clone()),
                    normalized_name: Set(normalized_name),
                    source_json: Set(source_json),
                    fetched_at: Set(fetched_at.to_rfc3339()),
                    expires_at: Set(expires_at.to_rfc3339()),
                    last_error: Set(None),
                }
                .insert(&self.connection)
                .await?;
            }
            let role = match credit.role {
                ArtistRole::Primary => "primary",
                ArtistRole::Guest => "guest",
            };
            if let Some(model) = canonical_release_credit::Entity::find_by_id((
                release_id.to_string(),
                artist_id.to_string(),
                role.to_owned(),
            ))
            .one(&self.connection)
            .await?
            {
                let mut active = model.into_active_model();
                active.source_count = Set(active.source_count.take().unwrap_or(1).max(1));
                active.update(&self.connection).await?;
            } else {
                canonical_release_credit::ActiveModel {
                    release_id: Set(release_id.to_string()),
                    artist_id: Set(artist_id.to_string()),
                    role: Set(role.to_owned()),
                    source_count: Set(1),
                }
                .insert(&self.connection)
                .await?;
            }
        }
        Ok(())
    }

    async fn put_match_candidate<T: Serialize>(
        &self,
        kind: &str,
        left: Uuid,
        right: Uuid,
        score: f64,
        status: &str,
        evidence: &T,
    ) -> Result<()> {
        if left == right && status == "accepted_auto" {
            return Ok(());
        }
        let (left, right) = if left.as_bytes() <= right.as_bytes() {
            (left, right)
        } else {
            (right, left)
        };
        if let Some(model) = match_candidate::Entity::find()
            .filter(match_candidate::Column::Kind.eq(kind))
            .filter(match_candidate::Column::LeftId.eq(left.to_string()))
            .filter(match_candidate::Column::RightId.eq(right.to_string()))
            .one(&self.connection)
            .await?
        {
            if model.status == "rejected" {
                return Ok(());
            }
            let mut active = model.into_active_model();
            active.score = Set(score);
            active.status = Set(status.to_owned());
            active.evidence_json = Set(serde_json::to_value(evidence)?);
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        } else {
            let now = Utc::now().to_rfc3339();
            match_candidate::ActiveModel {
                id: Set(Uuid::new_v4().to_string()),
                kind: Set(kind.to_owned()),
                left_id: Set(left.to_string()),
                right_id: Set(right.to_string()),
                score: Set(score),
                status: Set(status.to_owned()),
                evidence_json: Set(serde_json::to_value(evidence)?),
                created_at: Set(now.clone()),
                updated_at: Set(now),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    async fn rebuild_release_metadata(&self, release_id: Uuid) -> Result<()> {
        let sources = release_source::Entity::find()
            .filter(release_source::Column::ReleaseId.eq(release_id.to_string()))
            .all(&self.connection)
            .await?;
        let mut summaries = sources
            .iter()
            .filter_map(|source| {
                serde_json::from_value::<ReleaseSummary>(source.source_json.clone())
                    .ok()
                    .map(|summary| (source, summary))
            })
            .collect::<Vec<_>>();
        if summaries.is_empty() {
            return Ok(());
        }
        summaries.sort_by(|(left_source, left), (right_source, right)| {
            metadata_completeness(right)
                .cmp(&metadata_completeness(left))
                .then_with(|| left.title.cmp(&right.title))
                .then_with(|| left_source.tracker.cmp(&right_source.tracker))
        });
        let (_, mut chosen) = summaries[0].clone();
        chosen.id = Some(release_id);
        chosen.artists = summaries
            .iter()
            .flat_map(|(_, summary)| summary.artists.iter().cloned())
            .fold(Vec::new(), |mut artists, artist| {
                if !artists.iter().any(|known: &crate::model::ArtistCredit| {
                    known.role == artist.role
                        && match (known.canonical_id, artist.canonical_id) {
                            (Some(left), Some(right)) => left == right,
                            _ => {
                                crate::release_matcher::normalized(&known.name)
                                    == crate::release_matcher::normalized(&artist.name)
                            }
                        }
                }) {
                    artists.push(artist);
                }
                artists
            });
        chosen.sources = summaries
            .iter()
            .map(|(source, _)| crate::model::ReleaseSource {
                tracker: source.tracker.clone(),
                group_id: source.group_id,
                match_score: 1.0,
            })
            .collect();
        let choose = |score: fn(&ReleaseSummary) -> usize| {
            summaries
                .iter()
                .max_by(|(left_source, left), (right_source, right)| {
                    score(left)
                        .cmp(&score(right))
                        .then_with(|| right_source.tracker.cmp(&left_source.tracker))
                })
                .map(|(source, summary)| (*source, summary))
        };
        let (title_source, title) = choose(title_field_score).expect("non-empty release sources");
        let (artist_source, artist) =
            choose(artist_field_score).expect("non-empty release sources");
        let (artwork_source, artwork) =
            choose(artwork_field_score).expect("non-empty release sources");
        let (type_source, release_type) =
            choose(release_type_field_score).expect("non-empty release sources");
        let mut year_counts = std::collections::HashMap::new();
        for (_, summary) in &summaries {
            if let Some(year) = summary.year {
                *year_counts.entry(year).or_insert(0_usize) += 1;
            }
        }
        let selected_year = year_counts
            .into_iter()
            .max_by(|(left_year, left_count), (right_year, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| right_year.cmp(left_year))
            })
            .map(|(year, _)| year);
        let year_source = selected_year.and_then(|year| {
            summaries
                .iter()
                .find(|(_, summary)| summary.year == Some(year))
                .map(|(source, _)| *source)
        });
        chosen.title = title.title.clone();
        chosen.artist = artist.artist.clone();
        chosen.artwork = artwork.artwork.clone();
        chosen.release_type = release_type.release_type.clone();
        chosen.year = selected_year;
        let source_value = |source: &release_source::Model| serde_json::json!({ "tracker": source.tracker, "groupId": source.group_id });
        let mut provenance = serde_json::json!({
            "title": source_value(title_source),
            "artist": source_value(artist_source),
            "artwork": source_value(artwork_source),
            "releaseType": source_value(type_source)
        });
        if let Some(source) = year_source {
            provenance["year"] = source_value(source);
        }
        if let Some(model) = canonical_release::Entity::find_by_id(release_id.to_string())
            .one(&self.connection)
            .await?
        {
            if let Some(value) = model
                .overrides_json
                .get("title")
                .and_then(serde_json::Value::as_str)
            {
                chosen.title = value.to_owned();
                provenance["title"] = serde_json::json!({ "manual": true });
            }
            if let Some(value) = model
                .overrides_json
                .get("artist")
                .and_then(serde_json::Value::as_str)
            {
                chosen.artist = Some(value.to_owned());
                provenance["artist"] = serde_json::json!({ "manual": true });
            }
            if let Some(value) = model
                .overrides_json
                .get("year")
                .and_then(serde_json::Value::as_i64)
            {
                chosen.year = Some(value);
                provenance["year"] = serde_json::json!({ "manual": true });
            }
            let mut active = model.into_active_model();
            active.title = Set(chosen.title.clone());
            active.normalized_title = Set(crate::release_matcher::normalized(&chosen.title));
            active.artist = Set(chosen.artist.clone());
            active.year = Set(chosen.year);
            active.release_type = Set(chosen.release_type.clone());
            active.artwork = Set(chosen.artwork.clone());
            active.metadata_json = Set(serde_json::to_value(chosen)?);
            active.provenance_json = Set(provenance);
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn release_id_for_source(
        &self,
        tracker: &str,
        group_id: i64,
    ) -> Result<Option<Uuid>> {
        release_source::Entity::find_by_id((tracker.to_ascii_lowercase(), group_id))
            .one(&self.connection)
            .await?
            .map(|source| Uuid::parse_str(&source.release_id).map_err(Into::into))
            .transpose()
    }

    pub async fn release_ids_for_sources(
        &self,
        sources: &[(String, i64)],
    ) -> Result<HashMap<(String, i64), Uuid>> {
        let mut release_ids = HashMap::new();
        for chunk in sources.chunks(300) {
            if chunk.is_empty() {
                continue;
            }
            let condition = chunk
                .iter()
                .fold(Condition::any(), |condition, (tracker, id)| {
                    condition.add(
                        Condition::all()
                            .add(release_source::Column::Tracker.eq(tracker.to_ascii_lowercase()))
                            .add(release_source::Column::GroupId.eq(*id)),
                    )
                });
            for source in release_source::Entity::find()
                .filter(condition)
                .all(&self.connection)
                .await?
            {
                release_ids.insert(
                    (source.tracker, source.group_id),
                    Uuid::parse_str(&source.release_id)?,
                );
            }
        }
        Ok(release_ids)
    }

    pub async fn merge_release_sources(
        &self,
        sources: &[crate::model::ReleaseSource],
    ) -> Result<Option<Uuid>> {
        let mut ids = Vec::new();
        for source in sources {
            if let Some(id) = self
                .release_id_for_source(&source.tracker, source.group_id)
                .await?
            {
                ids.push(id);
            }
        }
        ids.sort_by_key(Uuid::as_u128);
        ids.dedup();
        let Some(target) = ids.first().copied() else {
            return Ok(None);
        };
        for source in ids.into_iter().skip(1) {
            self.merge_releases(target, source).await?;
        }
        Ok(Some(target))
    }

    pub async fn unlink_release_source(
        &self,
        release_id: Uuid,
        tracker: &str,
        group_id: i64,
    ) -> Result<Option<Uuid>> {
        let release_id = self.resolve_alias("release", release_id).await?;
        let Some(model) =
            release_source::Entity::find_by_id((tracker.to_ascii_lowercase(), group_id))
                .one(&self.connection)
                .await?
        else {
            return Ok(None);
        };
        if model.release_id != release_id.to_string() {
            return Ok(None);
        }
        let source_count = release_source::Entity::find()
            .filter(release_source::Column::ReleaseId.eq(release_id.to_string()))
            .count(&self.connection)
            .await?;
        if source_count < 2 {
            anyhow::bail!("a release with only one source cannot be unlinked");
        }
        let new_id = Uuid::new_v4();
        let mut summary: ReleaseSummary = serde_json::from_value(model.source_json.clone())?;
        summary.id = Some(new_id);
        summary.sources = vec![crate::model::ReleaseSource {
            tracker: model.tracker.clone(),
            group_id: model.group_id,
            match_score: 1.0,
        }];
        let now = Utc::now().to_rfc3339();
        canonical_release::ActiveModel {
            id: Set(new_id.to_string()),
            title: Set(summary.title.clone()),
            normalized_title: Set(crate::release_matcher::normalized(&summary.title)),
            artist: Set(summary.artist.clone()),
            year: Set(summary.year),
            release_type: Set(summary.release_type.clone()),
            artwork: Set(summary.artwork.clone()),
            metadata_json: Set(serde_json::to_value(&summary)?),
            provenance_json: Set(serde_json::json!({})),
            overrides_json: Set(serde_json::json!({})),
            created_at: Set(now.clone()),
            updated_at: Set(now),
        }
        .insert(&self.connection)
        .await?;
        let mut active = model.into_active_model();
        active.release_id = Set(new_id.to_string());
        active.source_json = Set(serde_json::to_value(&summary)?);
        active.update(&self.connection).await?;
        canonical_torrent::Entity::update_many()
            .col_expr(
                canonical_torrent::Column::ReleaseId,
                sea_orm::sea_query::Expr::value(Some(new_id.to_string())),
            )
            .filter(canonical_torrent::Column::Tracker.eq(tracker))
            .filter(canonical_torrent::Column::GroupId.eq(group_id))
            .exec(&self.connection)
            .await?;
        download_release_link::Entity::update_many()
            .col_expr(
                download_release_link::Column::ReleaseId,
                sea_orm::sea_query::Expr::value(Some(new_id.to_string())),
            )
            .filter(download_release_link::Column::Tracker.eq(tracker))
            .filter(download_release_link::Column::GroupId.eq(group_id))
            .exec(&self.connection)
            .await?;
        for credit in &summary.artists {
            let Some(artist_id) = credit.canonical_id else {
                continue;
            };
            let role = match credit.role {
                ArtistRole::Primary => "primary",
                ArtistRole::Guest => "guest",
            };
            canonical_release_credit::ActiveModel {
                release_id: Set(new_id.to_string()),
                artist_id: Set(artist_id.to_string()),
                role: Set(role.into()),
                source_count: Set(1),
            }
            .insert(&self.connection)
            .await?;
        }
        self.put_match_candidate("release", release_id, new_id, 0.0, "rejected", &summary)
            .await?;
        self.rebuild_release_metadata(release_id).await?;
        self.rebuild_release_metadata(new_id).await?;
        Ok(Some(new_id))
    }

    pub async fn get_release_detail(&self, id: Uuid) -> Result<Option<ReleaseDetail>> {
        let id = self.resolve_alias("release", id).await?;
        let Some(release) = canonical_release::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        else {
            return Ok(None);
        };
        let mut summary: ReleaseSummary = serde_json::from_value(release.metadata_json)?;
        summary.id = Some(id);
        let variants = canonical_torrent::Entity::find()
            .filter(canonical_torrent::Column::ReleaseId.eq(id.to_string()))
            .all(&self.connection)
            .await?;
        let mut tags = Vec::new();
        let mut description = None;
        let mut record_label = None;
        let mut torrent_variants = Vec::new();
        for model in variants {
            let canonical: CanonicalTorrent = serde_json::from_value(model.canonical_json)?;
            for tag in canonical.tags {
                if !tags
                    .iter()
                    .any(|known: &String| known.eq_ignore_ascii_case(&tag))
                {
                    tags.push(tag);
                }
            }
            if canonical
                .description
                .as_ref()
                .is_some_and(|value| value.len() > description.as_deref().unwrap_or("").len())
            {
                description = canonical.description;
            }
            if record_label.is_none() {
                record_label = canonical.record_label;
            }
            torrent_variants.push(canonical.variant);
        }
        Ok(Some(ReleaseDetail {
            release: summary,
            field_provenance: release.provenance_json,
            tags,
            description,
            record_label,
            variants: torrent_variants,
        }))
    }

    pub async fn get_release_details(&self, ids: &[Uuid]) -> Result<HashMap<Uuid, ReleaseDetail>> {
        let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        let releases = canonical_release::Entity::find()
            .filter(canonical_release::Column::Id.is_in(ids.clone()))
            .all(&self.connection)
            .await?;
        let variants = canonical_torrent::Entity::find()
            .filter(canonical_torrent::Column::ReleaseId.is_in(ids))
            .all(&self.connection)
            .await?;
        let mut variants_by_release: HashMap<String, Vec<canonical_torrent::Model>> =
            HashMap::new();
        for variant in variants {
            if let Some(release_id) = variant.release_id.clone() {
                variants_by_release
                    .entry(release_id)
                    .or_default()
                    .push(variant);
            }
        }

        let mut details = HashMap::new();
        for release in releases {
            let id = Uuid::parse_str(&release.id)?;
            let mut summary: ReleaseSummary = serde_json::from_value(release.metadata_json)?;
            summary.id = Some(id);
            let mut tags = Vec::new();
            let mut description: Option<String> = None;
            let mut record_label = None;
            let mut torrent_variants = Vec::new();
            for model in variants_by_release.remove(&release.id).unwrap_or_default() {
                let canonical: CanonicalTorrent = serde_json::from_value(model.canonical_json)?;
                for tag in canonical.tags {
                    if !tags
                        .iter()
                        .any(|known: &String| known.eq_ignore_ascii_case(&tag))
                    {
                        tags.push(tag);
                    }
                }
                if canonical
                    .description
                    .as_ref()
                    .is_some_and(|value| value.len() > description.as_deref().unwrap_or("").len())
                {
                    description = canonical.description;
                }
                if record_label.is_none() {
                    record_label = canonical.record_label;
                }
                torrent_variants.push(canonical.variant);
            }
            details.insert(
                id,
                ReleaseDetail {
                    release: summary,
                    field_provenance: release.provenance_json,
                    tags,
                    description,
                    record_label,
                    variants: torrent_variants,
                },
            );
        }
        Ok(details)
    }

    pub async fn set_release_overrides(&self, id: Uuid, overrides: Value) -> Result<bool> {
        let id = self.resolve_alias("release", id).await?;
        let Some(model) = canonical_release::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        else {
            return Ok(false);
        };
        if !overrides.is_object() {
            anyhow::bail!("release metadata overrides must be a JSON object");
        }
        let mut active = model.into_active_model();
        active.overrides_json = Set(overrides);
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        self.rebuild_release_metadata(id).await?;
        Ok(true)
    }

    pub async fn backfill_canonical_identities(&self, limit: u64) -> Result<usize> {
        let total = canonical_torrent::Entity::find()
            .count(&self.connection)
            .await? as i64;
        let remaining_before = canonical_torrent::Entity::find()
            .filter(canonical_torrent::Column::ReleaseId.is_null())
            .count(&self.connection)
            .await? as i64;
        let library_links = download_release_link::Entity::find()
            .filter(download_release_link::Column::LibraryAddedAt.is_not_null())
            .filter(download_release_link::Column::ReleaseId.is_null())
            .order_by_desc(download_release_link::Column::LibraryAddedAt)
            .limit(limit)
            .all(&self.connection)
            .await?;
        let mut models = Vec::new();
        let mut selected = std::collections::HashSet::new();
        for link in library_links {
            let (Some(tracker), Some(torrent_id)) = (link.tracker, link.torrent_id) else {
                continue;
            };
            if selected.insert((tracker.clone(), torrent_id))
                && let Some(model) = canonical_torrent::Entity::find_by_id((tracker, torrent_id))
                    .one(&self.connection)
                    .await?
                && model.release_id.is_none()
            {
                models.push(model);
            }
        }
        let remaining_limit = limit.saturating_sub(models.len() as u64);
        if remaining_limit > 0 {
            let fallback = canonical_torrent::Entity::find()
                .filter(canonical_torrent::Column::ReleaseId.is_null())
                .order_by_desc(canonical_torrent::Column::FetchedAt)
                .limit(remaining_limit)
                .all(&self.connection)
                .await?;
            for model in fallback {
                if selected.insert((model.tracker.clone(), model.torrent_id)) {
                    models.push(model);
                }
            }
        }
        let mut processed = 0_i64;
        let mut last_error = None;
        for model in models {
            let canonical =
                match serde_json::from_value::<CanonicalTorrent>(model.canonical_json.clone()) {
                    Ok(canonical) => canonical,
                    Err(error) => {
                        last_error = Some(error.to_string());
                        continue;
                    }
                };
            match self
                .put_canonical(
                    &canonical,
                    parse_timestamp(&model.fetched_at)?,
                    parse_timestamp(&model.expires_at)?,
                )
                .await
            {
                Ok(()) => processed += 1,
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        let remaining = (remaining_before - processed).max(0);
        let state = if remaining == 0 {
            "complete"
        } else {
            "running"
        };
        let progress = canonical_backfill_state::Entity::find_by_id("canonical_identity")
            .one(&self.connection)
            .await?;
        if let Some(model) = progress {
            let mut active = model.into_active_model();
            active.state = Set(state.into());
            active.processed = Set(total - remaining);
            active.total = Set(total);
            active.last_error = Set(last_error);
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        } else {
            canonical_backfill_state::ActiveModel {
                key: Set("canonical_identity".into()),
                state: Set(state.into()),
                processed: Set(total - remaining),
                total: Set(total),
                last_error: Set(last_error),
                updated_at: Set(Utc::now().to_rfc3339()),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(processed as usize)
    }

    pub async fn canonical_backfill_progress(&self) -> Result<CanonicalBackfillProgress> {
        if let Some(model) = canonical_backfill_state::Entity::find_by_id("canonical_identity")
            .one(&self.connection)
            .await?
        {
            return Ok(CanonicalBackfillProgress {
                state: model.state,
                processed: model.processed,
                total: model.total,
                remaining: (model.total - model.processed).max(0),
                last_error: model.last_error,
            });
        }
        let total = canonical_torrent::Entity::find()
            .count(&self.connection)
            .await? as i64;
        let remaining = canonical_torrent::Entity::find()
            .filter(canonical_torrent::Column::ReleaseId.is_null())
            .count(&self.connection)
            .await? as i64;
        Ok(CanonicalBackfillProgress {
            state: if remaining == 0 {
                "complete"
            } else {
                "pending"
            }
            .into(),
            processed: total - remaining,
            total,
            remaining,
            last_error: None,
        })
    }

    pub async fn get_canonical_artist(&self, id: Uuid) -> Result<Option<canonical_artist::Model>> {
        let id = self.resolve_alias("artist", id).await?;
        Ok(canonical_artist::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?)
    }

    pub async fn set_artist_overrides(&self, id: Uuid, overrides: Value) -> Result<bool> {
        let id = self.resolve_alias("artist", id).await?;
        let Some(model) = canonical_artist::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        else {
            return Ok(false);
        };
        if !overrides.is_object() {
            anyhow::bail!("artist metadata overrides must be a JSON object");
        }
        let name = overrides
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let artwork = overrides
            .get("artwork")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut active = model.into_active_model();
        if let Some(name) = name {
            active.name = Set(name.clone());
            active.normalized_name = Set(crate::release_matcher::normalized(&name));
        }
        if artwork.is_some() {
            active.artwork = Set(artwork);
        }
        active.overrides_json = Set(overrides);
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        Ok(true)
    }

    pub async fn artist_sources_for(&self, id: Uuid) -> Result<Vec<artist_source::Model>> {
        let id = self.resolve_alias("artist", id).await?;
        Ok(artist_source::Entity::find()
            .filter(artist_source::Column::CanonicalArtistId.eq(id.to_string()))
            .order_by_asc(artist_source::Column::Tracker)
            .all(&self.connection)
            .await?)
    }

    pub async fn list_match_candidates(
        &self,
        kind: Option<&str>,
        status: Option<&str>,
        limit: u64,
    ) -> Result<Vec<match_candidate::Model>> {
        let mut query = match_candidate::Entity::find();
        if let Some(kind) = kind {
            query = query.filter(match_candidate::Column::Kind.eq(kind));
        }
        if let Some(status) = status {
            query = query.filter(match_candidate::Column::Status.eq(status));
        }
        Ok(query
            .order_by_desc(match_candidate::Column::Score)
            .order_by_asc(match_candidate::Column::CreatedAt)
            .limit(limit.clamp(1, 500))
            .all(&self.connection)
            .await?)
    }

    pub async fn decide_match_candidate(&self, id: Uuid, accept: bool) -> Result<bool> {
        let Some(model) = match_candidate::Entity::find_by_id(id.to_string())
            .one(&self.connection)
            .await?
        else {
            return Ok(false);
        };
        if accept {
            let left = Uuid::parse_str(&model.left_id)?;
            let right = Uuid::parse_str(&model.right_id)?;
            match model.kind.as_str() {
                "release" => self.merge_releases(left, right).await?,
                "artist" => self.merge_artists(left, right).await?,
                _ => anyhow::bail!("unsupported match kind {}", model.kind),
            }
        }
        let mut active = model.into_active_model();
        active.status = Set(if accept {
            "accepted_manual"
        } else {
            "rejected"
        }
        .into());
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        Ok(true)
    }

    async fn merge_releases(&self, target: Uuid, source: Uuid) -> Result<()> {
        let target = self.resolve_alias("release", target).await?;
        let source = self.resolve_alias("release", source).await?;
        if target == source {
            return Ok(());
        }
        let source_rows = release_source::Entity::find()
            .filter(release_source::Column::ReleaseId.eq(source.to_string()))
            .all(&self.connection)
            .await?;
        for model in source_rows {
            let mut active = model.into_active_model();
            active.release_id = Set(target.to_string());
            active.update(&self.connection).await?;
        }
        canonical_torrent::Entity::update_many()
            .col_expr(
                canonical_torrent::Column::ReleaseId,
                sea_orm::sea_query::Expr::value(Some(target.to_string())),
            )
            .filter(canonical_torrent::Column::ReleaseId.eq(source.to_string()))
            .exec(&self.connection)
            .await?;
        download_release_link::Entity::update_many()
            .col_expr(
                download_release_link::Column::ReleaseId,
                sea_orm::sea_query::Expr::value(Some(target.to_string())),
            )
            .filter(download_release_link::Column::ReleaseId.eq(source.to_string()))
            .exec(&self.connection)
            .await?;
        let credits = canonical_release_credit::Entity::find()
            .filter(canonical_release_credit::Column::ReleaseId.eq(source.to_string()))
            .all(&self.connection)
            .await?;
        for credit in credits {
            let target_key = (
                target.to_string(),
                credit.artist_id.clone(),
                credit.role.clone(),
            );
            if canonical_release_credit::Entity::find_by_id(target_key.clone())
                .one(&self.connection)
                .await?
                .is_none()
            {
                canonical_release_credit::ActiveModel {
                    release_id: Set(target_key.0),
                    artist_id: Set(target_key.1),
                    role: Set(target_key.2),
                    source_count: Set(credit.source_count),
                }
                .insert(&self.connection)
                .await?;
            }
            credit.delete(&self.connection).await?;
        }
        self.put_alias("release", source, target).await?;
        canonical_release::Entity::delete_by_id(source.to_string())
            .exec(&self.connection)
            .await?;
        self.rebuild_release_metadata(target).await
    }

    async fn merge_artists(&self, target: Uuid, source: Uuid) -> Result<()> {
        let target = self.resolve_alias("artist", target).await?;
        let source = self.resolve_alias("artist", source).await?;
        if target == source {
            return Ok(());
        }
        let source_rows = artist_source::Entity::find()
            .filter(artist_source::Column::CanonicalArtistId.eq(source.to_string()))
            .all(&self.connection)
            .await?;
        for model in source_rows {
            let mut active = model.into_active_model();
            active.canonical_artist_id = Set(target.to_string());
            active.update(&self.connection).await?;
        }
        let credits = canonical_release_credit::Entity::find()
            .filter(canonical_release_credit::Column::ArtistId.eq(source.to_string()))
            .all(&self.connection)
            .await?;
        for credit in credits {
            let target_key = (
                credit.release_id.clone(),
                target.to_string(),
                credit.role.clone(),
            );
            if canonical_release_credit::Entity::find_by_id(target_key.clone())
                .one(&self.connection)
                .await?
                .is_none()
            {
                canonical_release_credit::ActiveModel {
                    release_id: Set(target_key.0),
                    artist_id: Set(target_key.1),
                    role: Set(target_key.2),
                    source_count: Set(credit.source_count),
                }
                .insert(&self.connection)
                .await?;
            }
            credit.delete(&self.connection).await?;
        }
        let mut affected_releases = std::collections::HashSet::new();
        for model in release_source::Entity::find().all(&self.connection).await? {
            let Ok(mut summary) =
                serde_json::from_value::<ReleaseSummary>(model.source_json.clone())
            else {
                continue;
            };
            let mut changed = false;
            for credit in &mut summary.artists {
                if credit.canonical_id == Some(source) {
                    credit.canonical_id = Some(target);
                    changed = true;
                }
            }
            if changed {
                affected_releases.insert(Uuid::parse_str(&model.release_id)?);
                let mut active = model.into_active_model();
                active.source_json = Set(serde_json::to_value(summary)?);
                active.update(&self.connection).await?;
            }
        }
        self.put_alias("artist", source, target).await?;
        canonical_artist::Entity::delete_by_id(source.to_string())
            .exec(&self.connection)
            .await?;
        for release_id in affected_releases {
            self.rebuild_release_metadata(release_id).await?;
        }
        Ok(())
    }

    async fn put_alias(&self, kind: &str, alias: Uuid, target: Uuid) -> Result<()> {
        if canonical_alias::Entity::find_by_id((kind.to_owned(), alias.to_string()))
            .one(&self.connection)
            .await?
            .is_none()
        {
            canonical_alias::ActiveModel {
                kind: Set(kind.to_owned()),
                alias_id: Set(alias.to_string()),
                target_id: Set(target.to_string()),
                created_at: Set(Utc::now().to_rfc3339()),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    async fn resolve_alias(&self, kind: &str, mut id: Uuid) -> Result<Uuid> {
        for _ in 0..8 {
            let Some(alias) =
                canonical_alias::Entity::find_by_id((kind.to_owned(), id.to_string()))
                    .one(&self.connection)
                    .await?
            else {
                return Ok(id);
            };
            id = Uuid::parse_str(&alias.target_id)?;
        }
        anyhow::bail!("canonical alias chain is too deep")
    }

    async fn replace_release_artists(&self, canonical: &CanonicalTorrent) -> Result<()> {
        if canonical.release.artists.is_empty() {
            return Ok(());
        }
        let transaction = self.begin_write().await?;
        canonical_release_artist::Entity::delete_many()
            .filter(canonical_release_artist::Column::Tracker.eq(&canonical.release.tracker))
            .filter(canonical_release_artist::Column::GroupId.eq(canonical.release.group_id))
            .exec(&transaction)
            .await?;
        for artist in &canonical.release.artists {
            canonical_release_artist::ActiveModel {
                tracker: Set(canonical.release.tracker.clone()),
                group_id: Set(canonical.release.group_id),
                artist_key: Set(artist.key.clone()),
                role: Set(match artist.role {
                    crate::model::ArtistRole::Primary => "primary",
                    crate::model::ArtistRole::Guest => "guest",
                }
                .into()),
                artist_id: Set(artist.artist_id),
                name: Set(artist.name.clone()),
                sort_name: Set(artist_sort_name(&artist.name)),
                source: Set(match artist.source {
                    ArtistCreditSource::Structured => "structured",
                    ArtistCreditSource::DisplayFallback => "display_fallback",
                }
                .into()),
            }
            .insert(&transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn get_canonical(
        &self,
        tracker: &str,
        torrent_id: i64,
    ) -> Result<Option<Cached<CanonicalTorrent>>> {
        canonical_torrent::Entity::find_by_id((tracker.to_owned(), torrent_id))
            .one(&self.connection)
            .await?
            .map(canonical_from_model)
            .transpose()
    }

    #[allow(dead_code)]
    pub async fn list_canonical_for_tracker(&self, tracker: &str) -> Result<Vec<CanonicalTorrent>> {
        canonical_torrent::Entity::find()
            .filter(canonical_torrent::Column::Tracker.eq(tracker))
            .all(&self.connection)
            .await?
            .into_iter()
            .map(|model| serde_json::from_value(model.canonical_json).map_err(Into::into))
            .collect()
    }

    pub async fn observe_download(
        &self,
        live: &LiveDownloadStatus,
        announce_host: Option<&str>,
        tracker: Option<&str>,
        plex_target: Option<&PlexScanTarget>,
    ) -> Result<()> {
        self.observe_downloads(&[DownloadObservation {
            torrent_name: None,
            live: live.clone(),
            announce_host: announce_host.map(str::to_owned),
            tracker: tracker.map(str::to_owned),
            plex_target: plex_target.cloned(),
        }])
        .await
    }

    pub async fn observe_downloads(&self, observations: &[DownloadObservation]) -> Result<()> {
        let transaction = self.begin_write().await?;
        for observation in observations {
            self.observe_download_on(&transaction, observation).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn observe_download_on<C: ConnectionTrait>(
        &self,
        transaction: &C,
        observation: &DownloadObservation,
    ) -> Result<()> {
        let live = &observation.live;
        let torrent_name = observation
            .torrent_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let announce_host = observation.announce_host.as_deref();
        let tracker = observation.tracker.as_deref();
        let plex_target = observation.plex_target.as_ref();
        let now_value = Utc::now();
        let now = now_value.to_rfc3339();
        let completed_at =
            (live.progress >= 1.0).then(|| live.completed_at.unwrap_or(now_value).to_rfc3339());
        let hash = live.info_hash.to_ascii_lowercase();
        let state = if tracker.is_some() {
            "pending"
        } else {
            "unconfigured"
        };
        let should_resolve;
        let reactivate_resolution;
        let newly_completed;
        if let Some(model) =
            download_release_link::Entity::find_by_id((live.client.clone(), hash.clone()))
                .one(transaction)
                .await?
        {
            let linked = model.resolution_state == "linked";
            let tracker_changed = model.tracker.as_deref() != tracker;
            let release_level_match =
                linked && (model.group_id.is_none() || model.torrent_id.is_none());
            reactivate_resolution = release_level_match;
            let retry_due = model
                .next_retry_at
                .as_deref()
                .is_none_or(|value| parse_timestamp(value).is_ok_and(|value| value <= now_value));
            should_resolve = tracker.is_some()
                && (tracker_changed
                    || release_level_match
                    || (model.resolution_state == "pending" && retry_due));
            let has_library_added_at = model.library_added_at.is_some();
            let has_completed_at = model.completed_at.is_some();
            newly_completed = !has_completed_at && completed_at.is_some();
            let client_added_at = live
                .added_at
                .map(|value| value.to_rfc3339())
                .or_else(|| model.client_added_at.clone());
            let mut active = model.into_active_model();
            active.announce_host = Set(announce_host.map(str::to_owned));
            if let Some(torrent_name) = torrent_name {
                active.torrent_name = Set(Some(torrent_name.chars().take(500).collect()));
            }
            if !linked {
                active.tracker = Set(tracker.map(str::to_owned));
                if tracker_changed {
                    active.resolution_state = Set(state.into());
                }
                active.updated_at = Set(now.clone());
            }
            active.last_seen_at = Set(now);
            active.observed_json = Set(Some(serde_json::to_value(live)?));
            active.observed_at = Set(Some(now_value.to_rfc3339()));
            active.client_added_at = Set(client_added_at);
            active.present = Set(true);
            active.missing_since = Set(None);
            if !has_library_added_at && completed_at.is_some() {
                active.library_added_at = Set(completed_at.clone());
            }
            if !has_completed_at && completed_at.is_some() {
                active.completed_at = Set(completed_at);
            }
            active.update(transaction).await?;
        } else {
            reactivate_resolution = false;
            newly_completed = completed_at.is_some();
            should_resolve = tracker.is_some();
            download_release_link::ActiveModel {
                client: Set(live.client.clone()),
                info_hash: Set(hash.clone()),
                announce_host: Set(announce_host.map(str::to_owned)),
                torrent_name: Set(torrent_name.map(|value| value.chars().take(500).collect())),
                tracker: Set(tracker.map(str::to_owned)),
                group_id: Set(None),
                torrent_id: Set(None),
                release_id: Set(None),
                resolution_state: Set(state.into()),
                attempts: Set(0),
                next_retry_at: Set(None),
                error_code: Set(None),
                error_message: Set(None),
                first_seen_at: Set(now.clone()),
                last_seen_at: Set(now.clone()),
                updated_at: Set(now),
                present: Set(true),
                missing_since: Set(None),
                library_added_at: Set(completed_at.clone()),
                completed_at: Set(completed_at),
                observed_json: Set(Some(serde_json::to_value(live)?)),
                observed_at: Set(Some(now_value.to_rfc3339())),
                client_added_at: Set(live.added_at.map(|value| value.to_rfc3339())),
            }
            .insert(transaction)
            .await?;
        }
        if should_resolve && let Some(tracker) = tracker {
            let key = format!("resolve-hash:{}:{hash}:v3", tracker.to_ascii_lowercase());
            if reactivate_resolution {
                self.reactivate_completed_background_job_on(transaction, &key)
                    .await?;
            }
            self.enqueue_background_job_on(
                transaction,
                EnqueueBackgroundJob {
                    deduplication_key: &key,
                    kind: "resolve_download_hash",
                    payload: serde_json::json!({ "tracker": tracker, "infoHash": hash }),
                    provider_id: Some(format!("tracker:{tracker}")),
                    lane: "sync",
                    priority: 20,
                    max_attempts: 8,
                    next_run_at: None,
                    parent_id: None,
                    recurring_interval_seconds: None,
                },
            )
            .await?;
        }
        if newly_completed && let Some(target) = plex_target {
            self.enqueue_plex_scan_on(transaction, target, now_value)
                .await?;
        }
        Ok(())
    }

    pub async fn seed_download_link(
        &self,
        client: &str,
        info_hash: &str,
        tracker: &str,
        group_id: Option<i64>,
        torrent_id: i64,
        linked: bool,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let hash = info_hash.to_ascii_lowercase();
        let release_id = match group_id {
            Some(group_id) => self
                .release_id_for_source(tracker, group_id)
                .await?
                .map(|id| id.to_string()),
            None => None,
        };
        if let Some(model) =
            download_release_link::Entity::find_by_id((client.to_owned(), hash.clone()))
                .one(&self.connection)
                .await?
        {
            let mut active = model.into_active_model();
            active.tracker = Set(Some(tracker.into()));
            active.group_id = Set(group_id);
            active.torrent_id = Set(Some(torrent_id));
            active.release_id = Set(release_id.clone());
            active.resolution_state = Set(if linked { "linked" } else { "pending" }.into());
            active.next_retry_at = Set(None);
            active.error_code = Set(None);
            active.error_message = Set(None);
            active.last_seen_at = Set(now.clone());
            active.updated_at = Set(now);
            active.update(&self.connection).await?;
        } else {
            download_release_link::ActiveModel {
                client: Set(client.into()),
                info_hash: Set(hash),
                announce_host: Set(None),
                torrent_name: Set(None),
                tracker: Set(Some(tracker.into())),
                group_id: Set(group_id),
                torrent_id: Set(Some(torrent_id)),
                release_id: Set(release_id),
                resolution_state: Set(if linked { "linked" } else { "pending" }.into()),
                attempts: Set(0),
                next_retry_at: Set(None),
                error_code: Set(None),
                error_message: Set(None),
                first_seen_at: Set(now.clone()),
                last_seen_at: Set(now.clone()),
                updated_at: Set(now),
                present: Set(true),
                missing_since: Set(None),
                library_added_at: Set(None),
                completed_at: Set(None),
                observed_json: Set(None),
                observed_at: Set(None),
                client_added_at: Set(None),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    pub async fn due_links(&self, limit: i64) -> Result<Vec<DownloadReleaseLink>> {
        let now = Utc::now().to_rfc3339();
        let models = download_release_link::Entity::find()
            .filter(download_release_link::Column::Tracker.is_not_null())
            .filter(download_release_link::Column::ResolutionState.eq("pending"))
            .filter(
                Condition::any()
                    .add(download_release_link::Column::NextRetryAt.is_null())
                    .add(download_release_link::Column::NextRetryAt.lte(now)),
            )
            .order_by_asc(download_release_link::Column::UpdatedAt)
            .limit(limit.max(0) as u64)
            .all(&self.connection)
            .await?;
        Ok(models.into_iter().map(link_from_model).collect())
    }

    pub async fn links_for_tracker_hash(
        &self,
        tracker: &str,
        info_hash: &str,
    ) -> Result<Vec<DownloadReleaseLink>> {
        Ok(download_release_link::Entity::find()
            .filter(download_release_link::Column::Tracker.eq(tracker))
            .filter(download_release_link::Column::InfoHash.eq(info_hash.to_ascii_lowercase()))
            .filter(
                Condition::any()
                    .add(download_release_link::Column::ResolutionState.is_in([
                        "pending",
                        "resolving",
                        "failed",
                    ]))
                    .add(
                        Condition::all()
                            .add(download_release_link::Column::ResolutionState.eq("linked"))
                            .add(
                                Condition::any()
                                    .add(download_release_link::Column::GroupId.is_null())
                                    .add(download_release_link::Column::TorrentId.is_null()),
                            ),
                    ),
            )
            .all(&self.connection)
            .await?
            .into_iter()
            .map(link_from_model)
            .collect())
    }

    pub async fn unregistered_downloads(&self) -> Result<Vec<DownloadReleaseLink>> {
        Ok(download_release_link::Entity::find()
            .filter(download_release_link::Column::Present.eq(true))
            .filter(download_release_link::Column::LibraryAddedAt.is_not_null())
            .filter(download_release_link::Column::ErrorCode.eq("torrent_unregistered"))
            .filter(download_release_link::Column::TorrentName.is_not_null())
            .order_by_desc(download_release_link::Column::ClientAddedAt)
            .order_by_asc(download_release_link::Column::Client)
            .order_by_asc(download_release_link::Column::InfoHash)
            .all(&self.connection)
            .await?
            .into_iter()
            .map(link_from_model)
            .collect())
    }

    pub async fn next_download_for_automatic_match(
        &self,
        matcher_version: i32,
    ) -> Result<Option<DownloadReleaseLink>> {
        let error_prefix = format!("automatic_match_v{matcher_version}_");
        let now = Utc::now();
        let models = download_release_link::Entity::find()
            .filter(download_release_link::Column::Present.eq(true))
            .filter(download_release_link::Column::ReleaseId.is_null())
            .filter(download_release_link::Column::TorrentName.is_not_null())
            .filter(download_release_link::Column::ResolutionState.eq("unconfigured"))
            .order_by_asc(download_release_link::Column::UpdatedAt)
            .all(&self.connection)
            .await?;
        Ok(models
            .into_iter()
            .find(|model| {
                model
                    .error_code
                    .as_deref()
                    .is_none_or(|code| !code.starts_with(&error_prefix))
                    || model
                        .next_retry_at
                        .as_deref()
                        .and_then(|value| parse_timestamp(value).ok())
                        .is_some_and(|retry_at| retry_at <= now)
            })
            .map(link_from_model))
    }

    pub async fn list_release_summaries(&self) -> Result<Vec<ReleaseSummary>> {
        canonical_release::Entity::find()
            .all(&self.connection)
            .await?
            .into_iter()
            .map(|model| {
                let id = Uuid::parse_str(&model.id)?;
                let mut summary: ReleaseSummary = serde_json::from_value(model.metadata_json)?;
                summary.id = Some(id);
                Ok(summary)
            })
            .collect()
    }

    pub async fn set_automatic_release_match(
        &self,
        client: &str,
        info_hash: &str,
        release_id: Uuid,
        tracker_hint: Option<&str>,
    ) -> Result<()> {
        let Some(model) = download_release_link::Entity::find_by_id((
            client.to_owned(),
            info_hash.to_ascii_lowercase(),
        ))
        .one(&self.connection)
        .await?
        else {
            return Ok(());
        };
        let mut active = model.into_active_model();
        if let Some(tracker) = tracker_hint {
            active.tracker = Set(Some(tracker.to_owned()));
        }
        active.release_id = Set(Some(release_id.to_string()));
        active.resolution_state = Set("linked".into());
        active.attempts = Set(0);
        active.next_retry_at = Set(None);
        active.error_code = Set(None);
        active.error_message = Set(None);
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        Ok(())
    }

    pub async fn set_automatic_match_review(
        &self,
        client: &str,
        info_hash: &str,
        matcher_version: i32,
        outcome: &str,
        message: &str,
    ) -> Result<()> {
        let Some(model) = download_release_link::Entity::find_by_id((
            client.to_owned(),
            info_hash.to_ascii_lowercase(),
        ))
        .one(&self.connection)
        .await?
        else {
            return Ok(());
        };
        if model.release_id.is_some() {
            return Ok(());
        }
        let mut active = model.into_active_model();
        let retry_at = Utc::now() + chrono::Duration::days(7);
        active.error_code = Set(Some(format!(
            "automatic_match_v{matcher_version}_{outcome}"
        )));
        active.error_message = Set(Some(message.chars().take(500).collect()));
        active.next_retry_at = Set(Some(retry_at.to_rfc3339()));
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        Ok(())
    }

    pub async fn recover_resolving_links(&self) -> Result<()> {
        let models = download_release_link::Entity::find()
            .filter(download_release_link::Column::ResolutionState.eq("resolving"))
            .all(&self.connection)
            .await?;
        for model in models {
            let mut active = model.into_active_model();
            active.resolution_state = Set("pending".into());
            active.next_retry_at = Set(None);
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn complete_client_scan(
        &self,
        client: &str,
        scan_started_at: DateTime<Utc>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let transaction = self.begin_write().await?;
        let stale = download_release_link::Entity::find()
            .filter(download_release_link::Column::Client.eq(client))
            .filter(download_release_link::Column::LastSeenAt.lt(scan_started_at.to_rfc3339()))
            .all(&transaction)
            .await?;
        for model in stale {
            if model.library_added_at.is_some() {
                let has_missing_since = model.missing_since.is_some();
                let mut active = model.into_active_model();
                active.present = Set(false);
                if !has_missing_since {
                    active.missing_since = Set(Some(now.clone()));
                }
                active.update(&transaction).await?;
            } else {
                model.delete(&transaction).await?;
            }
        }
        if let Some(model) = download_client_scan::Entity::find_by_id(client)
            .one(&transaction)
            .await?
        {
            let mut active = model.into_active_model();
            active.last_successful_at = Set(now);
            active.update(&transaction).await?;
        } else {
            download_client_scan::ActiveModel {
                client: Set(client.into()),
                last_successful_at: Set(now),
            }
            .insert(&transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn set_link_resolving(&self, client: &str, info_hash: &str) -> Result<()> {
        if let Some(model) =
            download_release_link::Entity::find_by_id((client.to_owned(), info_hash.to_owned()))
                .one(&self.connection)
                .await?
            && model.resolution_state != "linked"
        {
            let mut active = model.into_active_model();
            active.resolution_state = Set("resolving".into());
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn set_linked(
        &self,
        client: &str,
        info_hash: &str,
        canonical: &CanonicalTorrent,
    ) -> Result<()> {
        let release_id = match canonical.release.id {
            Some(id) => Some(id.to_string()),
            None => self
                .release_id_for_source(&canonical.release.tracker, canonical.release.group_id)
                .await?
                .map(|id| id.to_string()),
        };
        if let Some(model) =
            download_release_link::Entity::find_by_id((client.to_owned(), info_hash.to_owned()))
                .one(&self.connection)
                .await?
        {
            let mut active = model.into_active_model();
            active.tracker = Set(Some(canonical.release.tracker.clone()));
            active.group_id = Set(Some(canonical.release.group_id));
            active.torrent_id = Set(Some(canonical.variant.torrent_id));
            active.release_id = Set(release_id);
            active.resolution_state = Set("linked".into());
            active.next_retry_at = Set(None);
            active.error_code = Set(None);
            active.error_message = Set(None);
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn set_link_failure(
        &self,
        client: &str,
        info_hash: &str,
        not_found: bool,
        error_code: &str,
        message: &str,
    ) -> Result<()> {
        let model =
            download_release_link::Entity::find_by_id((client.to_owned(), info_hash.to_owned()))
                .one(&self.connection)
                .await?
                .context("download release link disappeared while recording failure")?;
        let delay =
            chrono::Duration::seconds((30_i64 * (1_i64 << model.attempts.min(7))).min(3600));
        let attempts = model.attempts;
        let mut active = model.into_active_model();
        active.resolution_state = Set(if not_found { "not_found" } else { "failed" }.into());
        active.attempts = Set(if not_found { attempts } else { attempts + 1 });
        active.next_retry_at = Set((!not_found).then(|| (Utc::now() + delay).to_rfc3339()));
        active.error_code = Set(Some(error_code.into()));
        active.error_message = Set(Some(message.chars().take(500).collect()));
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        Ok(())
    }

    pub async fn defer_link_resolution(
        &self,
        client: &str,
        info_hash: &str,
        message: &str,
    ) -> Result<()> {
        if let Some(model) =
            download_release_link::Entity::find_by_id((client.to_owned(), info_hash.to_owned()))
                .one(&self.connection)
                .await?
            && model.resolution_state != "linked"
        {
            let mut active = model.into_active_model();
            active.resolution_state = Set("pending".into());
            active.next_retry_at = Set(None);
            active.error_code = Set(Some("provider_wait".into()));
            active.error_message = Set(Some(message.chars().take(500).collect()));
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn defer_track_index(
        &self,
        tracker: &str,
        group_id: i64,
        message: &str,
    ) -> Result<()> {
        if let Some(model) = release_track_index::Entity::find_by_id((tracker.to_owned(), group_id))
            .one(&self.connection)
            .await?
        {
            let mut active = model.into_active_model();
            active.state = Set("pending".into());
            active.next_retry_at = Set(None);
            active.error_message = Set(Some(message.chars().take(500).collect()));
            active.updated_at = Set(Utc::now().to_rfc3339());
            active.update(&self.connection).await?;
        }
        Ok(())
    }

    pub async fn retry_link(&self, client: &str, info_hash: &str) -> Result<bool> {
        let model = download_release_link::Entity::find_by_id((
            client.to_owned(),
            info_hash.to_ascii_lowercase(),
        ))
        .one(&self.connection)
        .await?;
        let Some(model) = model.filter(|model| {
            model.tracker.is_some()
                && matches!(model.resolution_state.as_str(), "failed" | "not_found")
        }) else {
            return Ok(false);
        };
        let mut active = model.into_active_model();
        active.resolution_state = Set("pending".into());
        active.next_retry_at = Set(None);
        active.error_code = Set(None);
        active.error_message = Set(None);
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
        Ok(true)
    }

    pub async fn get_link(
        &self,
        client: &str,
        info_hash: &str,
    ) -> Result<Option<DownloadReleaseLink>> {
        Ok(download_release_link::Entity::find_by_id((
            client.to_owned(),
            info_hash.to_ascii_lowercase(),
        ))
        .one(&self.connection)
        .await?
        .map(link_from_model))
    }

    pub async fn index_counts(&self) -> Result<DownloadIndexCounts> {
        let count = |states: Vec<&'static str>| {
            download_release_link::Entity::find()
                .filter(download_release_link::Column::Present.eq(true))
                .filter(download_release_link::Column::ResolutionState.is_in(states))
                .count(&self.connection)
        };
        Ok(DownloadIndexCounts {
            linked: i64::try_from(count(vec!["linked"]).await?).unwrap_or(i64::MAX),
            pending: i64::try_from(count(vec!["pending"]).await?).unwrap_or(i64::MAX),
            resolving: i64::try_from(count(vec!["resolving"]).await?).unwrap_or(i64::MAX),
            failed: i64::try_from(count(vec!["failed", "not_found"]).await?).unwrap_or(i64::MAX),
            unconfigured: i64::try_from(count(vec!["unconfigured"]).await?).unwrap_or(i64::MAX),
        })
    }

    pub async fn list_indexed_downloads(
        &self,
        client: Option<&str>,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<IndexedDownload>, i64)> {
        let mut query = download_release_link::Entity::find()
            .filter(download_release_link::Column::Present.eq(true))
            .filter(download_release_link::Column::ResolutionState.eq("linked"))
            .filter(download_release_link::Column::ReleaseId.is_not_null());
        if let Some(client) = client {
            query = query.filter(download_release_link::Column::Client.eq(client));
        }
        let total = i64::try_from(query.clone().count(&self.connection).await?).unwrap_or(i64::MAX);
        let links = query
            .order_by_desc(download_release_link::Column::ClientAddedAt)
            .order_by_asc(download_release_link::Column::Client)
            .order_by_asc(download_release_link::Column::InfoHash)
            .offset(offset)
            .limit(limit)
            .all(&self.connection)
            .await?;
        let metadata = self.resolved_download_metadata(&links).await?;
        let mut indexed = Vec::with_capacity(links.len());
        for link in links {
            let exact = link
                .tracker
                .clone()
                .zip(link.torrent_id)
                .and_then(|identity| metadata.canonical_by_identity.get(&identity).cloned());
            let (release, variant) = if let Some(canonical) = exact {
                let canonical = canonical_from_model(canonical)?;
                let Cached {
                    value,
                    fetched_at,
                    expires_at,
                } = canonical;
                (
                    Cached {
                        value: value.release,
                        fetched_at,
                        expires_at,
                    },
                    Some(value.variant),
                )
            } else {
                let Some(release_id) = link.release_id.as_deref() else {
                    continue;
                };
                let Some(release) = metadata.release_by_id.get(release_id).cloned() else {
                    continue;
                };
                (cached_release_from_model(release)?, None)
            };
            indexed.push(IndexedDownload {
                release,
                variant,
                client: link.client,
                info_hash: link.info_hash,
                live: link.observed_json.map(serde_json::from_value).transpose()?,
                observed_at: link
                    .observed_at
                    .as_deref()
                    .map(parse_timestamp)
                    .transpose()?,
            });
        }
        Ok((indexed, total))
    }

    pub async fn list_library_records(&self) -> Result<Vec<LibraryRecord>> {
        let links = download_release_link::Entity::find()
            .filter(download_release_link::Column::ResolutionState.eq("linked"))
            .filter(download_release_link::Column::LibraryAddedAt.is_not_null())
            .all(&self.connection)
            .await?;
        self.library_records_from_links(links).await
    }

    pub async fn list_library_records_for_releases(
        &self,
        release_ids: &[Uuid],
    ) -> Result<Vec<LibraryRecord>> {
        if release_ids.is_empty() {
            return Ok(Vec::new());
        }
        let links = download_release_link::Entity::find()
            .filter(download_release_link::Column::ResolutionState.eq("linked"))
            .filter(download_release_link::Column::LibraryAddedAt.is_not_null())
            .filter(
                download_release_link::Column::ReleaseId
                    .is_in(release_ids.iter().map(ToString::to_string)),
            )
            .all(&self.connection)
            .await?;
        self.library_records_from_links(links).await
    }

    async fn library_records_from_links(
        &self,
        links: Vec<download_release_link::Model>,
    ) -> Result<Vec<LibraryRecord>> {
        let metadata = self.resolved_download_metadata(&links).await?;
        let mut records = Vec::with_capacity(links.len());
        for link in links {
            let Some(library_added_at) = link.library_added_at.as_deref() else {
                continue;
            };
            let exact = link
                .tracker
                .clone()
                .zip(link.torrent_id)
                .and_then(|identity| metadata.canonical_by_identity.get(&identity).cloned());
            let (release, variant) = if let Some(canonical) = exact {
                let canonical = canonical_from_model(canonical)?;
                let Cached {
                    value,
                    fetched_at,
                    expires_at,
                } = canonical;
                (
                    Cached {
                        value: value.release,
                        fetched_at,
                        expires_at,
                    },
                    Some(value.variant),
                )
            } else {
                let Some(release_id) = link.release_id.as_deref() else {
                    continue;
                };
                let Some(release) = metadata.release_by_id.get(release_id).cloned() else {
                    continue;
                };
                (cached_release_from_model(release)?, None)
            };
            let library_added_at = parse_timestamp(library_added_at)?;
            records.push(LibraryRecord {
                release,
                variant,
                client: link.client,
                info_hash: link.info_hash,
                present: link.present,
                library_added_at,
                completed_at: link
                    .completed_at
                    .as_deref()
                    .map(parse_timestamp)
                    .transpose()?
                    .unwrap_or(library_added_at),
                last_seen_at: parse_timestamp(&link.last_seen_at)?,
                missing_since: link
                    .missing_since
                    .as_deref()
                    .map(parse_timestamp)
                    .transpose()?,
            });
        }
        Ok(records)
    }

    async fn resolved_download_metadata(
        &self,
        links: &[download_release_link::Model],
    ) -> Result<ResolvedDownloadMetadata> {
        let identities = links
            .iter()
            .filter_map(|link| Some((link.tracker.clone()?, link.torrent_id?)))
            .collect::<Vec<_>>();
        let mut canonical_by_identity = HashMap::new();
        for chunk in identities.chunks(300) {
            if chunk.is_empty() {
                continue;
            }
            let condition =
                chunk
                    .iter()
                    .fold(Condition::any(), |condition, (tracker, torrent_id)| {
                        condition.add(
                            Condition::all()
                                .add(canonical_torrent::Column::Tracker.eq(tracker.clone()))
                                .add(canonical_torrent::Column::TorrentId.eq(*torrent_id)),
                        )
                    });
            for model in canonical_torrent::Entity::find()
                .filter(condition)
                .all(&self.connection)
                .await?
            {
                canonical_by_identity.insert((model.tracker.clone(), model.torrent_id), model);
            }
        }
        let release_ids = links
            .iter()
            .filter_map(|link| link.release_id.clone())
            .collect::<Vec<_>>();
        let release_by_id = if release_ids.is_empty() {
            HashMap::new()
        } else {
            canonical_release::Entity::find()
                .filter(canonical_release::Column::Id.is_in(release_ids))
                .all(&self.connection)
                .await?
                .into_iter()
                .map(|model| (model.id.clone(), model))
                .collect()
        };
        Ok(ResolvedDownloadMetadata {
            canonical_by_identity,
            release_by_id,
        })
    }

    pub async fn last_successful_download_scan(&self) -> Result<Option<DateTime<Utc>>> {
        download_client_scan::Entity::find()
            .order_by_asc(download_client_scan::Column::LastSuccessfulAt)
            .one(&self.connection)
            .await?
            .map(|model| parse_timestamp(&model.last_successful_at))
            .transpose()
    }

    async fn add_event(&self, id: Uuid, state: &DownloadState, detail: Option<&str>) -> Result<()> {
        self.add_event_on(&self.connection, id, state, detail).await
    }

    async fn add_event_on<C: ConnectionTrait>(
        &self,
        connection: &C,
        id: Uuid,
        state: &DownloadState,
        detail: Option<&str>,
    ) -> Result<()> {
        download_event::ActiveModel {
            id: Default::default(),
            job_id: Set(id.to_string()),
            state: Set(state.as_str().into()),
            detail: Set(detail.map(str::to_owned)),
            created_at: Set(Utc::now().to_rfc3339()),
        }
        .insert(connection)
        .await?;
        Ok(())
    }
}

fn snapshot_from_model<T: DeserializeOwned>(model: tracker_snapshot::Model) -> Result<Cached<T>> {
    Ok(Cached {
        value: serde_json::from_value(model.normalized_json)?,
        fetched_at: parse_timestamp(&model.fetched_at)?,
        expires_at: parse_timestamp(&model.expires_at)?,
    })
}

fn provider_state_from_model(model: provider_state::Model) -> Result<StoredProviderState> {
    Ok(StoredProviderState {
        id: model.id,
        display_name: model.display_name,
        kind: model.kind,
        state: ProviderCircuitState::from_str(&model.state)?,
        reason_code: model.reason_code,
        message: model.message,
        last_request_at: model
            .last_request_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        last_success_at: model
            .last_success_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        last_failure_at: model
            .last_failure_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        retry_at: model.retry_at.as_deref().map(parse_timestamp).transpose()?,
        last_background_request_at: model
            .last_background_request_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
        consecutive_failures: u32::try_from(model.consecutive_failures).unwrap_or(u32::MAX),
        minimum_interval_ms: u64::try_from(model.minimum_interval_ms).unwrap_or_default(),
        background_minimum_interval_ms: u64::try_from(model.background_minimum_interval_ms)
            .unwrap_or_default(),
        max_concurrency: u32::try_from(model.max_concurrency).unwrap_or(1).max(1),
    })
}

fn stored_background_job(model: &background_job::Model) -> Result<StoredBackgroundJob> {
    Ok(StoredBackgroundJob {
        id: Uuid::parse_str(&model.id)?,
        kind: model.kind.clone(),
        payload: model.payload_json.clone(),
        provider_id: model.provider_id.clone(),
        lane: model.lane.clone(),
        attempts: u32::try_from(model.attempts).unwrap_or(u32::MAX),
        max_attempts: u32::try_from(model.max_attempts).unwrap_or(u32::MAX),
    })
}

fn background_provider_is_due(
    job: &background_job::Model,
    states: &std::collections::HashMap<String, provider_state::Model>,
    running: &std::collections::HashSet<String>,
    now: DateTime<Utc>,
) -> bool {
    let Some(provider_id) = job.provider_id.as_deref() else {
        return true;
    };
    if running.contains(provider_id) {
        return false;
    }
    let Some(state) = states.get(provider_id) else {
        return true;
    };
    if state.state == "cooldown"
        && state
            .retry_at
            .as_deref()
            .and_then(|value| parse_timestamp(value).ok())
            .is_some_and(|retry_at| retry_at > now)
    {
        return false;
    }
    if matches!(state.state.as_str(), "blocked" | "paused") {
        return true;
    }
    state
        .last_background_request_at
        .as_deref()
        .and_then(|value| parse_timestamp(value).ok())
        .map(|last| {
            let interval =
                chrono::Duration::milliseconds(state.background_minimum_interval_ms.max(0));
            last + interval <= now
        })
        .unwrap_or(true)
}

fn background_job_status(model: background_job::Model) -> Result<BackgroundJobStatus> {
    let state = BackgroundJobState::from_str(&model.state)?;
    Ok(BackgroundJobStatus {
        id: Uuid::parse_str(&model.id)?,
        deduplication_key: model.deduplication_key,
        kind: model.kind,
        state,
        provider_id: model.provider_id,
        lane: model.lane,
        priority: model.priority,
        attempts: u32::try_from(model.attempts).unwrap_or(u32::MAX),
        deferrals: u64::try_from(model.deferrals).unwrap_or(u64::MAX),
        max_attempts: u32::try_from(model.max_attempts).unwrap_or(u32::MAX),
        next_run_at: model
            .next_run_at
            .map(|value| parse_timestamp(&value))
            .transpose()?,
        lease_until: model
            .lease_until
            .map(|value| parse_timestamp(&value))
            .transpose()?,
        progress_completed: u64::try_from(model.progress_completed).unwrap_or_default(),
        progress_total: model
            .progress_total
            .and_then(|value| u64::try_from(value).ok()),
        progress_message: model.progress_message,
        last_error_code: model.last_error_code,
        last_error_message: model.last_error_message,
        parent_id: model
            .parent_id
            .map(|value| Uuid::parse_str(&value))
            .transpose()?,
        created_at: parse_timestamp(&model.created_at)?,
        updated_at: parse_timestamp(&model.updated_at)?,
        started_at: model
            .started_at
            .map(|value| parse_timestamp(&value))
            .transpose()?,
        finished_at: model
            .finished_at
            .map(|value| parse_timestamp(&value))
            .transpose()?,
        can_cancel: matches!(
            state,
            BackgroundJobState::Pending
                | BackgroundJobState::Running
                | BackgroundJobState::Retrying
                | BackgroundJobState::Waiting
        ),
        can_retry: matches!(
            state,
            BackgroundJobState::Failed | BackgroundJobState::Cancelled
        ),
    })
}

fn channel_run_trigger(value: &ChannelRunTrigger) -> &'static str {
    match value {
        ChannelRunTrigger::Scheduled => "scheduled",
        ChannelRunTrigger::Manual => "manual",
    }
}

fn channel_run_status(value: &ChannelRunStatus) -> &'static str {
    match value {
        ChannelRunStatus::Running => "running",
        ChannelRunStatus::Successful => "successful",
        ChannelRunStatus::Partial => "partial",
        ChannelRunStatus::Failed => "failed",
    }
}

fn channel_run_phase(value: &ChannelRunPhase) -> &'static str {
    match value {
        ChannelRunPhase::Discovering => "discovering",
        ChannelRunPhase::Matching => "matching",
        ChannelRunPhase::WaitingProvider => "waiting_provider",
        ChannelRunPhase::Planning => "planning",
        ChannelRunPhase::Saving => "saving",
    }
}

fn channel_pack_decision(value: &ChannelPackDecision) -> &'static str {
    match value {
        ChannelPackDecision::Open => "open",
        ChannelPackDecision::Accepted => "accepted",
        ChannelPackDecision::Rejected => "rejected",
    }
}

fn channel_run_from_model(model: channel_run::Model) -> Result<ChannelRun> {
    Ok(ChannelRun {
        id: Uuid::parse_str(&model.id)?,
        channel_id: model.channel_id,
        trigger: match model.trigger.as_str() {
            "scheduled" => ChannelRunTrigger::Scheduled,
            _ => ChannelRunTrigger::Manual,
        },
        status: match model.status.as_str() {
            "successful" => ChannelRunStatus::Successful,
            "partial" => ChannelRunStatus::Partial,
            "failed" => ChannelRunStatus::Failed,
            _ => ChannelRunStatus::Running,
        },
        phase: match model.phase.as_deref() {
            Some("discovering") => Some(ChannelRunPhase::Discovering),
            Some("matching") => Some(ChannelRunPhase::Matching),
            Some("waiting_provider") => Some(ChannelRunPhase::WaitingProvider),
            Some("planning") => Some(ChannelRunPhase::Planning),
            Some("saving") => Some(ChannelRunPhase::Saving),
            _ => None,
        },
        progress_completed: model.progress_completed.max(0) as u32,
        progress_total: model.progress_total.map(|value| value.max(0) as u32),
        progress_message: model.progress_message,
        retry_at: model.retry_at.as_deref().map(parse_timestamp).transpose()?,
        pack_id: model.pack_id.as_deref().map(Uuid::parse_str).transpose()?,
        error: model.error,
        started_at: parse_timestamp(&model.started_at)?,
        updated_at: if model.updated_at.is_empty() {
            parse_timestamp(&model.started_at)?
        } else {
            parse_timestamp(&model.updated_at)?
        },
        finished_at: model
            .finished_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
    })
}

fn channel_pack_from_model(
    model: channel_pack::Model,
    mut items: Vec<ChannelPackItem>,
    current_fingerprint: &str,
) -> Result<ChannelPack> {
    for item in &mut items {
        if let Some(plan) = &mut item.plan
            && plan.use_token
            && plan.token_cost == 0
            && let Some(token_cost) =
                crate::model::ReleasePreferences::token_cost(&plan.tracker, plan.size)
        {
            plan.token_cost = token_cost;
        }
    }
    let mut summary = crate::model::ChannelPlanSummary::default();
    for item in &items {
        if matches!(
            item.plan_state,
            crate::model::PackItemPlanState::Executable
                | crate::model::PackItemPlanState::CleanupReady
                | crate::model::PackItemPlanState::Submitted
        ) || (item.replacement.is_some()
            && item.plan_state == crate::model::PackItemPlanState::AlreadyDownloading)
        {
            summary.executable += 1;
            if let Some(plan) = &item.plan {
                summary.total_size += plan.size.unwrap_or_default();
                summary.token_uses += plan.token_cost as usize;
                *summary.by_tracker.entry(plan.tracker.clone()).or_default() += 1;
            } else if let Some(replacement) = &item.replacement {
                *summary
                    .by_tracker
                    .entry(replacement.tracker.clone())
                    .or_default() += 1;
            }
        } else {
            summary.skipped += 1;
            let reason = item
                .reason
                .clone()
                .unwrap_or_else(|| format!("{:?}", item.plan_state).to_ascii_lowercase());
            *summary.by_reason.entry(reason).or_default() += 1;
        }
    }
    let decision = match model.decision.as_str() {
        "accepted" => ChannelPackDecision::Accepted,
        "rejected" => ChannelPackDecision::Rejected,
        _ => ChannelPackDecision::Open,
    };
    Ok(ChannelPack {
        id: Uuid::parse_str(&model.id)?,
        channel_id: model.channel_id,
        decision: decision.clone(),
        partial: model.partial,
        source_title: model.source_title,
        plan_version: model.plan_version,
        plan_stale: decision == ChannelPackDecision::Open
            && !current_fingerprint.is_empty()
            && model.preference_fingerprint != current_fingerprint,
        summary,
        items,
        created_at: parse_timestamp(&model.created_at)?,
        decided_at: model
            .decided_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
    })
}

fn canonical_from_model(model: canonical_torrent::Model) -> Result<Cached<CanonicalTorrent>> {
    Ok(Cached {
        value: serde_json::from_value(model.canonical_json)?,
        fetched_at: parse_timestamp(&model.fetched_at)?,
        expires_at: parse_timestamp(&model.expires_at)?,
    })
}

fn metadata_completeness(release: &ReleaseSummary) -> usize {
    usize::from(!release.title.trim().is_empty())
        + usize::from(
            release
                .artist
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
        )
        + usize::from(release.year.is_some())
        + usize::from(
            release
                .artwork
                .as_deref()
                .is_some_and(|value| value.starts_with("http")),
        ) * 2
        + usize::from(
            release
                .release_type
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
        )
        + release
            .artists
            .iter()
            .filter(|artist| artist.source == ArtistCreditSource::Structured)
            .count()
}

fn title_field_score(release: &ReleaseSummary) -> usize {
    let normalized = crate::release_matcher::normalized(&release.title);
    usize::from(!normalized.is_empty()) * 10
        + usize::from(!normalized.starts_with("release ")) * 5
        + normalized.len().min(100)
}

fn artist_field_score(release: &ReleaseSummary) -> usize {
    usize::from(
        release
            .artist
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
    ) * 10
        + release
            .artists
            .iter()
            .filter(|artist| artist.source == ArtistCreditSource::Structured)
            .count()
}

fn artwork_field_score(release: &ReleaseSummary) -> usize {
    release.artwork.as_deref().map_or(0, |artwork| {
        usize::from(artwork.starts_with("https://")) * 20
            + usize::from(artwork.starts_with("http://")) * 10
            + artwork.len().min(100)
    })
}

fn release_type_field_score(release: &ReleaseSummary) -> usize {
    usize::from(
        release
            .release_type
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
    )
}

fn link_from_model(model: download_release_link::Model) -> DownloadReleaseLink {
    DownloadReleaseLink {
        client: model.client,
        info_hash: model.info_hash,
        announce_host: model.announce_host,
        torrent_name: model.torrent_name,
        tracker: model.tracker,
        torrent_id: model.torrent_id,
        resolution_state: model.resolution_state,
    }
}

fn cached_release_from_model(model: canonical_release::Model) -> Result<Cached<ReleaseSummary>> {
    let id = Uuid::parse_str(&model.id)?;
    let fetched_at = parse_timestamp(&model.updated_at)?;
    let mut value: ReleaseSummary = serde_json::from_value(model.metadata_json)?;
    value.id = Some(id);
    Ok(Cached {
        value,
        fetched_at,
        expires_at: DateTime::<Utc>::MAX_UTC,
    })
}

fn release_downloads_from_links(
    links: Vec<download_release_link::Model>,
) -> Result<Vec<ReleaseDownload>> {
    links
        .into_iter()
        .filter_map(|link| {
            let observed = link.observed_json.clone()?;
            Some((link, observed))
        })
        .map(|(link, observed)| {
            Ok(ReleaseDownload {
                name: link.torrent_name.unwrap_or_else(|| link.info_hash.clone()),
                tracker: link.tracker,
                in_library: link.library_added_at.is_some(),
                live: serde_json::from_value(observed)?,
            })
        })
        .collect()
}

fn artist_sort_name(name: &str) -> String {
    let normalized = name.trim().to_lowercase();
    normalized
        .strip_prefix("the ")
        .unwrap_or(&normalized)
        .to_owned()
}

fn job_from_model(model: download_job::Model) -> Result<DownloadJob> {
    Ok(DownloadJob {
        id: Uuid::parse_str(&model.id)?,
        tracker: model.tracker,
        torrent_id: model.torrent_id,
        group_id: model.group_id,
        profile: model.profile,
        use_token: model.use_token,
        info_hash: model.info_hash,
        name: model.name,
        state: DownloadState::from_str(&model.state)?,
        progress: model.progress,
        download_speed: model.download_speed,
        upload_speed: model.upload_speed,
        eta: model.eta,
        error_code: model.error_code,
        error_message: model.error_message,
        created_at: parse_timestamp(&model.created_at)?,
        updated_at: parse_timestamp(&model.updated_at)?,
    })
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn import_state_for_link(
    link: &download_release_link::Model,
    live: Option<&LiveDownloadStatus>,
) -> (ImportTaskState, Option<String>) {
    if link.present && live.is_some_and(|value| value.progress < 1.0) {
        return (ImportTaskState::Downloading, None);
    }
    if matches!(link.resolution_state.as_str(), "pending" | "resolving") {
        return (
            ImportTaskState::Resolving,
            Some("Resolving the tracker release".into()),
        );
    }
    if link.error_code.is_some() || link.release_id.is_none() {
        return (
            ImportTaskState::NeedsReview,
            link.error_message
                .clone()
                .or_else(|| Some("The download is not linked to a canonical release".into())),
        );
    }
    if link.completed_at.is_some() || link.library_added_at.is_some() {
        return (ImportTaskState::Complete, None);
    }
    (ImportTaskState::Ready, None)
}

fn import_task_state_name(state: &ImportTaskState) -> &'static str {
    match state {
        ImportTaskState::Downloading => "downloading",
        ImportTaskState::Resolving => "resolving",
        ImportTaskState::NeedsReview => "needs_review",
        ImportTaskState::Ready => "ready",
        ImportTaskState::Processing => "processing",
        ImportTaskState::Complete => "complete",
        ImportTaskState::Blocked => "blocked",
        ImportTaskState::Failed => "failed",
        ImportTaskState::Dismissed => "dismissed",
    }
}

fn parse_import_task_state(value: &str) -> Result<ImportTaskState> {
    Ok(match value {
        "downloading" => ImportTaskState::Downloading,
        "resolving" => ImportTaskState::Resolving,
        "needs_review" => ImportTaskState::NeedsReview,
        "ready" => ImportTaskState::Ready,
        "processing" => ImportTaskState::Processing,
        "complete" => ImportTaskState::Complete,
        "blocked" => ImportTaskState::Blocked,
        "failed" => ImportTaskState::Failed,
        "dismissed" => ImportTaskState::Dismissed,
        _ => anyhow::bail!("unknown import task state {value}"),
    })
}

fn parse_cleanup_mode(value: &str) -> Result<ImportCleanupMode> {
    Ok(match value {
        "keep" => ImportCleanupMode::Keep,
        "remove_torrent" => ImportCleanupMode::RemoveTorrent,
        "delete_files" => ImportCleanupMode::DeleteFiles,
        _ => anyhow::bail!("unknown import cleanup mode {value}"),
    })
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use sea_orm::{
        ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
    };
    use tempfile::tempdir;

    use crate::{
        entity::{background_job, download_release_link, release_track_index},
        model::{
            CanonicalTorrent, ChannelPackItem, ChannelRunStatus, ChannelRunTrigger,
            ClientDownloadState, ImportCleanupMode, ImportTaskState, LiveDownloadStatus,
            PackItemPlanState, ProviderCircuitState, RecommendationMatchState,
            RecommendationSource, ReleaseSummary, TorrentVariant, TrumpedDownloadRef,
        },
        plex::PlexScanTarget,
        tracker::fallback_artist_credit,
    };

    use super::{
        CreateReplacementImport, Database, DownloadObservation, EnqueueBackgroundJob,
        StoredProviderState,
    };

    #[tokio::test]
    async fn replacement_imports_persist_exact_supersessions_without_backfill_cleanup() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("imports.sqlite"))
            .await
            .expect("database");
        let source = TrumpedDownloadRef {
            client: "music".into(),
            info_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            name: "Artist - Old Album".into(),
            tracker: "ops".into(),
        };
        let id = db
            .create_replacement_import(CreateReplacementImport {
                download_job_id: None,
                target_client: Some("music"),
                target_info_hash: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                release_id: None,
                tracker: "ops",
                torrent_id: 42,
                display_name: "Album",
                target_complete: true,
                sources: &[source],
                cleanup_mode: ImportCleanupMode::DeleteFiles,
            })
            .await
            .expect("replacement import");

        let page = db.list_imports(100, 0).await.expect("imports page");
        let task = page.items.iter().find(|task| task.id == id).expect("task");
        assert_eq!(task.state, ImportTaskState::Ready);
        assert!(!task.baseline);
        assert_eq!(task.supersessions.len(), 1);
        assert_eq!(
            task.supersessions[0].cleanup_mode,
            ImportCleanupMode::DeleteFiles
        );
        assert_eq!(
            task.supersessions[0].source_info_hash,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[tokio::test]
    async fn dependency_events_resume_only_affected_coverage_jobs() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("targeted-resume.sqlite"))
            .await
            .expect("database");
        db.ensure_single_coverage("ops", 1)
            .await
            .expect("first coverage");
        db.ensure_single_coverage("ops", 2)
            .await
            .expect("second coverage");
        background_job::Entity::update_many()
            .col_expr(
                background_job::Column::State,
                sea_orm::sea_query::Expr::value("waiting"),
            )
            .filter(background_job::Column::Kind.eq("compute_single_coverage"))
            .exec(&db.connection)
            .await
            .expect("park coverage jobs");

        assert_eq!(
            db.resume_waiting_single_coverages("ops", &[1])
                .await
                .expect("targeted resume"),
            1
        );
        let first = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq("single-coverage:ops:1:v2"))
            .one(&db.connection)
            .await
            .expect("first query")
            .expect("first job");
        let second = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq("single-coverage:ops:2:v2"))
            .one(&db.connection)
            .await
            .expect("second query")
            .expect("second job");
        assert_eq!(first.state, "pending");
        assert_eq!(second.state, "waiting");

        assert_eq!(
            db.reconcile_waiting_single_coverages()
                .await
                .expect("startup reconciliation"),
            1
        );
        let second = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq("single-coverage:ops:2:v2"))
            .one(&db.connection)
            .await
            .expect("second reconciled query")
            .expect("second reconciled job");
        assert_eq!(second.state, "pending");
    }

    #[tokio::test]
    async fn startup_reconciliation_ensures_waiting_single_track_indexes() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("coverage-index-recovery.sqlite"))
            .await
            .expect("database");
        db.ensure_single_coverage("ops", 42)
            .await
            .expect("single coverage");
        background_job::Entity::update_many()
            .col_expr(
                background_job::Column::State,
                sea_orm::sea_query::Expr::value("waiting"),
            )
            .col_expr(
                background_job::Column::LastErrorMessage,
                sea_orm::sea_query::Expr::value("Waiting for the Single tracklist"),
            )
            .filter(background_job::Column::Kind.eq("compute_single_coverage"))
            .exec(&db.connection)
            .await
            .expect("park coverage job");

        assert_eq!(
            db.reconcile_waiting_single_coverage_track_indexes()
                .await
                .expect("reconcile track index"),
            1
        );
        let index = release_track_index::Entity::find_by_id(("ops".to_owned(), 42))
            .one(&db.connection)
            .await
            .expect("track index query")
            .expect("track index");
        assert_eq!(index.state, "pending");
        let job = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq("track-index:ops:42:v2"))
            .one(&db.connection)
            .await
            .expect("track index job query")
            .expect("track index job");
        assert_eq!(job.state, "pending");
    }

    #[tokio::test]
    async fn successful_recurring_jobs_reset_failure_counters() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("recurring-jobs.sqlite"))
            .await
            .expect("database");
        let id = db
            .enqueue_background_job(EnqueueBackgroundJob {
                deduplication_key: "test:recurring",
                kind: "test_job",
                payload: serde_json::json!({}),
                provider_id: None,
                lane: "maintenance",
                priority: 1,
                max_attempts: 3,
                next_run_at: None,
                parent_id: None,
                recurring_interval_seconds: Some(60),
            })
            .await
            .expect("enqueue recurring job");
        background_job::Entity::update_many()
            .col_expr(
                background_job::Column::Attempts,
                sea_orm::sea_query::Expr::value(2),
            )
            .col_expr(
                background_job::Column::Deferrals,
                sea_orm::sea_query::Expr::value(5),
            )
            .filter(background_job::Column::Id.eq(id.to_string()))
            .exec(&db.connection)
            .await
            .expect("seed counters");

        let claimed = db
            .claim_background_job(
                "maintenance-worker",
                "maintenance",
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("claim recurring job")
            .expect("recurring job");
        db.complete_background_job(claimed.id, "maintenance-worker")
            .await
            .expect("complete recurring job");

        let job = background_job::Entity::find_by_id(id.to_string())
            .one(&db.connection)
            .await
            .expect("query recurring job")
            .expect("recurring job remains");
        assert_eq!(job.state, "pending");
        assert_eq!(job.attempts, 0);
        assert_eq!(job.deferrals, 0);
        assert!(job.next_run_at.is_some());
    }

    #[tokio::test]
    async fn completed_refresh_jobs_reactivate_and_accept_priority_upgrades() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("refresh-jobs.sqlite"))
            .await
            .expect("database");
        let key = "refresh-artist-catalog:ops:10:v1";
        let id = db
            .enqueue_background_job(EnqueueBackgroundJob {
                deduplication_key: key,
                kind: "refresh_artist_catalog",
                payload: serde_json::json!({
                    "tracker": "ops",
                    "artistId": 10,
                    "interactive": false,
                }),
                provider_id: Some("tracker:ops".into()),
                lane: "sync",
                priority: 5,
                max_attempts: 8,
                next_run_at: None,
                parent_id: None,
                recurring_interval_seconds: None,
            })
            .await
            .expect("enqueue refresh");
        background_job::Entity::update_many()
            .col_expr(
                background_job::Column::State,
                sea_orm::sea_query::Expr::value("completed"),
            )
            .filter(background_job::Column::Id.eq(id.to_string()))
            .exec(&db.connection)
            .await
            .expect("complete refresh");

        assert!(
            db.retry_completed_background_job_by_key(key)
                .await
                .expect("reactivate refresh")
        );
        db.enqueue_background_job(EnqueueBackgroundJob {
            deduplication_key: key,
            kind: "refresh_artist_catalog",
            payload: serde_json::json!({
                "tracker": "ops",
                "artistId": 10,
                "interactive": true,
            }),
            provider_id: Some("tracker:ops".into()),
            lane: "sync",
            priority: 35,
            max_attempts: 8,
            next_run_at: None,
            parent_id: None,
            recurring_interval_seconds: None,
        })
        .await
        .expect("upgrade refresh");

        let stored = background_job::Entity::find_by_id(id.to_string())
            .one(&db.connection)
            .await
            .expect("load refresh")
            .expect("refresh exists");
        assert_eq!(stored.state, "pending");
        assert_eq!(stored.priority, 35);
        assert_eq!(
            stored
                .payload_json
                .get("interactive")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            db.background_job_by_key(key)
                .await
                .expect("refresh status")
                .expect("refresh status exists")
                .state,
            crate::model::BackgroundJobState::Pending
        );
    }

    #[tokio::test]
    async fn job_claims_respect_provider_windows_and_in_flight_work() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("provider-claims.sqlite"))
            .await
            .expect("database");
        let mut provider = StoredProviderState {
            id: "tracker:ops".into(),
            display_name: "OPS".into(),
            kind: "tracker".into(),
            state: ProviderCircuitState::Available,
            reason_code: None,
            message: None,
            last_request_at: Some(Utc::now()),
            last_success_at: None,
            last_failure_at: None,
            retry_at: None,
            last_background_request_at: Some(Utc::now()),
            consecutive_failures: 0,
            minimum_interval_ms: 0,
            background_minimum_interval_ms: 60_000,
            max_concurrency: 1,
        };
        db.put_provider_state(&provider)
            .await
            .expect("provider state");
        for key in ["provider:first", "provider:second"] {
            db.enqueue_background_job(EnqueueBackgroundJob {
                deduplication_key: key,
                kind: "provider_job",
                payload: serde_json::json!({}),
                provider_id: Some("tracker:ops".into()),
                lane: "sync",
                priority: 100,
                max_attempts: 3,
                next_run_at: None,
                parent_id: None,
                recurring_interval_seconds: None,
            })
            .await
            .expect("provider job");
        }
        let local_id = db
            .enqueue_background_job(EnqueueBackgroundJob {
                deduplication_key: "local:first",
                kind: "local_job",
                payload: serde_json::json!({}),
                provider_id: None,
                lane: "sync",
                priority: 1,
                max_attempts: 3,
                next_run_at: None,
                parent_id: None,
                recurring_interval_seconds: None,
            })
            .await
            .expect("local job");

        let local = db
            .claim_background_job("local-worker", "sync", std::time::Duration::from_secs(60))
            .await
            .expect("claim")
            .expect("local job is eligible");
        assert_eq!(local.id, local_id);
        db.complete_background_job(local.id, "local-worker")
            .await
            .expect("complete local");
        assert!(
            db.claim_background_job("early-worker", "sync", std::time::Duration::from_secs(60))
                .await
                .expect("early claim")
                .is_none()
        );

        provider.last_background_request_at = Some(Utc::now() - Duration::minutes(2));
        db.put_provider_state(&provider)
            .await
            .expect("provider due");
        let claimed = db
            .claim_background_job(
                "provider-worker",
                "sync",
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("provider claim")
            .expect("provider job due");
        assert_eq!(claimed.provider_id.as_deref(), Some("tracker:ops"));
        assert!(
            db.claim_background_job("other-worker", "sync", std::time::Duration::from_secs(60))
                .await
                .expect("concurrent claim")
                .is_none()
        );
    }

    #[tokio::test]
    async fn background_jobs_are_deduplicated_leased_recovered_and_controllable() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("jobs.sqlite"))
            .await
            .expect("database");
        let request = || EnqueueBackgroundJob {
            deduplication_key: "test:durable-job",
            kind: "test_job",
            payload: serde_json::json!({ "value": 1 }),
            provider_id: None,
            lane: "sync",
            priority: 10,
            max_attempts: 3,
            next_run_at: None,
            parent_id: None,
            recurring_interval_seconds: None,
        };

        let id = db.enqueue_background_job(request()).await.expect("enqueue");
        assert_eq!(
            db.enqueue_background_job(request())
                .await
                .expect("deduplicated enqueue"),
            id
        );
        assert_eq!(
            db.background_jobs_overview(10)
                .await
                .expect("overview")
                .counts
                .pending,
            1
        );

        let claimed = db
            .claim_background_job("worker-one", "sync", std::time::Duration::ZERO)
            .await
            .expect("claim")
            .expect("claimed job");
        assert_eq!(claimed.id, id);
        assert_eq!(
            db.recover_expired_background_jobs().await.expect("recover"),
            1
        );
        let claimed = db
            .claim_background_job("worker-two", "sync", std::time::Duration::from_secs(60))
            .await
            .expect("second claim")
            .expect("recovered job");
        assert_eq!(claimed.id, id);
        assert_eq!(
            db.release_background_job_lease("worker-two")
                .await
                .expect("release on shutdown"),
            1
        );
        let claimed = db
            .claim_background_job("worker-three", "sync", std::time::Duration::from_secs(60))
            .await
            .expect("third claim")
            .expect("released job");
        assert_eq!(claimed.id, id);
        assert!(db.cancel_background_job(id).await.expect("cancel"));
        db.complete_background_job(id, "worker-three")
            .await
            .expect("stale completion ignored");
        assert_eq!(
            db.background_jobs_overview(10)
                .await
                .expect("overview")
                .jobs[0]
                .state,
            crate::model::BackgroundJobState::Cancelled
        );

        assert!(
            db.retry_failed_background_job(id)
                .await
                .expect("manual retry")
        );
        let claimed = db
            .claim_background_job("worker-four", "sync", std::time::Duration::from_secs(60))
            .await
            .expect("fourth claim")
            .expect("retried job");
        db.complete_background_job(claimed.id, "worker-four")
            .await
            .expect("complete");
        assert_eq!(
            db.background_jobs_overview(10)
                .await
                .expect("overview")
                .jobs[0]
                .state,
            crate::model::BackgroundJobState::Completed
        );
    }

    #[tokio::test]
    async fn concurrent_job_enqueues_are_serialized_and_deduplicated() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("concurrent-jobs.sqlite"))
            .await
            .expect("database");
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let db = db.clone();
            tasks.spawn(async move {
                db.enqueue_background_job(EnqueueBackgroundJob {
                    deduplication_key: "test:concurrent-job",
                    kind: "test_job",
                    payload: serde_json::json!({ "value": 1 }),
                    provider_id: None,
                    lane: "sync",
                    priority: 10,
                    max_attempts: 3,
                    next_run_at: None,
                    parent_id: None,
                    recurring_interval_seconds: None,
                })
                .await
            });
        }
        let mut ids = std::collections::HashSet::new();
        while let Some(result) = tasks.join_next().await {
            ids.insert(result.expect("task").expect("enqueue"));
        }
        assert_eq!(ids.len(), 1);
        assert_eq!(
            db.background_jobs_overview(10)
                .await
                .expect("overview")
                .counts
                .pending,
            1
        );
    }

    #[tokio::test]
    async fn domain_changes_enqueue_background_work_transactionally() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("transactional-jobs.sqlite"))
            .await
            .expect("database");

        db.enqueue_track_index_with_priority("ops", 42, 20)
            .await
            .expect("track index");
        db.ensure_single_coverage("ops", 42)
            .await
            .expect("single coverage");
        let overview = db.background_jobs_overview(10).await.expect("overview");
        assert_eq!(overview.counts.pending, 2);
        assert!(
            overview
                .jobs
                .iter()
                .any(|job| job.kind == "index_tracklist")
        );
        assert!(
            overview
                .jobs
                .iter()
                .any(|job| job.kind == "compute_single_coverage")
        );
    }

    #[tokio::test]
    async fn library_deduplication_seeding_is_batched_durable_and_idempotent() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("library-deduplication.sqlite"))
            .await
            .expect("database");
        let artists = vec![("ops".to_owned(), 10), ("ops".to_owned(), 20)];
        let singles = vec![("ops".to_owned(), 100), ("ops".to_owned(), 200)];

        db.ensure_artist_catalog_refreshes(&artists)
            .await
            .expect("artist catalogs");
        db.seed_single_deduplications(&singles)
            .await
            .expect("single coverage dependencies");
        db.ensure_artist_catalog_refreshes(&artists)
            .await
            .expect("duplicate artist catalogs");
        db.seed_single_deduplications(&singles)
            .await
            .expect("duplicate single coverage dependencies");

        let overview = db.background_jobs_overview(20).await.expect("overview");
        assert_eq!(overview.counts.pending, 6);
        for key in [
            "refresh-artist-catalog:ops:10:v1",
            "refresh-artist-catalog:ops:20:v1",
            "track-index:ops:100:v2",
            "track-index:ops:200:v2",
            "single-coverage:ops:100:v2",
            "single-coverage:ops:200:v2",
        ] {
            assert!(
                overview.jobs.iter().any(|job| job.deduplication_key == key),
                "missing durable job {key}"
            );
        }
    }

    #[tokio::test]
    async fn first_download_completion_enqueues_one_debounced_plex_scan() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("plex-jobs.sqlite"))
            .await
            .expect("database");
        let target = PlexScanTarget {
            section_id: 4,
            root: "/downloads/ops".into(),
        };
        let mut live = LiveDownloadStatus {
            client: "music".into(),
            info_hash: "abcdef0123456789abcdef0123456789abcdef01".into(),
            state: ClientDownloadState::Downloading,
            client_state: "downloading".into(),
            diagnostic: None,
            progress: 0.5,
            size: 100,
            downloaded: 50,
            uploaded: 0,
            download_speed: 1,
            upload_speed: 0,
            eta: Some(60),
            ratio: 0.0,
            save_path: "/downloads/ops/Artist/Album".into(),
            content_path: Some("/downloads/ops/Artist/Album".into()),
            added_at: None,
            completed_at: None,
        };

        db.observe_download(&live, None, None, Some(&target))
            .await
            .expect("observe incomplete");
        assert_eq!(
            db.active_background_jobs_by_kind("notify_plex")
                .await
                .expect("active jobs"),
            0
        );

        live.state = ClientDownloadState::Seeding;
        live.client_state = "stalledUP".into();
        live.progress = 1.0;
        live.downloaded = 100;
        live.completed_at = Some(Utc::now());
        db.observe_download(&live, None, None, Some(&target))
            .await
            .expect("observe completion");
        db.observe_download(&live, None, None, Some(&target))
            .await
            .expect("observe repeated completion");

        let overview = db.background_jobs_overview(10).await.expect("overview");
        let jobs = overview
            .jobs
            .iter()
            .filter(|job| job.kind == "notify_plex")
            .collect::<Vec<_>>();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].next_run_at.is_some());
    }

    #[tokio::test]
    async fn migrates_and_keeps_multi_client_hash_associations() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        let hash = "abcdef0123456789abcdef0123456789abcdef01";

        db.seed_download_link("one", hash, "ops", Some(10), 20, false)
            .await
            .expect("first link");
        db.seed_download_link("two", hash, "ops", Some(10), 20, false)
            .await
            .expect("second link");

        assert!(db.get_link("one", hash).await.expect("lookup").is_some());
        assert!(db.get_link("two", hash).await.expect("lookup").is_some());
        assert_eq!(db.index_counts().await.expect("counts").pending, 2);
    }

    #[tokio::test]
    async fn persists_runtime_preferences() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        let mut preferences = crate::model::RuntimePreferences::default();
        preferences.release.quality_cutoff_index = 3;
        db.put_runtime_preferences(&preferences)
            .await
            .expect("save preferences");
        assert_eq!(
            db.get_runtime_preferences()
                .await
                .expect("load preferences"),
            preferences
        );
    }

    #[tokio::test]
    async fn persists_channel_configuration_and_immutable_pack_items() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        db.ensure_default_channels().await.expect("seed channels");
        let channels = db.list_channels().await.expect("channels");
        assert_eq!(channels.len(), 3);
        assert!(
            !channels
                .iter()
                .find(|channel| channel.id == "country_chart")
                .expect("chart")
                .enabled
        );
        assert!(
            channels
                .iter()
                .find(|channel| channel.id == "trumped_downloads")
                .expect("trumped downloads")
                .enabled
        );
        let item = ChannelPackItem {
            ordinal: 1,
            source: RecommendationSource {
                id: "apple:42".into(),
                rank: 1,
                artist: "Artist".into(),
                title: "Album".into(),
                year: Some(2026),
                artwork: None,
                url: None,
                mbid: None,
                score: None,
                catalog_country: None,
                substituted_from: None,
                trumped_downloads: Vec::new(),
                lookup_files: Vec::new(),
            },
            match_state: RecommendationMatchState::Unmatched,
            release: None,
            variants: Vec::new(),
            candidates: Vec::new(),
            downloads: Vec::new(),
            plan_state: PackItemPlanState::Unmatched,
            plan: None,
            replacement: None,
            reason: Some("Unavailable".into()),
            job_id: None,
            job: None,
        };
        let id = db
            .create_channel_pack("country_chart", "AU Top 100", false, "fingerprint", &[item])
            .await
            .expect("create pack");
        let pack = db
            .get_channel_pack(id, "fingerprint")
            .await
            .expect("load pack")
            .expect("pack");
        assert_eq!(pack.items.len(), 1);
        assert_eq!(pack.summary.skipped, 1);
        assert!(!pack.plan_stale);
        assert!(
            db.get_channel_pack(id, "changed")
                .await
                .expect("load stale pack")
                .expect("pack")
                .plan_stale
        );
    }

    #[tokio::test]
    async fn recovers_interrupted_channel_runs_with_visible_retry_state() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        db.ensure_default_channels().await.expect("seed channels");
        let run = db
            .create_channel_run("country_chart", ChannelRunTrigger::Scheduled)
            .await
            .expect("create run")
            .expect("new run");

        db.recover_channel_runs().await.expect("recover runs");

        let recovered = db
            .get_channel_run(run.id)
            .await
            .expect("load run")
            .expect("run");
        assert_eq!(recovered.status, ChannelRunStatus::Failed);
        assert!(
            recovered
                .error
                .as_deref()
                .is_some_and(|error| error.contains("restarted"))
        );
        let channel = db
            .get_channel("country_chart")
            .await
            .expect("load channel")
            .expect("channel");
        assert_eq!(channel.failure_count, 1);
        assert!(
            channel
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("restarted"))
        );
    }

    #[tokio::test]
    async fn channel_runs_expose_provider_wait_progress_and_retry_time() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("channel-wait.sqlite"))
            .await
            .expect("database");
        db.ensure_default_channels().await.expect("seed channels");
        let run = db
            .create_channel_run("trumped_downloads", ChannelRunTrigger::Manual)
            .await
            .expect("create run")
            .expect("new run");
        let retry_at = Utc::now() + Duration::minutes(15);

        db.wait_channel_run_for_provider(
            run.id,
            12,
            53,
            "tracker:ops is temporarily limited",
            retry_at,
        )
        .await
        .expect("wait for provider");

        let waiting = db
            .get_channel_run(run.id)
            .await
            .expect("load run")
            .expect("run exists");
        assert_eq!(
            waiting.phase,
            Some(crate::model::ChannelRunPhase::WaitingProvider)
        );
        assert_eq!(waiting.progress_completed, 12);
        assert_eq!(waiting.progress_total, Some(53));
        assert_eq!(waiting.retry_at, Some(retry_at));
    }

    #[tokio::test]
    async fn failed_download_jobs_can_be_retried_with_a_new_idempotency_key() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        let (job, created) = db
            .create_job("ops", 3026589, "ops", false, Some("first-request"))
            .await
            .expect("create job");
        assert!(created);
        let submission_key = format!("submit-download:{}:v1", job.id);
        let submission = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(&submission_key))
            .one(&db.connection)
            .await
            .expect("submission lookup")
            .expect("durable submission");
        assert_eq!(submission.state, "pending");
        assert_eq!(submission.lane, "download");
        db.set_job_state(
            job.id,
            crate::model::DownloadState::Failed,
            Some(("download_failed", "tracker omitted its info hash")),
        )
        .await
        .expect("fail job");

        let (replayed, created) = db
            .create_job("ops", 3026589, "ops", false, Some("first-request"))
            .await
            .expect("replay job");
        assert!(!created);
        assert_eq!(replayed.state, crate::model::DownloadState::Failed);

        let (retried, created) = db
            .create_job("ops", 3026589, "ops", true, Some("retry-request"))
            .await
            .expect("retry job");
        assert!(created);
        assert_eq!(retried.id, job.id);
        assert_eq!(retried.state, crate::model::DownloadState::Queued);
        assert!(retried.use_token);
        assert!(retried.error_message.is_none());
        let submission = background_job::Entity::find()
            .filter(background_job::Column::DeduplicationKey.eq(&submission_key))
            .one(&db.connection)
            .await
            .expect("submission lookup")
            .expect("durable submission");
        assert_eq!(submission.state, "pending");
        assert_eq!(
            db.get_job(job.id)
                .await
                .expect("job lookup")
                .expect("job")
                .state,
            crate::model::DownloadState::Queued
        );
    }

    #[tokio::test]
    async fn pages_only_linked_downloads_in_client_added_order() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        let canonical = CanonicalTorrent {
            release: ReleaseSummary {
                id: None,
                tracker: "ops".into(),
                group_id: 10,
                title: "Indexed release".into(),
                artist: Some("Artist".into()),
                artists: vec![fallback_artist_credit("ops", "Artist")],
                year: Some(2026),
                artwork: None,
                release_type: Some("Album".into()),
                sources: vec![crate::model::ReleaseSource {
                    tracker: "ops".into(),
                    group_id: 10,
                    match_score: 1.0,
                }],
                album_coverage: None,
            },
            variant: TorrentVariant {
                tracker: "ops".into(),
                torrent_id: 20,
                group_id: 10,
                info_hash: None,
                format: Some("FLAC".into()),
                encoding: Some("Lossless".into()),
                media: Some("WEB".into()),
                size: Some(100),
                seeders: None,
                leechers: None,
                snatched: None,
                freeleech: false,
                leech_status: crate::model::LeechStatus::Regular,
                can_use_token: false,
                token_eligibility_known: false,
                eligibility: None,
                remaster_title: None,
                downloads: Vec::new(),
                library: None,
            },
            tags: Vec::new(),
            description: None,
            record_label: None,
        };
        db.put_canonical(&canonical, Utc::now(), Utc::now() + Duration::hours(1))
            .await
            .expect("canonical");
        let base = Utc::now() - Duration::minutes(10);
        for (index, hash) in ["aaa", "bbb", "ccc"].into_iter().enumerate() {
            db.seed_download_link("music", hash, "ops", Some(10), 20, true)
                .await
                .expect("seed link");
            db.observe_download(
                &LiveDownloadStatus {
                    client: "music".into(),
                    info_hash: hash.into(),
                    state: ClientDownloadState::Downloading,
                    client_state: "downloading".into(),
                    diagnostic: None,
                    progress: 0.5,
                    size: 100,
                    downloaded: 50,
                    uploaded: 0,
                    download_speed: 1,
                    upload_speed: 0,
                    eta: Some(50),
                    ratio: 0.0,
                    save_path: "/downloads/ops".into(),
                    content_path: Some(format!("/downloads/ops/{hash}")),
                    added_at: Some(base + Duration::minutes(index as i64)),
                    completed_at: None,
                },
                Some("home.opsfet.ch"),
                Some("ops"),
                None,
            )
            .await
            .expect("observe link");
        }
        db.observe_download(
            &LiveDownloadStatus {
                client: "music".into(),
                info_hash: "unconfigured".into(),
                state: ClientDownloadState::Downloading,
                client_state: "downloading".into(),
                diagnostic: None,
                progress: 0.1,
                size: 100,
                downloaded: 10,
                uploaded: 0,
                download_speed: 1,
                upload_speed: 0,
                eta: Some(90),
                ratio: 0.0,
                save_path: "/downloads/other".into(),
                content_path: Some("/downloads/other/unconfigured".into()),
                added_at: Some(Utc::now()),
                completed_at: None,
            },
            None,
            None,
            None,
        )
        .await
        .expect("observe unconfigured");

        let (page, total) = db
            .list_indexed_downloads(Some("music"), 1, 1)
            .await
            .expect("page");
        assert_eq!(total, 3);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].info_hash, "bbb");
        assert!(page[0].live.is_some());
    }

    #[tokio::test]
    async fn release_level_matches_leave_torrent_identity_unknown_but_complete_import_review() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("release-match.sqlite"))
            .await
            .expect("database");
        let canonical = CanonicalTorrent {
            release: ReleaseSummary {
                id: None,
                tracker: "ops".into(),
                group_id: 10,
                title: "Afterglow".into(),
                artist: Some("Sleep Theory".into()),
                artists: vec![fallback_artist_credit("ops", "Sleep Theory")],
                year: Some(2025),
                artwork: None,
                release_type: Some("Album".into()),
                sources: vec![crate::model::ReleaseSource {
                    tracker: "ops".into(),
                    group_id: 10,
                    match_score: 1.0,
                }],
                album_coverage: None,
            },
            variant: TorrentVariant {
                tracker: "ops".into(),
                torrent_id: 20,
                group_id: 10,
                info_hash: None,
                format: Some("FLAC".into()),
                encoding: Some("Lossless".into()),
                media: Some("WEB".into()),
                size: Some(100),
                seeders: None,
                leechers: None,
                snatched: None,
                freeleech: false,
                leech_status: crate::model::LeechStatus::Regular,
                can_use_token: false,
                token_eligibility_known: false,
                eligibility: None,
                remaster_title: None,
                downloads: Vec::new(),
                library: None,
            },
            tags: Vec::new(),
            description: None,
            record_label: None,
        };
        db.put_canonical(&canonical, Utc::now(), Utc::now() + Duration::hours(1))
            .await
            .expect("canonical");
        let release_id = db.list_release_summaries().await.expect("releases")[0]
            .id
            .expect("release id");
        let live = LiveDownloadStatus {
            client: "music".into(),
            info_hash: "redhash".into(),
            state: ClientDownloadState::Seeding,
            client_state: "stalledUP".into(),
            diagnostic: None,
            progress: 1.0,
            size: 100,
            downloaded: 100,
            uploaded: 50,
            download_speed: 0,
            upload_speed: 0,
            eta: None,
            ratio: 0.5,
            save_path: "/downloads/red".into(),
            content_path: Some("/downloads/red/Afterglow".into()),
            added_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
        };
        db.observe_downloads(&[DownloadObservation {
            torrent_name: Some("Sleep Theory — Afterglow (2025) [WEB-FLAC 24-48]".into()),
            live: live.clone(),
            announce_host: Some("flacsfor.me".into()),
            tracker: None,
            plex_target: None,
        }])
        .await
        .expect("observation");
        db.set_automatic_release_match("music", "redhash", release_id, Some("red"))
            .await
            .expect("automatic match");
        db.sync_import_tasks().await.expect("sync imports");

        let (downloads, total) = db
            .list_indexed_downloads(Some("music"), 10, 0)
            .await
            .expect("downloads");
        assert_eq!(total, 1);
        assert_eq!(downloads[0].release.value.title, "Afterglow");
        assert!(downloads[0].variant.is_none());
        let library = db.list_library_records().await.expect("library");
        assert_eq!(library.len(), 1);
        assert!(library[0].variant.is_none());
        assert_eq!(library[0].release.value.id, Some(release_id));
        let imports = db.list_imports(10, 0).await.expect("imports");
        assert_eq!(imports.counts.review, 0);
        assert_eq!(imports.counts.complete, 1);

        db.observe_downloads(&[DownloadObservation {
            torrent_name: Some("Sleep Theory — Afterglow (2025) [WEB-FLAC 24-48]".into()),
            live,
            announce_host: Some("flacsfor.me".into()),
            tracker: Some("red".into()),
            plex_target: None,
        }])
        .await
        .expect("configured tracker observation");
        assert!(
            db.background_job_by_key("resolve-hash:red:redhash:v3")
                .await
                .expect("resolution job")
                .is_some(),
            "release-level matches must be replaced by exact tracker hash resolution"
        );
        assert_eq!(
            db.links_for_tracker_hash("red", "redhash")
                .await
                .expect("resolvable links")
                .len(),
            1,
            "the resolver must include linked records without exact torrent identity"
        );
    }

    #[tokio::test]
    async fn persists_negative_results_and_recovers_in_progress_links() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        let hash = "abcdef0123456789abcdef0123456789abcdef01";
        db.seed_download_link("music", hash, "ops", None, 20, false)
            .await
            .expect("link");
        let model =
            download_release_link::Entity::find_by_id(("music".to_owned(), hash.to_owned()))
                .one(&db.connection)
                .await
                .expect("lookup link")
                .expect("link");
        let mut active = model.into_active_model();
        active.first_seen_at = Set((Utc::now() - Duration::hours(25)).to_rfc3339());
        active.update(&db.connection).await.expect("age link");

        db.set_link_failure("music", hash, true, "not_found", "not found")
            .await
            .expect("negative result");
        let link = db
            .get_link("music", hash)
            .await
            .expect("lookup")
            .expect("link");
        assert_eq!(link.resolution_state, "not_found");
        assert!(db.due_links(10).await.expect("due links").is_empty());
        assert!(db.retry_link("music", hash).await.expect("retry"));

        db.set_link_resolving("music", hash)
            .await
            .expect("resolving");
        db.recover_resolving_links().await.expect("recover");
        assert_eq!(
            db.get_link("music", hash)
                .await
                .expect("lookup")
                .expect("link")
                .resolution_state,
            "pending"
        );
    }

    #[tokio::test]
    async fn lists_only_completed_downloads_with_explicit_unregistered_evidence() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        let hash = "abcdef0123456789abcdef0123456789abcdef01";
        db.seed_download_link("music", hash, "ops", None, 20, false)
            .await
            .expect("link");
        let model =
            download_release_link::Entity::find_by_id(("music".to_owned(), hash.to_owned()))
                .one(&db.connection)
                .await
                .expect("lookup link")
                .expect("link");
        let mut active = model.into_active_model();
        active.torrent_name = Set(Some("Artist - Album (2026) [FLAC]".into()));
        active.library_added_at = Set(Some(Utc::now().to_rfc3339()));
        active.update(&db.connection).await.expect("complete link");

        db.set_link_failure("music", hash, true, "torrent_unregistered", "unregistered")
            .await
            .expect("unregistered result");
        let downloads = db
            .unregistered_downloads()
            .await
            .expect("unregistered downloads");
        assert_eq!(downloads.len(), 1);
        assert_eq!(
            downloads[0].torrent_name.as_deref(),
            Some("Artist - Album (2026) [FLAC]")
        );
        assert_eq!(
            db.downloaded_torrent_ids("ops", &[20, 21])
                .await
                .expect("downloaded torrent ids"),
            vec![20]
        );
        assert!(
            db.downloaded_torrent_ids("red", &[20])
                .await
                .expect("other tracker ids")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn keeps_completed_library_links_when_the_client_no_longer_reports_them() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        let hash = "abcdef0123456789abcdef0123456789abcdef01";
        let canonical = CanonicalTorrent {
            release: ReleaseSummary {
                id: None,
                tracker: "ops".into(),
                group_id: 10,
                title: "A Complete Release".into(),
                artist: Some("The Artist".into()),
                artists: vec![fallback_artist_credit("ops", "The Artist")],
                year: Some(2020),
                artwork: None,
                release_type: Some("Album".into()),
                sources: vec![crate::model::ReleaseSource {
                    tracker: "ops".into(),
                    group_id: 10,
                    match_score: 1.0,
                }],
                album_coverage: None,
            },
            variant: TorrentVariant {
                tracker: "ops".into(),
                torrent_id: 20,
                group_id: 10,
                info_hash: Some(hash.into()),
                format: Some("FLAC".into()),
                encoding: Some("Lossless".into()),
                media: Some("WEB".into()),
                size: Some(100),
                seeders: None,
                leechers: None,
                snatched: None,
                freeleech: false,
                leech_status: crate::model::LeechStatus::Regular,
                can_use_token: false,
                token_eligibility_known: false,
                eligibility: None,
                remaster_title: None,
                downloads: Vec::new(),
                library: None,
            },
            tags: Vec::new(),
            description: None,
            record_label: None,
        };
        db.put_canonical(&canonical, Utc::now(), Utc::now() + Duration::hours(24))
            .await
            .expect("canonical");
        let mut partial_catalog_variant = canonical.clone();
        partial_catalog_variant.variant.info_hash = None;
        db.put_canonical(
            &partial_catalog_variant,
            Utc::now(),
            Utc::now() + Duration::hours(24),
        )
        .await
        .expect("partial catalog canonical");
        assert_eq!(
            db.get_canonical("ops", 20)
                .await
                .expect("canonical lookup")
                .expect("canonical row")
                .value
                .variant
                .info_hash
                .as_deref(),
            Some(hash)
        );
        db.seed_download_link("music", hash, "ops", Some(10), 20, true)
            .await
            .expect("link");
        let completed_at = Utc::now() - Duration::days(2);
        db.observe_download(
            &LiveDownloadStatus {
                client: "music".into(),
                info_hash: hash.into(),
                state: ClientDownloadState::Seeding,
                client_state: "stalledUP".into(),
                diagnostic: None,
                progress: 1.0,
                size: 100,
                downloaded: 100,
                uploaded: 25,
                download_speed: 0,
                upload_speed: 0,
                eta: None,
                ratio: 0.25,
                save_path: "/downloads/ops".into(),
                content_path: Some("/downloads/ops/A Complete Release".into()),
                added_at: None,
                completed_at: Some(completed_at),
            },
            Some("home.opsfet.ch"),
            Some("ops"),
            None,
        )
        .await
        .expect("observe complete");

        let scan_started_at = Utc::now() + Duration::seconds(1);
        db.complete_client_scan("music", scan_started_at)
            .await
            .expect("complete scan");
        let records = db.list_library_records().await.expect("library records");
        assert_eq!(records.len(), 1);
        assert!(!records[0].present);
        assert!(records[0].missing_since.is_some());
        assert_eq!(records[0].completed_at, completed_at);
        let release_id = records[0].release.value.id.expect("canonical release id");
        assert_eq!(
            db.list_library_records_for_releases(&[release_id])
                .await
                .expect("scoped library records")
                .len(),
            1
        );
        assert!(
            db.list_library_records_for_releases(&[uuid::Uuid::new_v4()])
                .await
                .expect("unrelated library records")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn canonical_ids_merge_confirmed_sources_and_survive_restart() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("wotbox.sqlite");
        let db = Database::open(&path).await.expect("database");
        let now = Utc::now();
        let mut source = CanonicalTorrent {
            release: ReleaseSummary {
                id: None,
                tracker: "ops".into(),
                group_id: 100,
                title: "A Shared Album".into(),
                artist: Some("The Artist".into()),
                artists: vec![fallback_artist_credit("ops", "The Artist")],
                year: Some(2024),
                artwork: Some("https://example.test/cover.jpg".into()),
                release_type: Some("Album".into()),
                sources: vec![crate::model::ReleaseSource {
                    tracker: "ops".into(),
                    group_id: 100,
                    match_score: 1.0,
                }],
                album_coverage: None,
            },
            variant: TorrentVariant {
                tracker: "ops".into(),
                torrent_id: 1000,
                group_id: 100,
                info_hash: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
                format: Some("FLAC".into()),
                encoding: Some("Lossless".into()),
                media: Some("WEB".into()),
                size: Some(100),
                seeders: Some(10),
                leechers: Some(0),
                snatched: Some(20),
                freeleech: true,
                leech_status: crate::model::LeechStatus::Freeleech,
                can_use_token: true,
                token_eligibility_known: true,
                eligibility: None,
                remaster_title: None,
                downloads: Vec::new(),
                library: None,
            },
            tags: vec!["rock".into()],
            description: Some("OPS description".into()),
            record_label: None,
        };
        db.put_canonical(&source, now, now + Duration::hours(24))
            .await
            .expect("OPS source");
        let ops_id = db
            .release_id_for_source("ops", 100)
            .await
            .expect("OPS identity")
            .expect("OPS UUID");

        source.release.tracker = "red".into();
        source.release.group_id = 200;
        source.release.sources[0].tracker = "red".into();
        source.release.sources[0].group_id = 200;
        source.release.artists[0] = fallback_artist_credit("red", "The Artist");
        source.variant.tracker = "red".into();
        source.variant.group_id = 200;
        source.variant.torrent_id = 2000;
        source.variant.info_hash = Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        db.put_canonical(&source, now, now + Duration::hours(24))
            .await
            .expect("RED source");
        let red_id = db
            .release_id_for_source("red", 200)
            .await
            .expect("RED identity")
            .expect("RED UUID");
        assert_eq!(ops_id, red_id);
        let detail = db
            .get_release_detail(ops_id)
            .await
            .expect("canonical detail")
            .expect("release");
        assert_eq!(detail.release.sources.len(), 2);
        assert_eq!(detail.variants.len(), 2);
        assert!(
            detail
                .release
                .artists
                .iter()
                .all(|artist| artist.canonical_id.is_some())
        );
        let source_ids = db
            .release_ids_for_sources(&[
                ("ops".into(), 100),
                ("red".into(), 200),
                ("ops".into(), 999),
            ])
            .await
            .expect("bulk source identities");
        assert_eq!(source_ids.get(&("ops".into(), 100)), Some(&ops_id));
        assert_eq!(source_ids.get(&("red".into(), 200)), Some(&ops_id));
        assert!(!source_ids.contains_key(&("ops".into(), 999)));
        let details = db
            .get_release_details(&[ops_id])
            .await
            .expect("bulk canonical details");
        assert_eq!(details[&ops_id].variants.len(), 2);

        drop(db);
        let reopened = Database::open(&path).await.expect("reopened database");
        assert_eq!(
            reopened
                .release_id_for_source("red", 200)
                .await
                .expect("stable identity"),
            Some(ops_id)
        );
    }
}
