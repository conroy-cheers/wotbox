use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode, multipart};
use serde::Deserialize;
use std::time::Duration;
use url::Url;

use crate::{
    config::{DownloadClientConfig, read_secret},
    model::{
        ClientDownloadState, DownloadDiagnostic, DownloadProfile, LiveDownloadStatus,
        ObservedDownload,
    },
};

#[async_trait]
pub trait DownloadClient: Send + Sync {
    async fn health(&self) -> Result<String>;
    async fn downloads(&self, limit: u32, offset: u32) -> Result<Vec<ObservedDownload>>;
    async fn download(&self, info_hash: &str) -> Result<Option<ObservedDownload>>;
    async fn downloads_by_hashes(&self, info_hashes: &[String]) -> Result<Vec<ObservedDownload>>;
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
    ) -> Result<Vec<ObservedDownload>> {
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
    tracker: String,
    #[serde(default)]
    added_on: i64,
    #[serde(default)]
    completion_on: i64,
}

#[derive(Debug, Deserialize)]
struct AddTorrentResult {
    #[serde(default)]
    success_count: u64,
    #[serde(default)]
    failure_count: u64,
}

impl QbitTorrent {
    fn normalized(self, client: &str) -> ObservedDownload {
        ObservedDownload {
            announce_host: announce_host(&self.tracker),
            live: LiveDownloadStatus {
                client: client.to_owned(),
                info_hash: self.hash.to_ascii_lowercase(),
                state: normalize_state(&self.state, self.progress),
                diagnostic: download_diagnostic(&self.state),
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
                added_at: unix_timestamp(self.added_on),
                completed_at: unix_timestamp(self.completion_on),
            },
        }
    }
}

fn announce_host(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    let host = url.host_str()?.trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn normalize_state(state: &str, progress: f64) -> ClientDownloadState {
    match state.to_ascii_lowercase().as_str() {
        "error" | "missingfiles" => ClientDownloadState::Error,
        "pauseddl" | "pausedup" | "stoppeddl" | "stoppedup" => ClientDownloadState::Paused,
        "queueddl" | "queuedup" => ClientDownloadState::Queued,
        "checkingdl" | "checkingup" | "checkingresumedata" | "moving" | "allocating" => {
            ClientDownloadState::Checking
        }
        "stalledup" if progress >= 1.0 => ClientDownloadState::Seeding,
        "stalleddl" | "stalledup" => ClientDownloadState::Stalled,
        "uploading" | "forcedup" => ClientDownloadState::Seeding,
        "downloading" | "metadl" | "forceddl" => ClientDownloadState::Downloading,
        _ if progress >= 1.0 => ClientDownloadState::Complete,
        _ => ClientDownloadState::Unknown,
    }
}

fn download_diagnostic(state: &str) -> Option<DownloadDiagnostic> {
    match state.to_ascii_lowercase().as_str() {
        "missingfiles" => Some(DownloadDiagnostic {
            code: "missing_files".into(),
            summary: "Files are missing".into(),
            message: "qBittorrent cannot find some or all of the torrent payload at its configured save path.".into(),
            action: "Restore or remount the files at that path, or correct the torrent's location, then run Force recheck in qBittorrent.".into(),
        }),
        "error" => Some(DownloadDiagnostic {
            code: "client_error".into(),
            summary: "qBittorrent reported an error".into(),
            message: "The torrent status API does not provide the client's underlying error message.".into(),
            action: "Check qBittorrent's execution log for a disk, permission, or I/O error, correct the cause, then run Force recheck.".into(),
        }),
        _ => None,
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

    async fn downloads(&self, limit: u32, offset: u32) -> Result<Vec<ObservedDownload>> {
        self.fetch_downloads(None, Some(limit), Some(offset)).await
    }

    async fn download(&self, info_hash: &str) -> Result<Option<ObservedDownload>> {
        Ok(self
            .fetch_downloads(Some(info_hash), None, None)
            .await?
            .into_iter()
            .next())
    }

    async fn downloads_by_hashes(&self, info_hashes: &[String]) -> Result<Vec<ObservedDownload>> {
        if info_hashes.is_empty() {
            return Ok(Vec::new());
        }
        self.fetch_downloads(Some(&info_hashes.join("|")), None, None)
            .await
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
        let body = body.trim();
        if body == "Ok." {
            return Ok(());
        }
        if let Ok(result) = serde_json::from_str::<AddTorrentResult>(body)
            && result.success_count > 0
            && result.failure_count == 0
        {
            return Ok(());
        }
        bail!("qBittorrent rejected add request: {body}")
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

    use super::{
        ClientDownloadState, DownloadClient, QbittorrentClient, announce_host, download_diagnostic,
        normalize_state,
    };

    #[test]
    fn normalizes_qbittorrent_states() {
        assert_eq!(
            normalize_state("downloading", 0.4),
            ClientDownloadState::Downloading
        );
        assert_eq!(
            normalize_state("stalledUP", 1.0),
            ClientDownloadState::Seeding
        );
        assert_eq!(
            normalize_state("stalledDL", 0.8),
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

    #[test]
    fn extracts_only_normalized_announce_hostname() {
        assert_eq!(
            announce_host("https://PASSKEY@Home.Opsfet.Ch:443/abc/announce?token=secret"),
            Some("home.opsfet.ch".into())
        );
        assert_eq!(announce_host("not a URL"), None);
    }

    #[test]
    fn explains_qbittorrent_error_states() {
        let missing = download_diagnostic("missingFiles").expect("missing files diagnostic");
        assert_eq!(missing.code, "missing_files");
        assert!(missing.message.contains("save path"));
        assert!(missing.action.contains("Force recheck"));

        let generic = download_diagnostic("error").expect("generic error diagnostic");
        assert_eq!(generic.code, "client_error");
        assert!(generic.action.contains("execution log"));
        assert!(download_diagnostic("downloading").is_none());
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
        assert_eq!(download.live.client, "music");
        assert_eq!(download.live.info_hash, info_hash);
        assert_eq!(download.live.state, ClientDownloadState::Downloading);
        assert_eq!(download.live.downloaded, 1024);
        assert!(download.live.added_at.is_some());
        assert_eq!(download.announce_host.as_deref(), Some("tracker.invalid"));
        let public_json = serde_json::to_string(&download.live).expect("serialize live status");
        assert!(!public_json.contains("tracker.invalid"));
        assert!(!public_json.contains("A tracker download"));
        assert!(!public_json.contains("\"tags\""));
    }

    #[tokio::test]
    async fn accepts_structured_add_success_responses() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/add"))
            .and(header(
                "authorization",
                "Bearer qbt_0123456789012345678901234567",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "added_torrent_ids": ["abcdef0123456789abcdef0123456789abcdef01"],
                "failure_count": 0,
                "pending_count": 0,
                "success_count": 1
            })))
            .mount(&server)
            .await;
        let client = test_client(&server);

        client
            .add_torrent(b"d4:infode".to_vec(), "ops-99.torrent", &test_profile())
            .await
            .expect("structured success");
    }

    #[tokio::test]
    async fn rejects_structured_add_failure_responses() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/add"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "added_torrent_ids": [],
                "failure_count": 1,
                "pending_count": 0,
                "success_count": 0
            })))
            .mount(&server)
            .await;
        let client = test_client(&server);

        assert!(
            client
                .add_torrent(b"d4:infode".to_vec(), "ops-99.torrent", &test_profile(),)
                .await
                .is_err()
        );
    }

    fn test_client(server: &MockServer) -> QbittorrentClient {
        let directory = tempdir().expect("temporary directory");
        let key_path = directory.path().join("qbit-key");
        std::fs::write(&key_path, "qbt_0123456789012345678901234567").expect("write key");
        QbittorrentClient::new(
            "music".into(),
            &DownloadClientConfig {
                kind: DownloadClientKind::Qbittorrent,
                base_url: server.uri(),
                api_key_file: key_path,
            },
        )
        .expect("client")
    }

    fn test_profile() -> crate::model::DownloadProfile {
        crate::model::DownloadProfile {
            name: "ops".into(),
            client: "music".into(),
            save_path: "/downloads/ops".into(),
            tag: "ops".into(),
            start_paused: false,
        }
    }
}
