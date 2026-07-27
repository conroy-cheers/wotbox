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

use crate::model::{DownloadJob, DownloadState};

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

pub struct Cached<T> {
    pub value: T,
    pub fetched_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
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
