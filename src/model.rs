use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub tracker: String,
    pub fetched_at: DateTime<Utc>,
    pub cache_age_seconds: i64,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiEnvelope<T> {
    pub data: T,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: Option<i64>,
    pub username: String,
    pub uploaded: Option<i64>,
    pub downloaded: Option<i64>,
    pub ratio: Option<f64>,
    pub required_ratio: Option<f64>,
    pub user_class: Option<String>,
    pub bonus_points: Option<f64>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub current_page: i64,
    pub total_pages: i64,
    pub total_results: Option<i64>,
    pub groups: Vec<SearchGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchGroup {
    pub group_id: i64,
    pub name: String,
    pub artist: Option<String>,
    pub year: Option<i64>,
    pub release_type: Option<String>,
    pub image: Option<String>,
    pub tags: Vec<String>,
    pub torrents: Vec<SearchTorrent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchTorrent {
    pub torrent_id: i64,
    pub edition_id: Option<i64>,
    pub format: Option<String>,
    pub encoding: Option<String>,
    pub media: Option<String>,
    pub size: Option<i64>,
    pub seeders: Option<i64>,
    pub leechers: Option<i64>,
    pub snatched: Option<i64>,
    pub freeleech: bool,
    pub can_use_token: bool,
    pub remaster_title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TorrentMetadata {
    pub torrent_id: i64,
    pub group_id: Option<i64>,
    pub name: String,
    pub info_hash: String,
    pub can_use_token: bool,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProfile {
    pub name: String,
    pub client: String,
    pub save_path: String,
    pub tag: String,
    pub start_paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateDownload {
    pub tracker: String,
    pub torrent_id: i64,
    pub profile: String,
    #[serde(default)]
    pub use_token: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientDownloadState {
    Downloading,
    Seeding,
    Paused,
    Queued,
    Checking,
    Stalled,
    Complete,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientDownload {
    pub client: String,
    pub info_hash: String,
    pub name: String,
    pub state: ClientDownloadState,
    pub client_state: String,
    pub progress: f64,
    pub size: i64,
    pub downloaded: i64,
    pub uploaded: i64,
    pub download_speed: i64,
    pub upload_speed: i64,
    pub eta: Option<i64>,
    pub ratio: f64,
    pub save_path: String,
    pub category: String,
    pub tags: Vec<String>,
    pub tracker: Option<String>,
    pub added_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Queued,
    FetchingMetadata,
    Submitting,
    Active,
    Complete,
    Failed,
    Unknown,
}

impl DownloadState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::FetchingMetadata => "fetching_metadata",
            Self::Submitting => "submitting",
            Self::Active => "active",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

impl std::str::FromStr for DownloadState {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "queued" => Self::Queued,
            "fetching_metadata" => Self::FetchingMetadata,
            "submitting" => Self::Submitting,
            "active" => Self::Active,
            "complete" => Self::Complete,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadJob {
    pub id: Uuid,
    pub tracker: String,
    pub torrent_id: i64,
    pub group_id: Option<i64>,
    pub profile: String,
    pub use_token: bool,
    pub info_hash: Option<String>,
    pub name: Option<String>,
    pub state: DownloadState,
    pub progress: f64,
    pub download_speed: i64,
    pub upload_speed: i64,
    pub eta: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicConfig {
    pub base_path: String,
    pub trackers: Vec<String>,
    pub download_profiles: Vec<String>,
}

pub fn sanitized(mut value: Value) -> Value {
    redact(&mut value);
    value
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
                if matches!(
                    normalized.as_str(),
                    "authkey" | "passkey" | "token" | "apikey" | "torrentpass"
                ) {
                    *child = Value::String("[redacted]".into());
                } else {
                    redact(child);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact),
        _ => {}
    }
}

pub fn value_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value.as_i64().or_else(|| value.as_str()?.parse().ok())
    })
}

pub fn value_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value.as_f64().or_else(|| value.as_str()?.parse().ok())
    })
}

pub fn value_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key)?.as_str().map(ToOwned::to_owned))
}

pub fn value_bool(value: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .find_map(|key| {
            let value = value.get(*key)?;
            value.as_bool().or_else(|| value.as_i64().map(|v| v != 0))
        })
        .unwrap_or(false)
}
