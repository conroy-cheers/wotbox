use std::{collections::HashMap, io::Write, sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{Duration as ChronoDuration, Utc};
use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::time::MissedTickBehavior;
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::{
    config::{Config, DownloadClientKind},
    db::{Cached, Database},
    model::{
        Account, ApiEnvelope, ClientDownload, ClientDownloadState, CreateDownload, DownloadJob,
        DownloadProfile, DownloadState, Provenance, PublicConfig, SearchPage, TorrentMetadata,
    },
    qbittorrent::{DownloadClient, QbittorrentClient},
    tracker::{GazelleTrackerClient, SearchRequest, TrackerClient, search_cache_key},
};

static UI: Dir<'_> = include_dir!("$OUT_DIR/ui");

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub base_path: String,
    pub trackers: HashMap<String, Arc<dyn TrackerClient>>,
    pub download_clients: HashMap<String, Arc<dyn DownloadClient>>,
    pub profiles: HashMap<String, DownloadProfile>,
}

impl AppState {
    pub async fn new(config: &Config) -> Result<Arc<Self>> {
        let db = Database::open(&config.database_path).await?;
        let mut trackers: HashMap<String, Arc<dyn TrackerClient>> = HashMap::new();
        for (name, tracker) in &config.trackers {
            trackers.insert(
                name.clone(),
                Arc::new(GazelleTrackerClient::new(name.clone(), tracker)?),
            );
        }
        let mut download_clients: HashMap<String, Arc<dyn DownloadClient>> = HashMap::new();
        for (name, client) in &config.download_clients {
            let client: Arc<dyn DownloadClient> = match client.kind {
                DownloadClientKind::Qbittorrent => {
                    Arc::new(QbittorrentClient::new(name.clone(), client)?)
                }
            };
            download_clients.insert(name.clone(), client);
        }
        let profiles = config
            .download_profiles
            .iter()
            .map(|(name, profile)| {
                (
                    name.clone(),
                    DownloadProfile {
                        name: name.clone(),
                        client: profile.client.clone(),
                        save_path: profile.save_path.clone(),
                        tag: profile.tag.clone(),
                        start_paused: profile.start_paused,
                    },
                )
            })
            .collect();
        Ok(Arc::new(Self {
            db,
            base_path: config.base_path.clone(),
            trackers,
            download_clients,
            profiles,
        }))
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/openapi.json", get(openapi))
        .route("/api/v1/config", get(public_config))
        .route("/api/v1/account", get(account))
        .route("/api/v1/search", get(search))
        .route("/api/v1/groups/{tracker}/{id}", get(group))
        .route("/api/v1/torrents/{tracker}/{id}", get(torrent))
        .route("/api/v1/download-profiles", get(download_profiles))
        .route("/api/v1/downloads", get(downloads).post(create_download))
        .route("/api/v1/downloads/{client}/{info_hash}", get(download))
        .fallback(get(ui))
        .with_state(state)
}

#[derive(Debug, Serialize, ToSchema)]
struct Health {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    qbittorrent: Option<String>,
}

#[utoipa::path(get, path = "/health/live", responses((status = 200, body = Health)))]
async fn live() -> Json<Health> {
    Json(Health {
        status: "ok",
        qbittorrent: None,
    })
}

#[utoipa::path(get, path = "/health/ready", responses((status = 200, body = Health)))]
async fn ready(State(state): State<Arc<AppState>>) -> Result<Json<Health>, AppError> {
    let client = state.download_clients.values().next().ok_or_else(|| {
        AppError::unavailable(
            "download_client_unconfigured",
            "No download client is configured",
        )
    })?;
    let version = client
        .health()
        .await
        .map_err(|error| AppError::unavailable("qbittorrent_unavailable", error))?;
    Ok(Json(Health {
        status: "ok",
        qbittorrent: Some(version),
    }))
}

async fn openapi() -> Json<Value> {
    Json(serde_json::to_value(ApiDoc::openapi()).expect("serialize OpenAPI"))
}

async fn public_config(State(state): State<Arc<AppState>>) -> Json<PublicConfig> {
    let mut trackers: Vec<_> = state.trackers.keys().cloned().collect();
    trackers.sort();
    let mut download_profiles: Vec<_> = state.profiles.keys().cloned().collect();
    download_profiles.sort();
    Json(PublicConfig {
        base_path: state.base_path.clone(),
        trackers,
        download_profiles,
    })
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
struct TrackerQuery {
    tracker: Option<String>,
    #[serde(default)]
    refresh: bool,
}

#[utoipa::path(
    get, path = "/api/v1/account", params(TrackerQuery),
    responses((status = 200, body = inline(ApiEnvelope<Account>)))
)]
async fn account(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TrackerQuery>,
) -> Result<Json<ApiEnvelope<Account>>, AppError> {
    let (tracker_name, tracker) = get_tracker(&state, query.tracker.as_deref())?;
    let key = "current";
    if !query.refresh
        && let Some(cached) = state.db.get_snapshot(tracker_name, "account", key).await?
        && cached.expires_at > Utc::now()
    {
        return Ok(Json(envelope(tracker_name, cached, false)));
    }
    match tracker.account().await {
        Ok((value, raw)) => {
            let cached = store(&state.db, tracker_name, "account", key, value, raw, 60).await?;
            Ok(Json(envelope(tracker_name, cached, false)))
        }
        Err(error) => stale_or_error(&state.db, tracker_name, "account", key, error).await,
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
struct SearchQuery {
    tracker: Option<String>,
    query: Option<String>,
    artist: Option<String>,
    release_type: Option<String>,
    year: Option<i64>,
    format: Option<String>,
    encoding: Option<String>,
    media: Option<String>,
    page: Option<i64>,
    #[serde(default)]
    refresh: bool,
}

#[utoipa::path(
    get, path = "/api/v1/search", params(SearchQuery),
    responses((status = 200, body = inline(ApiEnvelope<SearchPage>)))
)]
async fn search(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ApiEnvelope<SearchPage>>, AppError> {
    let (tracker_name, tracker) = get_tracker(&state, query.tracker.as_deref())?;
    let request = SearchRequest {
        query: query.query,
        artist: query.artist,
        release_type: query.release_type,
        year: query.year,
        format: query.format,
        encoding: query.encoding,
        media: query.media,
        page: query.page,
    };
    let key = search_cache_key(&request);
    if !query.refresh
        && let Some(cached) = state.db.get_snapshot(tracker_name, "search", &key).await?
        && cached.expires_at > Utc::now()
    {
        return Ok(Json(envelope(tracker_name, cached, false)));
    }
    match tracker.search(&request).await {
        Ok((value, raw)) => {
            let cached = store(&state.db, tracker_name, "search", &key, value, raw, 300).await?;
            Ok(Json(envelope(tracker_name, cached, false)))
        }
        Err(error) => stale_or_error(&state.db, tracker_name, "search", &key, error).await,
    }
}

async fn group(
    State(state): State<Arc<AppState>>,
    Path((tracker_name, id)): Path<(String, i64)>,
    Query(query): Query<RefreshQuery>,
) -> Result<Json<ApiEnvelope<Value>>, AppError> {
    let (_, tracker) = get_tracker(&state, Some(&tracker_name))?;
    let key = id.to_string();
    if !query.refresh
        && let Some(cached) = state.db.get_snapshot(&tracker_name, "group", &key).await?
        && cached.expires_at > Utc::now()
    {
        return Ok(Json(envelope(&tracker_name, cached, false)));
    }
    match tracker.group(id).await {
        Ok(value) => {
            let cached = store(
                &state.db,
                &tracker_name,
                "group",
                &key,
                value.clone(),
                value,
                900,
            )
            .await?;
            Ok(Json(envelope(&tracker_name, cached, false)))
        }
        Err(error) => stale_or_error(&state.db, &tracker_name, "group", &key, error).await,
    }
}

async fn torrent(
    State(state): State<Arc<AppState>>,
    Path((tracker_name, id)): Path<(String, i64)>,
    Query(query): Query<RefreshQuery>,
) -> Result<Json<ApiEnvelope<TorrentMetadata>>, AppError> {
    let (_, tracker) = get_tracker(&state, Some(&tracker_name))?;
    let key = id.to_string();
    if !query.refresh
        && let Some(cached) = state
            .db
            .get_snapshot(&tracker_name, "torrent", &key)
            .await?
        && cached.expires_at > Utc::now()
    {
        return Ok(Json(envelope(&tracker_name, cached, false)));
    }
    match tracker.torrent(id).await {
        Ok((value, raw)) => {
            let cached = store(&state.db, &tracker_name, "torrent", &key, value, raw, 900).await?;
            Ok(Json(envelope(&tracker_name, cached, false)))
        }
        Err(error) => stale_or_error(&state.db, &tracker_name, "torrent", &key, error).await,
    }
}

#[derive(Debug, Deserialize, IntoParams)]
struct RefreshQuery {
    #[serde(default)]
    refresh: bool,
}

async fn download_profiles(State(state): State<Arc<AppState>>) -> Json<Vec<DownloadProfile>> {
    let mut profiles: Vec<_> = state.profiles.values().cloned().collect();
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    Json(profiles)
}

#[derive(Debug, Deserialize, IntoParams)]
struct DownloadsQuery {
    client: Option<String>,
    #[serde(default = "default_download_limit")]
    limit: u32,
    #[serde(default)]
    offset: u32,
}

fn default_download_limit() -> u32 {
    100
}

#[utoipa::path(
    get, path = "/api/v1/downloads", params(DownloadsQuery),
    responses((status = 200, body = inline(Vec<ClientDownload>)))
)]
async fn downloads(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DownloadsQuery>,
) -> Result<Json<Vec<ClientDownload>>, AppError> {
    let limit = query.limit.clamp(1, 500);
    let offset = query.offset.min(10_000);
    let clients: Vec<_> = if let Some(name) = query.client {
        vec![(
            name.clone(),
            state
                .download_clients
                .get(&name)
                .ok_or_else(|| {
                    AppError::bad_request("unknown_download_client", "Unknown download client")
                })?
                .clone(),
        )]
    } else {
        state
            .download_clients
            .iter()
            .map(|(name, client)| (name.clone(), client.clone()))
            .collect()
    };
    let mut downloads = Vec::new();
    for (name, client) in clients {
        let mut client_downloads = client
            .downloads(limit.saturating_add(offset), 0)
            .await
            .map_err(|error| {
                AppError::unavailable("download_client_unavailable", format!("{name}: {error}"))
            })?;
        downloads.append(&mut client_downloads);
    }
    downloads.sort_by(|left, right| {
        right
            .added_at
            .cmp(&left.added_at)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(Json(
        downloads
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect(),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/downloads/{client}/{info_hash}",
    params(
        ("client" = String, Path, description = "Configured download client name"),
        ("info_hash" = String, Path, description = "Torrent info hash")
    ),
    responses(
        (status = 200, body = ClientDownload),
        (status = 404, description = "Torrent is not present in the download client")
    )
)]
async fn download(
    State(state): State<Arc<AppState>>,
    Path((client_name, info_hash)): Path<(String, String)>,
) -> Result<Json<ClientDownload>, AppError> {
    let client = state.download_clients.get(&client_name).ok_or_else(|| {
        AppError::bad_request("unknown_download_client", "Unknown download client")
    })?;
    client
        .download(&info_hash)
        .await
        .map_err(|error| AppError::unavailable("download_client_unavailable", error))?
        .map(Json)
        .ok_or_else(|| {
            AppError::not_found(
                "download_not_found",
                "Torrent is not present in the download client",
            )
        })
}

#[utoipa::path(
    post, path = "/api/v1/downloads", request_body = CreateDownload,
    responses((status = 202, body = DownloadJob))
)]
async fn create_download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateDownload>,
) -> Result<(StatusCode, Json<DownloadJob>), AppError> {
    get_tracker(&state, Some(&request.tracker))?;
    if !state.profiles.contains_key(&request.profile) {
        return Err(AppError::bad_request(
            "unknown_download_profile",
            "Unknown download profile",
        ));
    }
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok());
    let (job, created) = state
        .db
        .create_job(
            &request.tracker,
            request.torrent_id,
            &request.profile,
            request.use_token,
            idempotency_key,
        )
        .await?;
    if created {
        let task_state = state.clone();
        let task_job = job.clone();
        tokio::spawn(async move {
            if let Err(error) = process_download(task_state.clone(), task_job.clone()).await {
                let message = truncate(&error.to_string(), 500);
                tracing::error!(job_id = %task_job.id, error = %message, "download submission failed");
                let _ = task_state
                    .db
                    .set_job_state(
                        task_job.id,
                        DownloadState::Failed,
                        Some(("download_failed", &message)),
                    )
                    .await;
            }
        });
    }
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn process_download(state: Arc<AppState>, job: DownloadJob) -> Result<()> {
    state
        .db
        .set_job_state(job.id, DownloadState::FetchingMetadata, None)
        .await?;
    let tracker = state
        .trackers
        .get(&job.tracker)
        .ok_or_else(|| anyhow!("tracker disappeared from configuration"))?;
    let profile = state
        .profiles
        .get(&job.profile)
        .ok_or_else(|| anyhow!("download profile disappeared from configuration"))?;
    let client = state
        .download_clients
        .get(&profile.client)
        .ok_or_else(|| anyhow!("download client disappeared from configuration"))?;

    let (metadata, raw) = tracker.torrent(job.torrent_id).await?;
    if job.use_token && !metadata.can_use_token {
        return Err(anyhow!("torrent is not eligible for a freeleech token"));
    }
    state
        .db
        .put_snapshot(
            &job.tracker,
            "torrent",
            &job.torrent_id.to_string(),
            &metadata,
            &raw,
            Utc::now(),
            Utc::now() + ChronoDuration::minutes(15),
        )
        .await?;
    state
        .db
        .update_job_metadata(
            job.id,
            metadata.group_id,
            &metadata.info_hash,
            &metadata.name,
        )
        .await?;

    if let Some(existing) = client.download(&metadata.info_hash).await? {
        let state_value = if existing.progress >= 1.0 {
            DownloadState::Complete
        } else {
            DownloadState::Active
        };
        state
            .db
            .update_progress(
                job.id,
                state_value,
                existing.progress,
                existing.download_speed,
                existing.upload_speed,
                existing.eta,
            )
            .await?;
        return Ok(());
    }

    state
        .db
        .set_job_state(job.id, DownloadState::Submitting, None)
        .await?;
    let bytes = tracker
        .download_torrent(job.torrent_id, job.use_token)
        .await?;
    let mut temporary = tempfile::NamedTempFile::new()?;
    temporary.write_all(&bytes)?;
    temporary.flush()?;
    let payload = tokio::fs::read(temporary.path()).await?;
    let file_name = format!("{}-{}.torrent", job.tracker, job.torrent_id);
    client.add_torrent(payload, &file_name, profile).await?;
    state
        .db
        .set_job_state(job.id, DownloadState::Active, None)
        .await?;
    Ok(())
}

pub fn spawn_reconciler(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let Ok(jobs) = state.db.list_jobs().await else {
                continue;
            };
            for job in jobs.into_iter().filter(|job| {
                matches!(job.state, DownloadState::Active | DownloadState::Submitting)
            }) {
                let (Some(info_hash), Some(profile)) =
                    (job.info_hash.as_deref(), state.profiles.get(&job.profile))
                else {
                    continue;
                };
                let Some(client) = state.download_clients.get(&profile.client) else {
                    continue;
                };
                match client.download(info_hash).await {
                    Ok(Some(status)) => {
                        let next = if status.progress >= 1.0 {
                            DownloadState::Complete
                        } else {
                            DownloadState::Active
                        };
                        let _ = state
                            .db
                            .update_progress(
                                job.id,
                                next,
                                status.progress,
                                status.download_speed,
                                status.upload_speed,
                                status.eta,
                            )
                            .await;
                    }
                    Ok(None) => {
                        let _ = state
                            .db
                            .set_job_state(
                                job.id,
                                DownloadState::Unknown,
                                Some((
                                    "torrent_missing",
                                    "Torrent is no longer present in qBittorrent",
                                )),
                            )
                            .await;
                    }
                    Err(error) => {
                        tracing::warn!(job_id = %job.id, %error, "qBittorrent reconciliation failed")
                    }
                }
            }
        }
    });
}

async fn store<T: Serialize + DeserializeOwned>(
    db: &Database,
    tracker: &str,
    kind: &str,
    key: &str,
    value: T,
    raw: Value,
    ttl_seconds: i64,
) -> Result<Cached<T>, AppError> {
    let fetched_at = Utc::now();
    let expires_at = fetched_at + ChronoDuration::seconds(ttl_seconds);
    db.put_snapshot(tracker, kind, key, &value, &raw, fetched_at, expires_at)
        .await?;
    Ok(Cached {
        value,
        fetched_at,
        expires_at,
    })
}

async fn stale_or_error<T: Serialize + DeserializeOwned>(
    db: &Database,
    tracker: &str,
    kind: &str,
    key: &str,
    error: anyhow::Error,
) -> Result<Json<ApiEnvelope<T>>, AppError> {
    if let Some(cached) = db.get_snapshot(tracker, kind, key).await? {
        tracing::warn!(%tracker, resource_kind = kind, %error, "serving stale tracker snapshot");
        return Ok(Json(envelope(tracker, cached, true)));
    }
    Err(AppError::unavailable("tracker_unavailable", error))
}

fn envelope<T>(tracker: &str, cached: Cached<T>, stale: bool) -> ApiEnvelope<T> {
    ApiEnvelope {
        data: cached.value,
        provenance: Provenance {
            tracker: tracker.to_owned(),
            fetched_at: cached.fetched_at,
            cache_age_seconds: (Utc::now() - cached.fetched_at).num_seconds().max(0),
            stale,
        },
    }
}

fn get_tracker<'a>(
    state: &'a AppState,
    requested: Option<&str>,
) -> Result<(&'a str, &'a Arc<dyn TrackerClient>), AppError> {
    let name = requested
        .or_else(|| state.trackers.keys().next().map(String::as_str))
        .ok_or_else(|| AppError::unavailable("tracker_unconfigured", "No tracker is configured"))?;
    let tracker = state
        .trackers
        .get(name)
        .ok_or_else(|| AppError::bad_request("unknown_tracker", "Unknown tracker"))?;
    Ok((tracker.name(), tracker))
}

async fn ui(State(state): State<Arc<AppState>>, uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") || path.starts_with("health/") {
        return AppError::not_found("route_not_found", "Route not found").into_response();
    }
    let asset = UI.get_file(path).or_else(|| UI.get_file("index.html"));
    let Some(asset) = asset else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let is_index = asset.path() == std::path::Path::new("index.html");
    let body = if is_index {
        let index = asset.contents_utf8().unwrap_or_default();
        let base = if state.base_path == "/" {
            "/".to_owned()
        } else {
            format!("{}/", state.base_path)
        };
        let injection = format!(
            r#"<base href="{base}"><script>window.__WOTBOX_CONFIG__={};</script></head>"#,
            json!({ "basePath": state.base_path }),
        );
        Body::from(index.replace("</head>", &injection))
    } else {
        Body::from(asset.contents().to_vec())
    };
    let mime = mime_guess::from_path(asset.path()).first_or_octet_stream();
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if is_index {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    } else {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    response
}

#[derive(Debug, Serialize, ToSchema)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct ErrorDetail {
    code: &'static str,
    message: String,
    retryable: bool,
}

pub struct AppError {
    status: StatusCode,
    body: ErrorBody,
}

impl AppError {
    fn bad_request(code: &'static str, message: impl ToString) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message, false)
    }

    fn not_found(code: &'static str, message: impl ToString) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message, false)
    }

    fn unavailable(code: &'static str, message: impl ToString) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message, true)
    }

    fn new(
        status: StatusCode,
        code: &'static str,
        message: impl ToString,
        retryable: bool,
    ) -> Self {
        Self {
            status,
            body: ErrorBody {
                error: ErrorDetail {
                    code,
                    message: truncate(&message.to_string(), 500),
                    retryable,
                },
            },
        }
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        let error = error.into();
        tracing::error!(%error, "internal request failure");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "An internal error occurred",
            false,
        )
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[derive(OpenApi)]
#[openapi(
    paths(live, ready, account, search, downloads, download, create_download),
    components(schemas(
        Health, Account, Provenance, SearchPage, TorrentMetadata, DownloadProfile,
        ClientDownload, ClientDownloadState, CreateDownload, DownloadJob, DownloadState,
        PublicConfig, ErrorBody, ErrorDetail
    )),
    tags((name = "wotbox", description = "Wotbox API"))
)]
struct ApiDoc;
