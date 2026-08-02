use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode, multipart};
use serde::Deserialize;
use std::{future::Future, time::Duration};
use url::Url;

use crate::{
    config::{DownloadClientConfig, read_secret},
    model::{
        ClientDownloadState, DownloadDiagnostic, DownloadFile, DownloadProfile,
        DownloadTrackerStatus, LiveDownloadStatus, ObservedDownload,
    },
    provider::{ProviderFailure, ProviderFailureKind, ProviderGovernor, RequestClass, retry_after},
};

#[async_trait]
pub trait DownloadClient: Send + Sync {
    async fn free_space(&self) -> Result<i64>;
    async fn downloads(&self, limit: u32, offset: u32) -> Result<Vec<ObservedDownload>>;
    async fn downloads_with_class(
        &self,
        limit: u32,
        offset: u32,
        _class: RequestClass,
    ) -> Result<Vec<ObservedDownload>> {
        self.downloads(limit, offset).await
    }
    async fn download(&self, info_hash: &str) -> Result<Option<ObservedDownload>>;
    async fn download_with_class(
        &self,
        info_hash: &str,
        _class: RequestClass,
    ) -> Result<Option<ObservedDownload>> {
        self.download(info_hash).await
    }
    async fn downloads_by_hashes(&self, info_hashes: &[String]) -> Result<Vec<ObservedDownload>>;
    async fn downloads_by_hashes_with_class(
        &self,
        info_hashes: &[String],
        _class: RequestClass,
    ) -> Result<Vec<ObservedDownload>> {
        self.downloads_by_hashes(info_hashes).await
    }
    async fn tracker_statuses_with_class(
        &self,
        _info_hash: &str,
        _class: RequestClass,
    ) -> Result<Vec<DownloadTrackerStatus>> {
        Ok(Vec::new())
    }
    async fn files(&self, info_hash: &str) -> Result<Vec<DownloadFile>>;
    async fn delete_torrent(&self, info_hash: &str, delete_files: bool) -> Result<()>;
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
    governor: Option<ProviderGovernor>,
    provider_id: String,
}

impl QbittorrentClient {
    pub fn new(name: String, config: &DownloadClientConfig) -> Result<Self> {
        let api_key = read_secret(&config.api_key_file)?;
        if !api_key.starts_with("qbt_") || api_key.chars().count() != 32 {
            bail!("qBittorrent API key must be a 32-character qbt_ key");
        }
        let provider_id = format!("qbittorrent:{name}");
        Ok(Self {
            name,
            base_url: config.base_url.trim_end_matches('/').into(),
            api_key,
            client: Client::builder().timeout(Duration::from_secs(30)).build()?,
            governor: None,
            provider_id,
        })
    }

    pub fn governed(
        name: String,
        config: &DownloadClientConfig,
        governor: ProviderGovernor,
    ) -> Result<Self> {
        let mut client = Self::new(name, config)?;
        client.governor = Some(governor);
        Ok(client)
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
        class: RequestClass,
    ) -> Result<Vec<ObservedDownload>> {
        let torrents: Vec<QbitTorrent> = self
            .execute(class, || async {
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
                let response = request.send().await.map_err(transient)?;
                let response = successful(response, "qBittorrent list").await?;
                response.json().await.map_err(transient)
            })
            .await?;
        Ok(torrents
            .into_iter()
            .map(|torrent| torrent.normalized(&self.name))
            .collect())
    }

    async fn execute<T, F, Fut>(&self, class: RequestClass, operation: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<T, ProviderFailure>>,
    {
        match &self.governor {
            Some(governor) => governor
                .execute(&self.provider_id, class, operation)
                .await
                .map_err(Into::into),
            None => operation()
                .await
                .map_err(|failure| anyhow::anyhow!(failure.message)),
        }
    }
}

fn transient(error: impl ToString) -> ProviderFailure {
    ProviderFailure::new(ProviderFailureKind::Transient, error)
}

async fn successful(
    response: reqwest::Response,
    operation: &str,
) -> std::result::Result<reqwest::Response, ProviderFailure> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retry = retry_after(&response);
    let kind = if status == StatusCode::TOO_MANY_REQUESTS {
        ProviderFailureKind::RateLimited
    } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        ProviderFailureKind::Authentication
    } else if status.is_server_error() {
        ProviderFailureKind::Transient
    } else {
        ProviderFailureKind::Permanent
    };
    Err(
        ProviderFailure::new(kind, format!("{operation} returned HTTP {status}"))
            .retry_after(retry),
    )
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
    content_path: String,
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

#[derive(Debug, Deserialize)]
struct SyncMainData {
    #[serde(default)]
    server_state: QbitServerState,
}

#[derive(Debug, Default, Deserialize)]
struct QbitServerState {
    #[serde(default)]
    free_space_on_disk: i64,
}

#[derive(Debug, Deserialize)]
struct QbitFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    progress: f64,
}

#[derive(Debug, Deserialize)]
struct QbitTrackerStatus {
    #[serde(default)]
    url: String,
    #[serde(default)]
    status: i64,
    #[serde(default, rename = "msg")]
    message: String,
}

impl QbitTorrent {
    fn normalized(self, client: &str) -> ObservedDownload {
        ObservedDownload {
            name: self.name,
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
                content_path: (!self.content_path.trim().is_empty()).then_some(self.content_path),
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
    async fn free_space(&self) -> Result<i64> {
        self.execute(RequestClass::Manual, || async {
            let response = self
                .request(reqwest::Method::GET, "/api/v2/sync/maindata")
                .query(&[("rid", 0)])
                .send()
                .await
                .map_err(transient)?;
            let response = successful(response, "qBittorrent main data").await?;
            response
                .json::<SyncMainData>()
                .await
                .map(|value| value.server_state.free_space_on_disk)
                .map_err(transient)
        })
        .await
    }

    async fn downloads(&self, limit: u32, offset: u32) -> Result<Vec<ObservedDownload>> {
        self.downloads_with_class(limit, offset, RequestClass::Interactive)
            .await
    }

    async fn downloads_with_class(
        &self,
        limit: u32,
        offset: u32,
        class: RequestClass,
    ) -> Result<Vec<ObservedDownload>> {
        self.fetch_downloads(None, Some(limit), Some(offset), class)
            .await
    }

    async fn download(&self, info_hash: &str) -> Result<Option<ObservedDownload>> {
        self.download_with_class(info_hash, RequestClass::Interactive)
            .await
    }

    async fn download_with_class(
        &self,
        info_hash: &str,
        class: RequestClass,
    ) -> Result<Option<ObservedDownload>> {
        Ok(self
            .fetch_downloads(Some(info_hash), None, None, class)
            .await?
            .into_iter()
            .next())
    }

    async fn downloads_by_hashes(&self, info_hashes: &[String]) -> Result<Vec<ObservedDownload>> {
        self.downloads_by_hashes_with_class(info_hashes, RequestClass::Background)
            .await
    }

    async fn downloads_by_hashes_with_class(
        &self,
        info_hashes: &[String],
        class: RequestClass,
    ) -> Result<Vec<ObservedDownload>> {
        if info_hashes.is_empty() {
            return Ok(Vec::new());
        }
        self.fetch_downloads(Some(&info_hashes.join("|")), None, None, class)
            .await
    }

    async fn tracker_statuses_with_class(
        &self,
        info_hash: &str,
        class: RequestClass,
    ) -> Result<Vec<DownloadTrackerStatus>> {
        self.execute(class, || async {
            let response = self
                .request(reqwest::Method::GET, "/api/v2/torrents/trackers")
                .query(&[("hash", info_hash)])
                .send()
                .await
                .map_err(transient)?;
            let response = successful(response, "qBittorrent tracker status").await?;
            response
                .json::<Vec<QbitTrackerStatus>>()
                .await
                .map(|statuses| {
                    statuses
                        .into_iter()
                        .map(|status| DownloadTrackerStatus {
                            announce_host: announce_host(&status.url),
                            status: status.status,
                            message: (!status.message.trim().is_empty())
                                .then(|| status.message.trim().to_owned()),
                        })
                        .collect()
                })
                .map_err(transient)
        })
        .await
    }

    async fn files(&self, info_hash: &str) -> Result<Vec<DownloadFile>> {
        self.execute(RequestClass::Interactive, || async {
            let response = self
                .request(reqwest::Method::GET, "/api/v2/torrents/files")
                .query(&[("hash", info_hash)])
                .send()
                .await
                .map_err(transient)?;
            let response = successful(response, "qBittorrent file list").await?;
            response
                .json::<Vec<QbitFile>>()
                .await
                .map(|files| {
                    files
                        .into_iter()
                        .map(|file| DownloadFile {
                            name: file.name,
                            size: file.size,
                            progress: file.progress,
                        })
                        .collect()
                })
                .map_err(transient)
        })
        .await
    }

    async fn delete_torrent(&self, info_hash: &str, delete_files: bool) -> Result<()> {
        let info_hash = info_hash.to_ascii_lowercase();
        self.execute(RequestClass::Manual, || async {
            let response = self
                .request(reqwest::Method::POST, "/api/v2/torrents/delete")
                .form(&[
                    ("hashes", info_hash.as_str()),
                    ("deleteFiles", if delete_files { "true" } else { "false" }),
                ])
                .send()
                .await
                .map_err(transient)?;
            successful(response, "qBittorrent delete").await?;
            Ok(())
        })
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
            .execute(RequestClass::Download, || async {
                let response = self
                    .request(reqwest::Method::POST, "/api/v2/torrents/add")
                    .multipart(form)
                    .send()
                    .await
                    .map_err(transient)?;
                successful(response, "qBittorrent add").await
            })
            .await
            .context("submit torrent to qBittorrent")?;
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
        matchers::{body_string, header, method, path, query_param},
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
                    "content_path": "/downloads/ops/A tracker download",
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
        assert_eq!(download.name, "A tracker download");
        assert_eq!(download.live.info_hash, info_hash);
        assert_eq!(download.live.state, ClientDownloadState::Downloading);
        assert_eq!(download.live.downloaded, 1024);
        assert_eq!(
            download.live.content_path.as_deref(),
            Some("/downloads/ops/A tracker download")
        );
        assert!(download.live.added_at.is_some());
        assert_eq!(download.announce_host.as_deref(), Some("tracker.invalid"));
        let public_json = serde_json::to_string(&download.live).expect("serialize live status");
        assert!(!public_json.contains("tracker.invalid"));
        assert!(!public_json.contains("A tracker download"));
        assert!(!public_json.contains("\"tags\""));
    }

    #[tokio::test]
    async fn reads_torrent_files_without_mutating_the_client() {
        let server = MockServer::start().await;
        let info_hash = "abcdef0123456789abcdef0123456789abcdef01";
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/files"))
            .and(query_param("hash", info_hash))
            .and(header(
                "authorization",
                "Bearer qbt_0123456789012345678901234567",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "name": "Album/01 Track.flac",
                    "size": 2048,
                    "progress": 1.0
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let files = test_client(&server)
            .files(info_hash)
            .await
            .expect("qBittorrent file response");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "Album/01 Track.flac");
        assert_eq!(files[0].size, 2048);
        assert_eq!(files[0].progress, 1.0);
    }

    #[tokio::test]
    async fn deletes_only_the_explicit_torrent_and_payload_policy() {
        let server = MockServer::start().await;
        let info_hash = "abcdef0123456789abcdef0123456789abcdef01";
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/delete"))
            .and(header(
                "authorization",
                "Bearer qbt_0123456789012345678901234567",
            ))
            .and(body_string(format!("hashes={info_hash}&deleteFiles=true")))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        test_client(&server)
            .delete_torrent(&info_hash.to_uppercase(), true)
            .await
            .expect("guarded qBittorrent delete");
    }

    #[tokio::test]
    async fn reads_sanitized_tracker_status_for_a_torrent() {
        let server = MockServer::start().await;
        let info_hash = "abcdef0123456789abcdef0123456789abcdef01";
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/trackers"))
            .and(query_param("hash", info_hash))
            .and(header(
                "authorization",
                "Bearer qbt_0123456789012345678901234567",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "url": "https://secret-passkey@home.opsfet.ch/announce",
                    "status": 4,
                    "tier": 0,
                    "msg": "Unregistered torrent"
                }, {
                    "url": "** [DHT] **",
                    "status": 0,
                    "msg": ""
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let statuses = test_client(&server)
            .tracker_statuses_with_class(info_hash, crate::provider::RequestClass::Background)
            .await
            .expect("qBittorrent tracker response");
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].announce_host.as_deref(), Some("home.opsfet.ch"));
        assert_eq!(statuses[0].status, 4);
        assert_eq!(statuses[0].message.as_deref(), Some("Unregistered torrent"));
        assert_eq!(statuses[1].announce_host, None);
        assert!(format!("{statuses:?}").contains("home.opsfet.ch"));
        assert!(!format!("{statuses:?}").contains("secret-passkey"));
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
    async fn reads_free_space_for_pack_planning() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/sync/maindata"))
            .and(query_param("rid", "0"))
            .and(header(
                "authorization",
                "Bearer qbt_0123456789012345678901234567",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "server_state": {
                    "free_space_on_disk": 987654321
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            test_client(&server).free_space().await.expect("free space"),
            987654321
        );
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
