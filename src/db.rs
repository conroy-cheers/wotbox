use std::{path::Path, str::FromStr};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, ConnectOptions,
    Database as SeaDatabase, DatabaseConnection, EntityTrait, IntoActiveModel, ModelTrait,
    QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    dedupe::{CatalogMembership, RawSingleCoverage, ReleaseTrackIndex},
    entity::{
        canonical_release_artist, canonical_torrent, dedupe_catalog_membership,
        download_client_scan, download_event, download_job, download_release_link,
        release_track_index, runtime_preference, single_album_coverage, tracker_snapshot,
    },
    migration::Migrator,
    model::{
        ArtistCatalogPage, ArtistCreditSource, CanonicalTorrent, DownloadIndexCounts, DownloadJob,
        DownloadState, LiveDownloadStatus, RuntimePreferences,
    },
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
    pub tracker: Option<String>,
    pub torrent_id: Option<i64>,
    pub resolution_state: String,
}

pub struct LibraryRecord {
    pub canonical: Cached<CanonicalTorrent>,
    pub client: String,
    pub info_hash: String,
    pub present: bool,
    pub library_added_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub missing_since: Option<DateTime<Utc>>,
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
            .map(|model| serde_json::from_value(model.value_json).map_err(Into::into))
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
        let now = Utc::now();
        if let Some(model) = release_track_index::Entity::find_by_id((tracker.to_owned(), group_id))
            .one(&self.connection)
            .await?
        {
            let current_priority = model.priority;
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
            }
            active.update(&self.connection).await?;
        } else {
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
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    pub async fn ensure_single_coverage(&self, tracker: &str, group_id: i64) -> Result<()> {
        if single_album_coverage::Entity::find_by_id((tracker.to_owned(), group_id))
            .one(&self.connection)
            .await?
            .is_none()
        {
            single_album_coverage::ActiveModel {
                tracker: Set(tracker.into()),
                single_group_id: Set(group_id),
                state: Set("pending".into()),
                coverage_json: Set(None),
                updated_at: Set(Utc::now().to_rfc3339()),
            }
            .insert(&self.connection)
            .await?;
        }
        Ok(())
    }

    pub async fn due_track_indexes(&self, limit: i64) -> Result<Vec<TrackIndexJob>> {
        let now = Utc::now().to_rfc3339();
        let models = release_track_index::Entity::find()
            .filter(release_track_index::Column::State.is_in(["pending", "failed"]))
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
        if let Some(model) =
            release_track_index::Entity::find_by_id((index.tracker.clone(), index.group_id))
                .one(&self.connection)
                .await?
        {
            let now = Utc::now();
            let mut active = model.into_active_model();
            active.state = Set("indexed".into());
            active.index_json = Set(Some(serde_json::to_value(index)?));
            active.attempts = Set(0);
            active.next_retry_at = Set(None);
            active.error_message = Set(None);
            active.fetched_at = Set(Some(now.to_rfc3339()));
            active.expires_at = Set(Some((now + chrono::Duration::hours(24)).to_rfc3339()));
            active.updated_at = Set(now.to_rfc3339());
            active.update(&self.connection).await?;
        }
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
        let transaction = self.connection.begin().await?;
        dedupe_catalog_membership::Entity::delete_many()
            .filter(dedupe_catalog_membership::Column::Tracker.eq(tracker))
            .filter(dedupe_catalog_membership::Column::ArtistId.eq(catalog.artist.artist_id))
            .exec(&transaction)
            .await?;
        let now = Utc::now().to_rfc3339();
        for group in &catalog.groups {
            dedupe_catalog_membership::ActiveModel {
                tracker: Set(tracker.into()),
                artist_id: Set(catalog.artist.artist_id),
                group_id: Set(group.release.group_id),
                group_json: Set(serde_json::to_value(group)?),
                updated_at: Set(now.clone()),
            }
            .insert(&transaction)
            .await?;
        }
        transaction.commit().await?;
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
        let mut progress = TrackIndexProgress {
            indexed: 0,
            pending: 0,
            resolving: 0,
            failed: 0,
        };
        for model in release_track_index::Entity::find()
            .all(&self.connection)
            .await?
        {
            match model.state.as_str() {
                "indexed" => progress.indexed += 1,
                "pending" => progress.pending += 1,
                "resolving" => progress.resolving += 1,
                "failed" => progress.failed += 1,
                _ => {}
            }
        }
        Ok(progress)
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
        if let Some(idempotency_key) = idempotency_key
            && let Some(existing) = self.find_job_by_idempotency_key(idempotency_key).await?
        {
            return Ok((existing, false));
        }
        if let Some(existing) = self.find_job(tracker, torrent_id, profile).await? {
            if existing.state == DownloadState::Failed {
                let now = Utc::now();
                let model = download_job::Entity::find_by_id(existing.id.to_string())
                    .one(&self.connection)
                    .await?
                    .context("download job disappeared while retrying")?;
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
                active.update(&self.connection).await?;
                self.add_event(existing.id, &DownloadState::Queued, None)
                    .await?;
                return Ok((
                    DownloadJob {
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
                    },
                    true,
                ));
            }
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
        .insert(&self.connection)
        .await?;
        self.add_event(job.id, &job.state, None).await?;
        Ok((job, true))
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

        let canonical_json = serde_json::to_value(&canonical)?;
        if let Some(model) = existing_torrent {
            let mut active = model.into_active_model();
            active.group_id = Set(canonical.release.group_id);
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

    async fn replace_release_artists(&self, canonical: &CanonicalTorrent) -> Result<()> {
        if canonical.release.artists.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.begin().await?;
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
    ) -> Result<()> {
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
        if let Some(model) =
            download_release_link::Entity::find_by_id((live.client.clone(), hash.clone()))
                .one(&self.connection)
                .await?
        {
            let linked = model.resolution_state == "linked";
            let tracker_changed = model.tracker.as_deref() != tracker;
            let has_library_added_at = model.library_added_at.is_some();
            let has_completed_at = model.completed_at.is_some();
            let mut active = model.into_active_model();
            active.announce_host = Set(announce_host.map(str::to_owned));
            if !linked {
                active.tracker = Set(tracker.map(str::to_owned));
                if tracker_changed {
                    active.resolution_state = Set(state.into());
                }
                active.updated_at = Set(now.clone());
            }
            active.last_seen_at = Set(now);
            active.present = Set(true);
            active.missing_since = Set(None);
            if !has_library_added_at && completed_at.is_some() {
                active.library_added_at = Set(completed_at.clone());
            }
            if !has_completed_at && completed_at.is_some() {
                active.completed_at = Set(completed_at);
            }
            active.update(&self.connection).await?;
        } else {
            download_release_link::ActiveModel {
                client: Set(live.client.clone()),
                info_hash: Set(hash),
                announce_host: Set(announce_host.map(str::to_owned)),
                tracker: Set(tracker.map(str::to_owned)),
                group_id: Set(None),
                torrent_id: Set(None),
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
            }
            .insert(&self.connection)
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
        if let Some(model) =
            download_release_link::Entity::find_by_id((client.to_owned(), hash.clone()))
                .one(&self.connection)
                .await?
        {
            let mut active = model.into_active_model();
            active.tracker = Set(Some(tracker.into()));
            active.group_id = Set(group_id);
            active.torrent_id = Set(Some(torrent_id));
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
                tracker: Set(Some(tracker.into())),
                group_id: Set(group_id),
                torrent_id: Set(Some(torrent_id)),
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
            .filter(download_release_link::Column::ResolutionState.is_in(["pending", "failed"]))
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
        let transaction = self.connection.begin().await?;
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
        if let Some(model) =
            download_release_link::Entity::find_by_id((client.to_owned(), info_hash.to_owned()))
                .one(&self.connection)
                .await?
        {
            let mut active = model.into_active_model();
            active.tracker = Set(Some(canonical.release.tracker.clone()));
            active.group_id = Set(Some(canonical.release.group_id));
            active.torrent_id = Set(Some(canonical.variant.torrent_id));
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
        message: &str,
    ) -> Result<()> {
        let model =
            download_release_link::Entity::find_by_id((client.to_owned(), info_hash.to_owned()))
                .one(&self.connection)
                .await?
                .context("download release link disappeared while recording failure")?;
        let first_seen = parse_timestamp(&model.first_seen_at)?;
        let old_enough = Utc::now() - first_seen >= chrono::Duration::hours(24);
        let delay = if not_found {
            chrono::Duration::hours(1)
        } else {
            chrono::Duration::seconds((30_i64 * (1_i64 << model.attempts.min(7))).min(3600))
        };
        let attempts = model.attempts;
        let mut active = model.into_active_model();
        active.resolution_state = Set(if not_found && old_enough {
            "not_found"
        } else {
            "failed"
        }
        .into());
        active.attempts = Set(attempts + 1);
        active.next_retry_at =
            Set((!not_found || !old_enough).then(|| (Utc::now() + delay).to_rfc3339()));
        active.error_code = Set(Some(
            if not_found {
                "not_found"
            } else {
                "tracker_error"
            }
            .into(),
        ));
        active.error_message = Set(Some(message.chars().take(500).collect()));
        active.updated_at = Set(Utc::now().to_rfc3339());
        active.update(&self.connection).await?;
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
        let mut counts = DownloadIndexCounts::default();
        for model in download_release_link::Entity::find()
            .filter(download_release_link::Column::Present.eq(true))
            .all(&self.connection)
            .await?
        {
            match model.resolution_state.as_str() {
                "linked" => counts.linked += 1,
                "pending" => counts.pending += 1,
                "resolving" => counts.resolving += 1,
                "unconfigured" => counts.unconfigured += 1,
                "failed" | "not_found" => counts.failed += 1,
                _ => {}
            }
        }
        Ok(counts)
    }

    pub async fn list_library_records(&self) -> Result<Vec<LibraryRecord>> {
        let links = download_release_link::Entity::find()
            .filter(download_release_link::Column::ResolutionState.eq("linked"))
            .filter(download_release_link::Column::LibraryAddedAt.is_not_null())
            .all(&self.connection)
            .await?;
        let mut records = Vec::with_capacity(links.len());
        for link in links {
            let (Some(tracker), Some(torrent_id), Some(library_added_at)) = (
                link.tracker.as_deref(),
                link.torrent_id,
                link.library_added_at.as_deref(),
            ) else {
                continue;
            };
            let Some(canonical) =
                canonical_torrent::Entity::find_by_id((tracker.to_owned(), torrent_id))
                    .one(&self.connection)
                    .await?
            else {
                continue;
            };
            let library_added_at = parse_timestamp(library_added_at)?;
            records.push(LibraryRecord {
                canonical: canonical_from_model(canonical)?,
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

    pub async fn last_successful_download_scan(&self) -> Result<Option<DateTime<Utc>>> {
        download_client_scan::Entity::find()
            .order_by_asc(download_client_scan::Column::LastSuccessfulAt)
            .one(&self.connection)
            .await?
            .map(|model| parse_timestamp(&model.last_successful_at))
            .transpose()
    }

    async fn find_job(
        &self,
        tracker: &str,
        torrent_id: i64,
        profile: &str,
    ) -> Result<Option<DownloadJob>> {
        download_job::Entity::find()
            .filter(download_job::Column::Tracker.eq(tracker))
            .filter(download_job::Column::TorrentId.eq(torrent_id))
            .filter(download_job::Column::Profile.eq(profile))
            .one(&self.connection)
            .await?
            .map(job_from_model)
            .transpose()
    }

    async fn find_job_by_idempotency_key(&self, key: &str) -> Result<Option<DownloadJob>> {
        download_job::Entity::find()
            .filter(download_job::Column::IdempotencyKey.eq(key))
            .one(&self.connection)
            .await?
            .map(job_from_model)
            .transpose()
    }

    async fn add_event(&self, id: Uuid, state: &DownloadState, detail: Option<&str>) -> Result<()> {
        download_event::ActiveModel {
            id: Default::default(),
            job_id: Set(id.to_string()),
            state: Set(state.as_str().into()),
            detail: Set(detail.map(str::to_owned)),
            created_at: Set(Utc::now().to_rfc3339()),
        }
        .insert(&self.connection)
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

fn canonical_from_model(model: canonical_torrent::Model) -> Result<Cached<CanonicalTorrent>> {
    Ok(Cached {
        value: serde_json::from_value(model.canonical_json)?,
        fetched_at: parse_timestamp(&model.fetched_at)?,
        expires_at: parse_timestamp(&model.expires_at)?,
    })
}

fn link_from_model(model: download_release_link::Model) -> DownloadReleaseLink {
    DownloadReleaseLink {
        client: model.client,
        info_hash: model.info_hash,
        tracker: model.tracker,
        torrent_id: model.torrent_id,
        resolution_state: model.resolution_state,
    }
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

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait, IntoActiveModel};
    use tempfile::tempdir;

    use crate::{
        entity::download_release_link,
        model::{
            CanonicalTorrent, ClientDownloadState, LiveDownloadStatus, ReleaseSummary,
            TorrentVariant,
        },
        tracker::fallback_artist_credit,
    };

    use super::Database;

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
        preferences.release.minimum_quality = "320".into();
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

        db.set_link_failure("music", hash, true, "not found")
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
    async fn keeps_completed_library_links_when_the_client_no_longer_reports_them() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        let hash = "abcdef0123456789abcdef0123456789abcdef01";
        let canonical = CanonicalTorrent {
            release: ReleaseSummary {
                tracker: "ops".into(),
                group_id: 10,
                title: "A Complete Release".into(),
                artist: Some("The Artist".into()),
                artists: vec![fallback_artist_credit("ops", "The Artist")],
                year: Some(2020),
                artwork: None,
                release_type: Some("Album".into()),
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
                can_use_token: false,
                token_eligibility_known: false,
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
                added_at: None,
                completed_at: Some(completed_at),
            },
            Some("home.opsfet.ch"),
            Some("ops"),
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
    }
}
