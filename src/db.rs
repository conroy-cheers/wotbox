use std::{path::Path, str::FromStr};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use uuid::Uuid;

use crate::dedupe::{CatalogMembership, RawSingleCoverage, ReleaseTrackIndex};
use crate::model::{
    ArtistCatalogPage, ArtistCreditSource, CanonicalTorrent, DownloadIndexCounts, DownloadJob,
    DownloadState, LiveDownloadStatus, RuntimePreferences,
};

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
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

impl Database {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .context("connect to SQLite")?;
        sqlx::migrate!()
            .run(&pool)
            .await
            .context("run migrations")?;
        Ok(Self { pool })
    }

    pub async fn get_snapshot<T: DeserializeOwned>(
        &self,
        tracker: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<Cached<T>>> {
        let row = sqlx::query(
            "SELECT normalized_json, sanitized_raw_json, fetched_at, expires_at
             FROM tracker_snapshots
             WHERE tracker = ? AND resource_kind = ? AND resource_key = ?",
        )
        .bind(tracker)
        .bind(kind)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(Cached {
            value: serde_json::from_str(row.get("normalized_json"))?,
            fetched_at: DateTime::parse_from_rfc3339(row.get("fetched_at"))?.with_timezone(&Utc),
            expires_at: DateTime::parse_from_rfc3339(row.get("expires_at"))?.with_timezone(&Utc),
        }))
    }

    pub async fn get_runtime_preferences(&self) -> Result<RuntimePreferences> {
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM runtime_preferences WHERE key = 'runtime'",
        )
        .fetch_optional(&self.pool)
        .await?;
        match value {
            Some(value) => Ok(serde_json::from_str(&value)?),
            None => Ok(RuntimePreferences::default()),
        }
    }

    pub async fn put_runtime_preferences(&self, preferences: &RuntimePreferences) -> Result<()> {
        sqlx::query(
            "INSERT INTO runtime_preferences (key, value_json, updated_at)
             VALUES ('runtime', ?, ?)
             ON CONFLICT (key) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at = excluded.updated_at",
        )
        .bind(serde_json::to_string(preferences)?)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
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
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO release_track_indexes (tracker, group_id, state, priority, updated_at)
             VALUES (?, ?, 'pending', ?, ?)
             ON CONFLICT (tracker, group_id) DO UPDATE SET
                priority = MAX(release_track_indexes.priority, excluded.priority),
                state = CASE
                    WHEN release_track_indexes.expires_at IS NOT NULL
                     AND release_track_indexes.expires_at <= excluded.updated_at
                    THEN 'pending'
                    ELSE release_track_indexes.state
                END,
                next_retry_at = CASE
                    WHEN release_track_indexes.expires_at IS NOT NULL
                     AND release_track_indexes.expires_at <= excluded.updated_at
                    THEN NULL
                    ELSE release_track_indexes.next_retry_at
                END,
                updated_at = CASE
                    WHEN release_track_indexes.expires_at IS NOT NULL
                     AND release_track_indexes.expires_at <= excluded.updated_at
                    THEN excluded.updated_at
                    ELSE release_track_indexes.updated_at
                END",
        )
        .bind(tracker)
        .bind(group_id)
        .bind(priority)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn ensure_single_coverage(&self, tracker: &str, group_id: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO single_album_coverages
                (tracker, single_group_id, state, updated_at)
             VALUES (?, ?, 'pending', ?)
             ON CONFLICT (tracker, single_group_id) DO NOTHING",
        )
        .bind(tracker)
        .bind(group_id)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn due_track_indexes(&self, limit: i64) -> Result<Vec<TrackIndexJob>> {
        let rows = sqlx::query(
            "SELECT tracker, group_id FROM release_track_indexes
             WHERE state IN ('pending', 'failed')
               AND (next_retry_at IS NULL OR next_retry_at <= ?)
             ORDER BY priority DESC, updated_at ASC LIMIT ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| TrackIndexJob {
                tracker: row.get("tracker"),
                group_id: row.get("group_id"),
            })
            .collect())
    }

    pub async fn set_track_index_resolving(&self, tracker: &str, group_id: i64) -> Result<()> {
        sqlx::query(
            "UPDATE release_track_indexes
             SET state = 'resolving', updated_at = ?
             WHERE tracker = ? AND group_id = ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(tracker)
        .bind(group_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn put_track_index(&self, index: &ReleaseTrackIndex) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE release_track_indexes SET
                state = 'indexed', index_json = ?, attempts = 0,
                next_retry_at = NULL, error_message = NULL,
                fetched_at = ?, expires_at = ?, updated_at = ?
             WHERE tracker = ? AND group_id = ?",
        )
        .bind(serde_json::to_string(index)?)
        .bind(now.to_rfc3339())
        .bind((now + chrono::Duration::hours(24)).to_rfc3339())
        .bind(now.to_rfc3339())
        .bind(&index.tracker)
        .bind(index.group_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn fail_track_index(
        &self,
        tracker: &str,
        group_id: i64,
        message: &str,
    ) -> Result<()> {
        let attempts: i64 = sqlx::query_scalar(
            "SELECT attempts FROM release_track_indexes WHERE tracker = ? AND group_id = ?",
        )
        .bind(tracker)
        .bind(group_id)
        .fetch_one(&self.pool)
        .await?;
        let delay = chrono::Duration::seconds((30_i64 * (1_i64 << attempts.min(7))).min(3600));
        sqlx::query(
            "UPDATE release_track_indexes SET
                state = 'failed', attempts = attempts + 1,
                next_retry_at = ?, error_message = ?, updated_at = ?
             WHERE tracker = ? AND group_id = ?",
        )
        .bind((Utc::now() + delay).to_rfc3339())
        .bind(message.chars().take(500).collect::<String>())
        .bind(Utc::now().to_rfc3339())
        .bind(tracker)
        .bind(group_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn recover_track_indexes(&self) -> Result<()> {
        sqlx::query(
            "UPDATE release_track_indexes
             SET state = 'pending', next_retry_at = NULL, updated_at = ?
             WHERE state = 'resolving'",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn replace_catalog_memberships(
        &self,
        tracker: &str,
        catalog: &ArtistCatalogPage,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM dedupe_catalog_memberships WHERE tracker = ? AND artist_id = ?")
            .bind(tracker)
            .bind(catalog.artist.artist_id)
            .execute(&mut *transaction)
            .await?;
        let now = Utc::now().to_rfc3339();
        for group in &catalog.groups {
            sqlx::query(
                "INSERT INTO dedupe_catalog_memberships
                    (tracker, artist_id, group_id, group_json, updated_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(tracker)
            .bind(catalog.artist.artist_id)
            .bind(group.release.group_id)
            .bind(serde_json::to_string(group)?)
            .bind(&now)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn list_catalog_memberships(&self) -> Result<Vec<CatalogMembership>> {
        let rows = sqlx::query("SELECT artist_id, group_json FROM dedupe_catalog_memberships")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(CatalogMembership {
                    artist_id: row.get("artist_id"),
                    group: serde_json::from_str(row.get("group_json"))?,
                })
            })
            .collect()
    }

    pub async fn list_track_indexes(&self) -> Result<Vec<StoredTrackIndex>> {
        let rows =
            sqlx::query("SELECT tracker, group_id, state, index_json FROM release_track_indexes")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter()
            .map(|row| {
                let value: Option<&str> = row.get("index_json");
                Ok(StoredTrackIndex {
                    tracker: row.get("tracker"),
                    group_id: row.get("group_id"),
                    state: row.get("state"),
                    index: value.map(serde_json::from_str).transpose()?,
                })
            })
            .collect()
    }

    pub async fn put_single_coverage(
        &self,
        tracker: &str,
        group_id: i64,
        state: &str,
        coverage: Option<&RawSingleCoverage>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO single_album_coverages
                (tracker, single_group_id, state, coverage_json, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (tracker, single_group_id) DO UPDATE SET
                state = excluded.state,
                coverage_json = excluded.coverage_json,
                updated_at = excluded.updated_at",
        )
        .bind(tracker)
        .bind(group_id)
        .bind(state)
        .bind(coverage.map(serde_json::to_string).transpose()?)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_single_coverage(
        &self,
        tracker: &str,
        group_id: i64,
    ) -> Result<Option<StoredCoverage>> {
        let row = sqlx::query(
            "SELECT state, coverage_json FROM single_album_coverages
             WHERE tracker = ? AND single_group_id = ?",
        )
        .bind(tracker)
        .bind(group_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let value: Option<&str> = row.get("coverage_json");
            Ok(StoredCoverage {
                state: row.get("state"),
                coverage: value.map(serde_json::from_str).transpose()?,
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
        sqlx::query(
            "INSERT INTO tracker_snapshots
                (tracker, resource_kind, resource_key, normalized_json, sanitized_raw_json, fetched_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (tracker, resource_kind, resource_key) DO UPDATE SET
                normalized_json = excluded.normalized_json,
                sanitized_raw_json = excluded.sanitized_raw_json,
                fetched_at = excluded.fetched_at,
                expires_at = excluded.expires_at,
                schema_version = 1",
        )
        .bind(tracker)
        .bind(kind)
        .bind(key)
        .bind(serde_json::to_string(value)?)
        .bind(serde_json::to_string(raw)?)
        .bind(fetched_at.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
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
        if let Some(existing) = self.find_job(tracker, torrent_id, profile).await? {
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
        sqlx::query(
            "INSERT INTO download_jobs
             (id, idempotency_key, tracker, torrent_id, profile, use_token, state, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(job.id.to_string())
        .bind(idempotency_key)
        .bind(tracker)
        .bind(torrent_id)
        .bind(profile)
        .bind(use_token)
        .bind(job.state.as_str())
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&self.pool)
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
        sqlx::query(
            "UPDATE download_jobs SET group_id = ?, info_hash = ?, name = ?, updated_at = ? WHERE id = ?",
        )
        .bind(group_id)
        .bind(info_hash)
        .bind(name)
        .bind(Utc::now().to_rfc3339())
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_job_state(
        &self,
        id: Uuid,
        state: DownloadState,
        error: Option<(&str, &str)>,
    ) -> Result<()> {
        let (code, message) = error.unwrap_or(("", ""));
        sqlx::query(
            "UPDATE download_jobs SET state = ?, error_code = NULLIF(?, ''), error_message = NULLIF(?, ''), updated_at = ? WHERE id = ?",
        )
        .bind(state.as_str())
        .bind(code)
        .bind(message)
        .bind(Utc::now().to_rfc3339())
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
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
        sqlx::query(
            "UPDATE download_jobs
             SET state = ?, progress = ?, download_speed = ?, upload_speed = ?, eta = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(state.as_str())
        .bind(progress)
        .bind(download_speed)
        .bind(upload_speed)
        .bind(eta)
        .bind(Utc::now().to_rfc3339())
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_jobs(&self) -> Result<Vec<DownloadJob>> {
        let rows = sqlx::query("SELECT * FROM download_jobs ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_job).collect()
    }

    pub async fn put_canonical(
        &self,
        canonical: &CanonicalTorrent,
        fetched_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        let mut canonical = canonical.clone();
        let existing_torrent = sqlx::query(
            "SELECT canonical_json FROM canonical_torrents
             WHERE tracker = ? AND torrent_id = ?",
        )
        .bind(&canonical.release.tracker)
        .bind(canonical.variant.torrent_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = &existing_torrent {
            let previous: CanonicalTorrent = serde_json::from_str(row.get("canonical_json"))?;
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
            let existing = if existing_torrent.is_some() {
                existing_torrent
            } else {
                sqlx::query(
                    "SELECT canonical_json FROM canonical_torrents
                     WHERE tracker = ? AND group_id = ? LIMIT 1",
                )
                .bind(&canonical.release.tracker)
                .bind(canonical.release.group_id)
                .fetch_optional(&self.pool)
                .await?
            };
            if let Some(row) = existing {
                let previous: CanonicalTorrent = serde_json::from_str(row.get("canonical_json"))?;
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
        sqlx::query(
            "INSERT INTO canonical_torrents
                (tracker, torrent_id, group_id, info_hash, canonical_json, fetched_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (tracker, torrent_id) DO UPDATE SET
                group_id = excluded.group_id,
                info_hash = excluded.info_hash,
                canonical_json = excluded.canonical_json,
                fetched_at = excluded.fetched_at,
                expires_at = excluded.expires_at",
        )
        .bind(&canonical.release.tracker)
        .bind(canonical.variant.torrent_id)
        .bind(canonical.release.group_id)
        .bind(canonical.variant.info_hash.as_deref())
        .bind(serde_json::to_string(&canonical)?)
        .bind(fetched_at.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        self.replace_release_artists(&canonical).await?;
        Ok(())
    }

    async fn replace_release_artists(&self, canonical: &CanonicalTorrent) -> Result<()> {
        if canonical.release.artists.is_empty() {
            return Ok(());
        }
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM canonical_release_artists WHERE tracker = ? AND group_id = ?")
            .bind(&canonical.release.tracker)
            .bind(canonical.release.group_id)
            .execute(&mut *transaction)
            .await?;
        for artist in &canonical.release.artists {
            sqlx::query(
                "INSERT INTO canonical_release_artists
                    (tracker, group_id, artist_key, artist_id, name, sort_name, role, source)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&canonical.release.tracker)
            .bind(canonical.release.group_id)
            .bind(&artist.key)
            .bind(artist.artist_id)
            .bind(&artist.name)
            .bind(artist_sort_name(&artist.name))
            .bind(match artist.role {
                crate::model::ArtistRole::Primary => "primary",
                crate::model::ArtistRole::Guest => "guest",
            })
            .bind(match artist.source {
                ArtistCreditSource::Structured => "structured",
                ArtistCreditSource::DisplayFallback => "display_fallback",
            })
            .execute(&mut *transaction)
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
        let row = sqlx::query(
            "SELECT canonical_json, fetched_at, expires_at
             FROM canonical_torrents WHERE tracker = ? AND torrent_id = ?",
        )
        .bind(tracker)
        .bind(torrent_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(canonical_from_row).transpose()
    }

    pub async fn list_canonical_for_tracker(&self, tracker: &str) -> Result<Vec<CanonicalTorrent>> {
        let rows = sqlx::query("SELECT canonical_json FROM canonical_torrents WHERE tracker = ?")
            .bind(tracker)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| serde_json::from_str(row.get("canonical_json")).map_err(Into::into))
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
        let state = if tracker.is_some() {
            "pending"
        } else {
            "unconfigured"
        };
        sqlx::query(
            "INSERT INTO download_release_links
                (client, info_hash, announce_host, tracker, resolution_state,
                 first_seen_at, last_seen_at, updated_at, present,
                 missing_since, library_added_at, completed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, NULL, ?, ?)
             ON CONFLICT (client, info_hash) DO UPDATE SET
                announce_host = excluded.announce_host,
                tracker = CASE
                    WHEN download_release_links.resolution_state = 'linked'
                        THEN download_release_links.tracker
                    ELSE excluded.tracker
                END,
                resolution_state = CASE
                    WHEN download_release_links.resolution_state = 'linked'
                        THEN 'linked'
                    WHEN COALESCE(download_release_links.tracker, '') != COALESCE(excluded.tracker, '')
                        THEN excluded.resolution_state
                    ELSE download_release_links.resolution_state
                END,
                last_seen_at = excluded.last_seen_at,
                present = 1,
                missing_since = NULL,
                library_added_at = COALESCE(
                    download_release_links.library_added_at,
                    excluded.library_added_at
                ),
                completed_at = COALESCE(
                    download_release_links.completed_at,
                    excluded.completed_at
                ),
                updated_at = CASE
                    WHEN download_release_links.resolution_state = 'linked'
                        THEN download_release_links.updated_at
                    ELSE excluded.updated_at
                END",
        )
        .bind(&live.client)
        .bind(live.info_hash.to_ascii_lowercase())
        .bind(announce_host)
        .bind(tracker)
        .bind(state)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .bind(&completed_at)
        .bind(&completed_at)
        .execute(&self.pool)
        .await?;
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
        sqlx::query(
            "INSERT INTO download_release_links
                (client, info_hash, tracker, group_id, torrent_id, resolution_state,
                 first_seen_at, last_seen_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (client, info_hash) DO UPDATE SET
                tracker = excluded.tracker,
                group_id = excluded.group_id,
                torrent_id = excluded.torrent_id,
                resolution_state = excluded.resolution_state,
                next_retry_at = NULL,
                error_code = NULL,
                error_message = NULL,
                last_seen_at = excluded.last_seen_at,
                updated_at = excluded.updated_at",
        )
        .bind(client)
        .bind(info_hash.to_ascii_lowercase())
        .bind(tracker)
        .bind(group_id)
        .bind(torrent_id)
        .bind(if linked { "linked" } else { "pending" })
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn due_links(&self, limit: i64) -> Result<Vec<DownloadReleaseLink>> {
        let rows = sqlx::query(
            "SELECT * FROM download_release_links
             WHERE tracker IS NOT NULL
               AND resolution_state IN ('pending', 'failed')
               AND (next_retry_at IS NULL OR next_retry_at <= ?)
             ORDER BY updated_at ASC LIMIT ?",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(link_from_row).collect()
    }

    pub async fn recover_resolving_links(&self) -> Result<()> {
        sqlx::query(
            "UPDATE download_release_links
             SET resolution_state = 'pending', next_retry_at = NULL, updated_at = ?
             WHERE resolution_state = 'resolving'",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn complete_client_scan(
        &self,
        client: &str,
        scan_started_at: DateTime<Utc>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE download_release_links
             SET present = 0, missing_since = COALESCE(missing_since, ?)
             WHERE client = ? AND last_seen_at < ? AND library_added_at IS NOT NULL",
        )
        .bind(&now)
        .bind(client)
        .bind(scan_started_at.to_rfc3339())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "DELETE FROM download_release_links
             WHERE client = ? AND last_seen_at < ? AND library_added_at IS NULL",
        )
        .bind(client)
        .bind(scan_started_at.to_rfc3339())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO download_client_scans (client, last_successful_at)
             VALUES (?, ?)
             ON CONFLICT (client) DO UPDATE SET last_successful_at = excluded.last_successful_at",
        )
        .bind(client)
        .bind(&now)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn set_link_resolving(&self, client: &str, info_hash: &str) -> Result<()> {
        sqlx::query(
            "UPDATE download_release_links
             SET resolution_state = 'resolving', updated_at = ?
             WHERE client = ? AND info_hash = ? AND resolution_state != 'linked'",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(client)
        .bind(info_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_linked(
        &self,
        client: &str,
        info_hash: &str,
        canonical: &CanonicalTorrent,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE download_release_links SET
                tracker = ?, group_id = ?, torrent_id = ?,
                resolution_state = 'linked', next_retry_at = NULL,
                error_code = NULL, error_message = NULL, updated_at = ?
             WHERE client = ? AND info_hash = ?",
        )
        .bind(&canonical.release.tracker)
        .bind(canonical.release.group_id)
        .bind(canonical.variant.torrent_id)
        .bind(Utc::now().to_rfc3339())
        .bind(client)
        .bind(info_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_link_failure(
        &self,
        client: &str,
        info_hash: &str,
        not_found: bool,
        message: &str,
    ) -> Result<()> {
        let row = sqlx::query(
            "SELECT attempts, first_seen_at FROM download_release_links
             WHERE client = ? AND info_hash = ?",
        )
        .bind(client)
        .bind(info_hash)
        .fetch_one(&self.pool)
        .await?;
        let attempts: i64 = row.get("attempts");
        let first_seen =
            DateTime::parse_from_rfc3339(row.get("first_seen_at"))?.with_timezone(&Utc);
        let next_attempt = attempts + 1;
        let old_enough = Utc::now() - first_seen >= chrono::Duration::hours(24);
        let state = if not_found && old_enough {
            "not_found"
        } else {
            "failed"
        };
        let delay = if not_found {
            chrono::Duration::hours(1)
        } else {
            chrono::Duration::seconds((30_i64 * (1_i64 << attempts.min(7))).min(3600))
        };
        sqlx::query(
            "UPDATE download_release_links SET
                resolution_state = ?, attempts = ?, next_retry_at = ?,
                error_code = ?, error_message = ?, updated_at = ?
             WHERE client = ? AND info_hash = ?",
        )
        .bind(state)
        .bind(next_attempt)
        .bind((!not_found || !old_enough).then(|| (Utc::now() + delay).to_rfc3339()))
        .bind(if not_found {
            "not_found"
        } else {
            "tracker_error"
        })
        .bind(message.chars().take(500).collect::<String>())
        .bind(Utc::now().to_rfc3339())
        .bind(client)
        .bind(info_hash)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn retry_link(&self, client: &str, info_hash: &str) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE download_release_links SET
                resolution_state = 'pending', next_retry_at = NULL,
                error_code = NULL, error_message = NULL, updated_at = ?
             WHERE client = ? AND info_hash = ?
               AND tracker IS NOT NULL AND resolution_state IN ('failed', 'not_found')",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(client)
        .bind(info_hash.to_ascii_lowercase())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_link(
        &self,
        client: &str,
        info_hash: &str,
    ) -> Result<Option<DownloadReleaseLink>> {
        let row =
            sqlx::query("SELECT * FROM download_release_links WHERE client = ? AND info_hash = ?")
                .bind(client)
                .bind(info_hash.to_ascii_lowercase())
                .fetch_optional(&self.pool)
                .await?;
        row.as_ref().map(link_from_row).transpose()
    }

    pub async fn index_counts(&self) -> Result<DownloadIndexCounts> {
        let rows = sqlx::query(
            "SELECT resolution_state, COUNT(*) AS count
             FROM download_release_links WHERE present = 1 GROUP BY resolution_state",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut counts = DownloadIndexCounts::default();
        for row in rows {
            let count = row.get("count");
            match row.get::<&str, _>("resolution_state") {
                "linked" => counts.linked = count,
                "pending" => counts.pending = count,
                "resolving" => counts.resolving = count,
                "unconfigured" => counts.unconfigured = count,
                "failed" | "not_found" => counts.failed += count,
                _ => {}
            }
        }
        Ok(counts)
    }

    pub async fn list_library_records(&self) -> Result<Vec<LibraryRecord>> {
        let rows = sqlx::query(
            "SELECT c.canonical_json, c.fetched_at, c.expires_at,
                    l.client, l.info_hash, l.present, l.library_added_at,
                    l.completed_at, l.last_seen_at, l.missing_since
             FROM download_release_links l
             JOIN canonical_torrents c
               ON c.tracker = l.tracker AND c.torrent_id = l.torrent_id
             WHERE l.resolution_state = 'linked' AND l.library_added_at IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(library_record_from_row).collect()
    }

    pub async fn last_successful_download_scan(&self) -> Result<Option<DateTime<Utc>>> {
        let value: Option<String> =
            sqlx::query_scalar("SELECT MIN(last_successful_at) FROM download_client_scans")
                .fetch_one(&self.pool)
                .await?;
        value
            .map(|value| Ok(DateTime::parse_from_rfc3339(&value)?.with_timezone(&Utc)))
            .transpose()
    }

    async fn find_job(
        &self,
        tracker: &str,
        torrent_id: i64,
        profile: &str,
    ) -> Result<Option<DownloadJob>> {
        let row = sqlx::query(
            "SELECT * FROM download_jobs WHERE tracker = ? AND torrent_id = ? AND profile = ?",
        )
        .bind(tracker)
        .bind(torrent_id)
        .bind(profile)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_job).transpose()
    }

    async fn add_event(&self, id: Uuid, state: &DownloadState, detail: Option<&str>) -> Result<()> {
        sqlx::query(
            "INSERT INTO download_events (job_id, state, detail, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(state.as_str())
        .bind(detail)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn canonical_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Cached<CanonicalTorrent>> {
    Ok(Cached {
        value: serde_json::from_str(row.get("canonical_json"))?,
        fetched_at: DateTime::parse_from_rfc3339(row.get("fetched_at"))?.with_timezone(&Utc),
        expires_at: DateTime::parse_from_rfc3339(row.get("expires_at"))?.with_timezone(&Utc),
    })
}

fn link_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<DownloadReleaseLink> {
    Ok(DownloadReleaseLink {
        client: row.get("client"),
        info_hash: row.get("info_hash"),
        tracker: row.get("tracker"),
        torrent_id: row.get("torrent_id"),
        resolution_state: row.get("resolution_state"),
    })
}

fn library_record_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<LibraryRecord> {
    let library_added_at =
        DateTime::parse_from_rfc3339(row.get("library_added_at"))?.with_timezone(&Utc);
    let completed_at: Option<&str> = row.get("completed_at");
    let missing_since: Option<&str> = row.get("missing_since");
    Ok(LibraryRecord {
        canonical: Cached {
            value: serde_json::from_str(row.get("canonical_json"))?,
            fetched_at: DateTime::parse_from_rfc3339(row.get("fetched_at"))?.with_timezone(&Utc),
            expires_at: DateTime::parse_from_rfc3339(row.get("expires_at"))?.with_timezone(&Utc),
        },
        client: row.get("client"),
        info_hash: row.get("info_hash"),
        present: row.get("present"),
        library_added_at,
        completed_at: completed_at
            .map(DateTime::parse_from_rfc3339)
            .transpose()?
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or(library_added_at),
        last_seen_at: DateTime::parse_from_rfc3339(row.get("last_seen_at"))?.with_timezone(&Utc),
        missing_since: missing_since
            .map(DateTime::parse_from_rfc3339)
            .transpose()?
            .map(|value| value.with_timezone(&Utc)),
    })
}

fn artist_sort_name(name: &str) -> String {
    let normalized = name.trim().to_lowercase();
    normalized
        .strip_prefix("the ")
        .unwrap_or(&normalized)
        .to_owned()
}

fn row_to_job(row: &sqlx::sqlite::SqliteRow) -> Result<DownloadJob> {
    Ok(DownloadJob {
        id: Uuid::parse_str(row.get("id"))?,
        tracker: row.get("tracker"),
        torrent_id: row.get("torrent_id"),
        group_id: row.get("group_id"),
        profile: row.get("profile"),
        use_token: row.get("use_token"),
        info_hash: row.get("info_hash"),
        name: row.get("name"),
        state: DownloadState::from_str(row.get("state"))?,
        progress: row.get("progress"),
        download_speed: row.get("download_speed"),
        upload_speed: row.get("upload_speed"),
        eta: row.get("eta"),
        error_code: row.get("error_code"),
        error_message: row.get("error_message"),
        created_at: DateTime::parse_from_rfc3339(row.get("created_at"))?.with_timezone(&Utc),
        updated_at: DateTime::parse_from_rfc3339(row.get("updated_at"))?.with_timezone(&Utc),
    })
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use tempfile::tempdir;

    use crate::{
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
    async fn persists_negative_results_and_recovers_in_progress_links() {
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("wotbox.sqlite"))
            .await
            .expect("database");
        let hash = "abcdef0123456789abcdef0123456789abcdef01";
        db.seed_download_link("music", hash, "ops", None, 20, false)
            .await
            .expect("link");
        sqlx::query("UPDATE download_release_links SET first_seen_at = ? WHERE client = 'music'")
            .bind((Utc::now() - Duration::hours(25)).to_rfc3339())
            .execute(&db.pool)
            .await
            .expect("age link");
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
