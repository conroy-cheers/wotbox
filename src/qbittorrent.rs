use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode, multipart};
use serde::Deserialize;
use std::time::Duration;

use crate::{
    config::{DownloadClientConfig, read_secret},
    model::{ClientDownload, ClientDownloadState, DownloadProfile},
};

#[async_trait]
pub trait DownloadClient: Send + Sync {
    async fn health(&self) -> Result<String>;
    async fn downloads(&self, limit: u32, offset: u32) -> Result<Vec<ClientDownload>>;
    async fn download(&self, info_hash: &str) -> Result<Option<ClientDownload>>;
    async fn add_torrent(
        &self,
        bytes: Vec<u8>,
        file_name: &str,
        profile: &DownloadProfile,
    ) -> Result<()>;
}

pub struct QbittorrentClient {
    name: String,
    base_url: String,
    api_key: String,
    client: Client,
}

impl QbittorrentClient {
    pub fn new(name: String, config: &DownloadClientConfig) -> Result<Self> {
        let api_key = read_secret(&config.api_key_file)?;
        if !api_key.starts_with("qbt_") || api_key.chars().count() != 32 {
            bail!("qBittorrent API key must be a 32-character qbt_ key");
        }
        Ok(Self {
            name,
            base_url: config.base_url.trim_end_matches('/').into(),
            api_key,
            client: Client::builder().timeout(Duration::from_secs(30)).build()?,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{}", self.base_url, path))
            .bearer_auth(&self.api_key)
    }

    async fn fetch_downloads(
        &self,
        info_hash: Option<&str>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<ClientDownload>> {
        let mut request = self.request(reqwest::Method::GET, "/api/v2/torrents/info");
        if let Some(info_hash) = info_hash {
            request = request.query(&[("hashes", info_hash)]);
        }
        if let Some(limit) = limit {
            request = request.query(&[
                ("sort", "added_on".to_owned()),
                ("reverse", "true".to_owned()),
                ("limit", limit.to_string()),
                ("offset", offset.unwrap_or_default().to_string()),
            ]);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            bail!("qBittorrent list returned HTTP {}", response.status());
        }
        let torrents: Vec<QbitTorrent> = response.json().await?;
        Ok(torrents
            .into_iter()
            .map(|torrent| torrent.normalized(&self.name))
            .collect())
    }
}

#[derive(Debug, Deserialize)]
struct QbitTorrent {
    #[serde(default)]
    hash: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    progress: f64,
    #[serde(default)]
    dlspeed: i64,
    #[serde(default)]
    upspeed: i64,
    #[serde(default)]
    eta: i64,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    downloaded: i64,
    #[serde(default)]
    uploaded: i64,
    #[serde(default)]
    ratio: f64,
    #[serde(default)]
    save_path: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    tracker: String,
    #[serde(default)]
    added_on: i64,
    #[serde(default)]
    completion_on: i64,
}

impl QbitTorrent {
    fn normalized(self, client: &str) -> ClientDownload {
        ClientDownload {
            client: client.to_owned(),
            info_hash: self.hash.to_ascii_lowercase(),
            name: self.name,
            state: normalize_state(&self.state, self.progress),
            client_state: self.state,
            progress: self.progress,
            size: self.size,
            downloaded: self.downloaded,
            uploaded: self.uploaded,
            download_speed: self.dlspeed,
            upload_speed: self.upspeed,
            eta: (self.eta >= 0 && self.eta < 8_640_000).then_some(self.eta),
            ratio: self.ratio,
            save_path: self.save_path,
            category: self.category,
            tags: self
                .tags
                .split(',')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            tracker: (!self.tracker.is_empty()).then_some(self.tracker),
            added_at: unix_timestamp(self.added_on),
            completed_at: unix_timestamp(self.completion_on),
        }
    }
}

fn normalize_state(state: &str, progress: f64) -> ClientDownloadState {
    match state.to_ascii_lowercase().as_str() {
        "error" | "missingfiles" => ClientDownloadState::Error,
        "pauseddl" | "pausedup" | "stoppeddl" | "stoppedup" => ClientDownloadState::Paused,
        "queueddl" | "queuedup" => ClientDownloadState::Queued,
        "checkingdl" | "checkingup" | "checkingresumedata" | "moving" | "allocating" => {
            ClientDownloadState::Checking
        }
        "stalleddl" | "stalledup" => ClientDownloadState::Stalled,
        "uploading" | "forcedup" => ClientDownloadState::Seeding,
        "downloading" | "metadl" | "forceddl" => ClientDownloadState::Downloading,
        _ if progress >= 1.0 => ClientDownloadState::Complete,
        _ => ClientDownloadState::Unknown,
    }
}

fn unix_timestamp(value: i64) -> Option<DateTime<Utc>> {
    (value > 0)
        .then(|| DateTime::from_timestamp(value, 0))
        .flatten()
}

#[async_trait]
impl DownloadClient for QbittorrentClient {
    async fn health(&self) -> Result<String> {
        let response = self
            .request(reqwest::Method::GET, "/api/v2/app/version")
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("qBittorrent health returned HTTP {}", response.status());
        }
        Ok(response.text().await?.trim().to_owned())
    }

    async fn downloads(&self, limit: u32, offset: u32) -> Result<Vec<ClientDownload>> {
        self.fetch_downloads(None, Some(limit), Some(offset)).await
    }

    async fn download(&self, info_hash: &str) -> Result<Option<ClientDownload>> {
        Ok(self
            .fetch_downloads(Some(info_hash), None, None)
            .await?
            .into_iter()
            .next())
    }

    async fn add_torrent(
        &self,
        bytes: Vec<u8>,
        file_name: &str,
        profile: &DownloadProfile,
    ) -> Result<()> {
        let torrent = multipart::Part::bytes(bytes)
            .file_name(file_name.to_owned())
            .mime_str("application/x-bittorrent")?;
        let mut form = multipart::Form::new()
            .part("torrents", torrent)
            .text("savepath", profile.save_path.clone())
            .text("tags", profile.tag.clone());
        if profile.start_paused {
            form = form.text("stopped", "true");
        }
        let response = self
            .request(reqwest::Method::POST, "/api/v2/torrents/add")
            .multipart(form)
            .send()
            .await
            .context("submit torrent to qBittorrent")?;
        if response.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE {
            bail!("qBittorrent rejected the torrent payload");
        }
        if !response.status().is_success() {
            bail!("qBittorrent add returned HTTP {}", response.status());
        }
        let body = response.text().await?;
        if body.trim() != "Ok." {
            bail!("qBittorrent rejected add request: {}", body.trim());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{DownloadClientConfig, DownloadClientKind};
    use tempfile::tempdir;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path, query_param},
    };

    use super::{ClientDownloadState, DownloadClient, QbittorrentClient, normalize_state};

    #[test]
    fn normalizes_qbittorrent_states() {
        assert_eq!(
            normalize_state("downloading", 0.4),
            ClientDownloadState::Downloading
        );
        assert_eq!(
            normalize_state("stalledUP", 1.0),
            ClientDownloadState::Stalled
        );
        assert_eq!(
            normalize_state("missingFiles", 0.8),
            ClientDownloadState::Error
        );
        assert_eq!(
            normalize_state("someFutureCompleteState", 1.0),
            ClientDownloadState::Complete
        );
    }

    #[tokio::test]
    async fn reads_download_detail_from_qbittorrent() {
        let server = MockServer::start().await;
        let info_hash = "abcdef0123456789abcdef0123456789abcdef01";
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .and(query_param("hashes", info_hash))
            .and(header(
                "authorization",
                "Bearer qbt_0123456789012345678901234567",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "hash": info_hash.to_uppercase(),
                    "name": "A tracker download",
                    "state": "downloading",
                    "progress": 0.42,
                    "size": 2048,
                    "downloaded": 1024,
                    "uploaded": 512,
                    "dlspeed": 100,
                    "upspeed": 20,
                    "eta": 90,
                    "ratio": 0.5,
                    "save_path": "/downloads/ops",
                    "category": "music",
                    "tags": "ops, flac",
                    "tracker": "https://tracker.invalid/announce",
                    "added_on": 1_700_000_000
                }])),
            )
            .mount(&server)
            .await;

        let directory = tempdir().expect("temporary directory");
        let key_path = directory.path().join("qbit-key");
        std::fs::write(&key_path, "qbt_0123456789012345678901234567").expect("write key");
        let client = QbittorrentClient::new(
            "music".into(),
            &DownloadClientConfig {
                kind: DownloadClientKind::Qbittorrent,
                base_url: server.uri(),
                api_key_file: key_path,
            },
        )
        .expect("client");

        let download = client
            .download(info_hash)
            .await
            .expect("qBittorrent response")
            .expect("download");
        assert_eq!(download.client, "music");
        assert_eq!(download.info_hash, info_hash);
        assert_eq!(download.name, "A tracker download");
        assert_eq!(download.state, ClientDownloadState::Downloading);
        assert_eq!(download.tags, ["ops", "flac"]);
        assert_eq!(download.downloaded, 1024);
        assert!(download.added_at.is_some());
    }
}
