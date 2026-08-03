use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{File, OpenOptions},
    io::Write,
    path::{Path as StdPath, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
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
use tokio::{sync::RwLock, time::MissedTickBehavior};
use utoipa::{IntoParams, OpenApi, ToSchema};
use uuid::Uuid;

use crate::{
    background::{self, BackgroundJobNotifier},
    channel,
    config::{Config, DownloadClientKind, TrackerKind, read_secret},
    db::{Cached, CreateReplacementImport, Database, DownloadObservation},
    dedupe::track_index_from_group,
    model::{
        Account, ApiEnvelope, ArtistCatalogPage, ArtistCatalogRelease, ArtistCatalogRole,
        ArtistCredit, ArtistCreditSource, ArtistRole, AttachChannelPackItem,
        BackgroundJobsOverview, CanonicalDownload, CanonicalTorrent, ChannelBatchResult,
        ChannelConfig, ChannelKind, ChannelOverview, ChannelPack, ChannelPackDecision,
        ChannelPackSummary, ChannelRun, ChannelRunStatus, ChannelRunTrigger, ClientDownloadState,
        CreateDownload, DecideChannelPack, DeduplicationIndexStatus, DownloadJob, DownloadProfile,
        DownloadState, DownloadsPage, ImportTaskState, ImportsPage, LibraryArtistPage,
        LibraryArtistSummary, LibraryArtistsPage, LibraryAvailability, LibraryCopy,
        LibraryIndexStatus, LibraryRelease, LibraryVariantState, LiveDownloadStatus,
        PlexIntegrationStatus, PlexScanQueued, Provenance, ProviderStatus, PublicConfig,
        ReleaseDetail, ReleaseSummary, RuntimePreferences, SearchPage, SnapshotState,
        SourceProvenance, TorrentMetadata, TorrentVariant, value_i64,
    },
    plex::PlexIntegration,
    provider::{
        ProviderDefinition, ProviderGovernor, ProviderRequestError, RequestClass,
        is_provider_unavailable,
    },
    qbittorrent::{DownloadClient, QbittorrentClient},
    tracker::{
        GazelleTrackerClient, SearchRequest, TrackerClient, fallback_artist_credit,
        search_cache_key, torrent_info_hash,
    },
};

static UI: Dir<'_> = include_dir!("$OUT_DIR/ui");

struct LibraryCache {
    loaded_at: std::time::Instant,
    releases: Vec<LibraryRelease>,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub base_path: String,
    pub trackers: HashMap<String, Arc<dyn TrackerClient>>,
    pub tracker_sites: BTreeMap<String, String>,
    pub download_clients: HashMap<String, Arc<dyn DownloadClient>>,
    pub profiles: HashMap<String, DownloadProfile>,
    pub announce_hosts: HashMap<String, String>,
    pub source_client: reqwest::Client,
    pub providers: ProviderGovernor,
    pub lastfm_api_key: Option<String>,
    pub plex: Option<PlexIntegration>,
    pub background_jobs: BackgroundJobNotifier,
    download_staging_dir: PathBuf,
    library_cache: Arc<RwLock<Option<LibraryCache>>>,
    _instance_lock: Arc<File>,
}

impl AppState {
    pub async fn new(config: &Config) -> Result<Arc<Self>> {
        let instance_lock = acquire_database_lock(&config.database_path)?;
        let download_staging_dir = config
            .database_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| StdPath::new("."))
            .join("download-staging");
        std::fs::create_dir_all(&download_staging_dir).with_context(|| {
            format!(
                "create download staging directory {}",
                download_staging_dir.display()
            )
        })?;
        let db = Database::open(&config.database_path).await?;
        let preferences = db.get_runtime_preferences().await?;
        let mut definitions = config
            .trackers
            .keys()
            .map(|name| ProviderDefinition::tracker(name))
            .collect::<Vec<_>>();
        definitions.extend(
            config
                .download_clients
                .keys()
                .map(|name| ProviderDefinition::qbittorrent(name)),
        );
        definitions.push(ProviderDefinition::lastfm());
        definitions.push(ProviderDefinition::apple());
        if config.plex.is_some() {
            definitions.push(ProviderDefinition::plex());
        }
        let providers = ProviderGovernor::new(db.clone(), definitions, &preferences.api).await?;
        let mut trackers: HashMap<String, Arc<dyn TrackerClient>> = HashMap::new();
        let tracker_sites = config
            .trackers
            .iter()
            .map(|(name, tracker)| {
                (
                    name.to_ascii_lowercase(),
                    tracker.base_url.trim_end_matches('/').to_owned(),
                )
            })
            .collect();
        let mut announce_hosts = HashMap::new();
        for (name, tracker) in &config.trackers {
            trackers.insert(
                name.clone(),
                Arc::new(GazelleTrackerClient::governed(
                    name.clone(),
                    tracker,
                    providers.clone(),
                )?),
            );
            let hosts =
                if tracker.announce_hosts.is_empty() && matches!(tracker.kind, TrackerKind::Ops) {
                    vec!["home.opsfet.ch".to_owned()]
                } else {
                    tracker.announce_hosts.clone()
                };
            for host in hosts {
                announce_hosts.insert(
                    host.trim().trim_end_matches('.').to_ascii_lowercase(),
                    name.clone(),
                );
            }
        }
        let mut download_clients: HashMap<String, Arc<dyn DownloadClient>> = HashMap::new();
        for (name, client) in &config.download_clients {
            let client: Arc<dyn DownloadClient> = match client.kind {
                DownloadClientKind::Qbittorrent => Arc::new(QbittorrentClient::governed(
                    name.clone(),
                    client,
                    providers.clone(),
                )?),
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
        let state = Arc::new(Self {
            db,
            base_path: config.base_path.clone(),
            trackers,
            tracker_sites,
            download_clients,
            profiles,
            announce_hosts,
            source_client: reqwest::Client::builder()
                .user_agent(format!("wotbox/{}", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()?,
            providers,
            lastfm_api_key: config
                .lastfm_api_key_file
                .as_deref()
                .map(read_secret)
                .transpose()?,
            plex: config.plex.as_ref().map(PlexIntegration::new).transpose()?,
            background_jobs: BackgroundJobNotifier::new(),
            download_staging_dir,
            library_cache: Arc::new(RwLock::new(None)),
            _instance_lock: Arc::new(instance_lock),
        });
        state.db.ensure_default_channels().await?;
        state.db.recover_channel_runs().await?;
        state.db.recover_resolving_links().await?;
        state.db.recover_track_indexes().await?;
        state.db.sync_import_tasks().await?;
        seed_existing_job_links(&state).await?;
        cleanup_orphaned_download_stages(&state).await?;
        Ok(state)
    }
}

fn acquire_database_lock(path: &StdPath) -> Result<File> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create database directory {}", parent.display()))?;
    }
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock_path = std::path::PathBuf::from(lock_name);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open instance lock {}", lock_path.display()))?;
    file.try_lock()
        .with_context(|| format!("another Wotbox process is already using {}", path.display()))?;
    Ok(file)
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/openapi.json", get(openapi))
        .route("/api/v1/config", get(public_config))
        .route(
            "/api/v1/preferences",
            get(preferences).put(update_preferences),
        )
        .route("/api/v1/providers", get(providers))
        .route(
            "/api/v1/integrations/plex",
            get(plex_status).post(scan_plex),
        )
        .route("/api/v1/background-jobs", get(background_jobs))
        .route(
            "/api/v1/background-jobs/{id}/cancel",
            axum::routing::post(cancel_background_job),
        )
        .route(
            "/api/v1/background-jobs/{id}/retry",
            axum::routing::post(retry_background_job),
        )
        .route(
            "/api/v1/providers/{id}/pause",
            axum::routing::post(pause_provider),
        )
        .route(
            "/api/v1/providers/{id}/resume",
            axum::routing::post(resume_provider),
        )
        .route("/api/v1/account", get(account))
        .route("/api/v1/accounts", get(accounts))
        .route("/api/v1/search", get(search))
        .route(
            "/api/v1/releases/{id}",
            get(release).put(update_release_metadata),
        )
        .route(
            "/api/v1/releases/{id}/cross-seed-plans",
            get(cross_seed_plans),
        )
        .route(
            "/api/v1/releases/{id}/unlink-source",
            axum::routing::post(unlink_release_source),
        )
        .route("/api/v1/index/canonical", get(canonical_index))
        .route("/api/v1/matches", get(match_candidates))
        .route(
            "/api/v1/matches/{id}/accept",
            axum::routing::post(accept_match),
        )
        .route(
            "/api/v1/matches/{id}/reject",
            axum::routing::post(reject_match),
        )
        .route("/api/v1/torrents/{tracker}/{id}", get(torrent))
        .route(
            "/api/v1/artists/{id}",
            get(canonical_artist_catalog).put(update_artist_metadata),
        )
        .route(
            "/api/v1/artists/{tracker}/{id}/releases",
            get(artist_catalog),
        )
        .route("/api/v1/download-profiles", get(download_profiles))
        .route("/api/v1/library/artists", get(library_artists))
        .route("/api/v1/library/artists/{id}", get(library_artist))
        .route("/api/v1/downloads", get(downloads).post(create_download))
        .route("/api/v1/imports", get(imports))
        .route(
            "/api/v1/imports/{id}/retry",
            axum::routing::post(retry_import),
        )
        .route(
            "/api/v1/imports/{id}/dismiss",
            axum::routing::post(dismiss_import),
        )
        .route(
            "/api/v1/downloads/{client}/{info_hash}",
            get(download_detail_compatibility),
        )
        .route("/api/v1/download-jobs/{id}", get(download_job))
        .route("/api/v1/channels", get(channels))
        .route("/api/v1/channels/{id}", axum::routing::put(update_channel))
        .route(
            "/api/v1/channels/{id}/refresh",
            axum::routing::post(refresh_channel),
        )
        .route("/api/v1/channel-runs/{id}", get(channel_run))
        .route("/api/v1/channels/{id}/packs", get(channel_packs))
        .route("/api/v1/channel-packs/{id}", get(channel_pack))
        .route(
            "/api/v1/channel-packs/{id}/replan",
            axum::routing::post(replan_channel_pack),
        )
        .route(
            "/api/v1/channel-packs/{id}/items/{ordinal}/attach",
            axum::routing::post(attach_channel_pack_item),
        )
        .route(
            "/api/v1/channel-packs/{id}/accept",
            axum::routing::post(accept_channel_pack),
        )
        .route(
            "/api/v1/channel-packs/{id}/reject",
            axum::routing::post(reject_channel_pack),
        )
        .route(
            "/api/v1/downloads/{client}/{info_hash}/retry",
            axum::routing::post(retry_download_link),
        )
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

#[utoipa::path(get, path = "/api/v1/preferences", responses((status = 200, body = RuntimePreferences)))]
async fn preferences(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RuntimePreferences>, AppError> {
    Ok(Json(state.db.get_runtime_preferences().await?))
}

#[utoipa::path(put, path = "/api/v1/preferences", request_body = RuntimePreferences, responses((status = 200, body = RuntimePreferences)))]
async fn update_preferences(
    State(state): State<Arc<AppState>>,
    Json(mut preferences): Json<RuntimePreferences>,
) -> Result<Json<RuntimePreferences>, AppError> {
    preferences.release = preferences.release.migrate_legacy();
    preferences
        .release
        .validate()
        .map_err(|message| AppError::bad_request("invalid_preferences", message))?;
    for policy in &preferences.release.tracker_policies {
        if let Some(profile) = policy.download_profile.as_deref()
            && !state.profiles.contains_key(profile)
        {
            return Err(AppError::bad_request(
                "invalid_download_profile",
                format!(
                    "Download profile {profile} configured for {} does not exist",
                    policy.tracker
                ),
            ));
        }
    }
    state
        .providers
        .validate_preferences(&preferences.api)
        .map_err(|error| AppError::bad_request("invalid_provider_policy", error))?;
    state.db.put_runtime_preferences(&preferences).await?;
    state.providers.apply_preferences(&preferences.api).await?;
    Ok(Json(preferences))
}

#[utoipa::path(get, path = "/api/v1/providers", responses((status = 200, body = [ProviderStatus])))]
async fn providers(State(state): State<Arc<AppState>>) -> Json<Vec<ProviderStatus>> {
    Json(state.providers.statuses().await)
}

#[utoipa::path(get, path = "/api/v1/integrations/plex", responses((status = 200, body = PlexIntegrationStatus)))]
async fn plex_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<PlexIntegrationStatus>, AppError> {
    let pending_scans = state
        .db
        .active_background_jobs_by_kind(background::NOTIFY_PLEX)
        .await?;
    Ok(Json(match state.plex.as_ref() {
        Some(plex) => PlexIntegrationStatus {
            configured: true,
            section_id: Some(plex.section_id()),
            library_roots: plex.library_roots().to_vec(),
            pending_scans,
        },
        None => PlexIntegrationStatus {
            configured: false,
            section_id: None,
            library_roots: Vec::new(),
            pending_scans,
        },
    }))
}

#[utoipa::path(post, path = "/api/v1/integrations/plex", responses((status = 202, body = PlexScanQueued)))]
async fn scan_plex(
    State(state): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<PlexScanQueued>), AppError> {
    let plex = state
        .plex
        .as_ref()
        .ok_or_else(|| AppError::unavailable("plex_unconfigured", "Plex is not configured"))?;
    let mut job_ids = Vec::new();
    let detected_at = Utc::now();
    for target in plex.targets() {
        job_ids.push(state.db.enqueue_plex_scan(&target, detected_at).await?);
    }
    state.background_jobs.wake();
    Ok((StatusCode::ACCEPTED, Json(PlexScanQueued { job_ids })))
}

#[derive(Debug, Deserialize, IntoParams)]
struct BackgroundJobsQuery {
    #[serde(default = "default_background_job_limit")]
    limit: u64,
}

fn default_background_job_limit() -> u64 {
    100
}

#[utoipa::path(get, path = "/api/v1/background-jobs", params(BackgroundJobsQuery), responses((status = 200, body = BackgroundJobsOverview)))]
async fn background_jobs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BackgroundJobsQuery>,
) -> Result<Json<BackgroundJobsOverview>, AppError> {
    Ok(Json(
        state
            .db
            .background_jobs_overview(query.limit.clamp(1, 500))
            .await?,
    ))
}

#[utoipa::path(post, path = "/api/v1/background-jobs/{id}/cancel", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn cancel_background_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    if state.db.cancel_background_job(id).await? {
        state.background_jobs.wake();
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::conflict(
            "job_not_cancellable",
            "The background job was not found or has already finished",
        ))
    }
}

#[utoipa::path(post, path = "/api/v1/background-jobs/{id}/retry", params(("id" = Uuid, Path)), responses((status = 202)))]
async fn retry_background_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    if state.db.retry_failed_background_job(id).await? {
        state.background_jobs.wake();
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(AppError::conflict(
            "job_not_retryable",
            "The background job was not found or is not failed or cancelled",
        ))
    }
}

#[utoipa::path(post, path = "/api/v1/providers/{id}/pause", params(("id" = String, Path)), responses((status = 200, body = ProviderStatus)))]
async fn pause_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ProviderStatus>, AppError> {
    Ok(Json(state.providers.pause(&id).await.map_err(|error| {
        AppError::not_found("provider_not_found", error)
    })?))
}

#[utoipa::path(post, path = "/api/v1/providers/{id}/resume", params(("id" = String, Path)), responses((status = 200, body = ProviderStatus)))]
async fn resume_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ProviderStatus>, AppError> {
    let status = state
        .providers
        .resume(&id)
        .await
        .map_err(|error| AppError::not_found("provider_not_found", error))?;
    state.db.resume_waiting_jobs_for_provider(&id).await?;
    state.background_jobs.wake();
    Ok(Json(status))
}

#[utoipa::path(get, path = "/health/ready", responses((status = 200, body = Health)))]
async fn ready(State(state): State<Arc<AppState>>) -> Result<Json<Health>, AppError> {
    tokio::time::timeout(Duration::from_millis(500), state.db.ping())
        .await
        .map_err(|_| AppError::unavailable("database_timeout", "Database readiness timed out"))?
        .map_err(|error| AppError::unavailable("database_unavailable", error))?;
    Ok(Json(Health {
        status: "ok",
        qbittorrent: None,
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
        tracker_sites: state.tracker_sites.clone(),
        download_profiles,
    })
}

#[utoipa::path(get, path = "/api/v1/channels", responses((status = 200, body = [ChannelOverview])))]
async fn channels(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ChannelOverview>>, AppError> {
    let mut result = Vec::new();
    for mut config in state.db.list_channels().await? {
        hydrate_channel_config(&state, &mut config)?;
        let latest_pack = state
            .db
            .list_channel_packs(&config.id, 1, 0)
            .await?
            .into_iter()
            .next();
        result.push(ChannelOverview {
            active_run: state.db.active_channel_run(&config.id).await?,
            latest_pack,
            channel: config,
        });
    }
    Ok(Json(result))
}

#[utoipa::path(put, path = "/api/v1/channels/{id}", request_body = ChannelConfig, responses((status = 200, body = ChannelConfig)))]
async fn update_channel(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut requested): Json<ChannelConfig>,
) -> Result<Json<ChannelConfig>, AppError> {
    let existing = state
        .db
        .get_channel(&id)
        .await?
        .ok_or_else(|| AppError::not_found("channel_not_found", "Channel was not found"))?;
    if requested.id != id || requested.kind != existing.kind {
        return Err(AppError::bad_request(
            "channel_identity_immutable",
            "Channel id and kind cannot be changed",
        ));
    }
    requested.country_chart = match requested.kind {
        ChannelKind::CountryChart => requested.country_chart,
        ChannelKind::Lastfm | ChannelKind::TrumpedDownloads => None,
    };
    requested.lastfm = match requested.kind {
        ChannelKind::Lastfm => requested.lastfm,
        ChannelKind::CountryChart | ChannelKind::TrumpedDownloads => None,
    };
    requested.credential_configured =
        !matches!(requested.kind, ChannelKind::Lastfm) || state.lastfm_api_key.is_some();
    let lastfm_username_changed = requested.kind == ChannelKind::Lastfm
        && requested
            .lastfm
            .as_ref()
            .map(|settings| settings.username.trim())
            != existing
                .lastfm
                .as_ref()
                .map(|settings| settings.username.trim());
    requested.last_successful_at = existing.last_successful_at;
    requested.last_attempt_at = existing.last_attempt_at;
    requested.last_error = (!lastfm_username_changed)
        .then_some(existing.last_error)
        .flatten();
    requested.failure_count = if lastfm_username_changed {
        0
    } else {
        existing.failure_count
    };
    requested.next_refresh_at = None;
    requested.updated_at = Utc::now();
    channel::validate_channel(&requested, state.lastfm_api_key.is_some())
        .map_err(|error| AppError::bad_request("invalid_channel", error))?;
    state.db.put_channel(&requested).await?;
    hydrate_channel_config(&state, &mut requested)?;
    Ok(Json(requested))
}

#[utoipa::path(post, path = "/api/v1/channels/{id}/refresh", responses((status = 202, body = ChannelRun)))]
async fn refresh_channel(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<ChannelRun>), AppError> {
    let config = state
        .db
        .get_channel(&id)
        .await?
        .ok_or_else(|| AppError::not_found("channel_not_found", "Channel was not found"))?;
    channel::validate_channel_refresh(&config, state.lastfm_api_key.is_some())
        .map_err(|error| AppError::bad_request("invalid_channel", error))?;
    let run = start_channel_run(state, config, ChannelRunTrigger::Manual).await?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

#[utoipa::path(get, path = "/api/v1/channel-runs/{id}", responses((status = 200, body = ChannelRun)))]
async fn channel_run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<ChannelRun>, AppError> {
    state
        .db
        .get_channel_run(id)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found("channel_run_not_found", "Channel run was not found"))
}

#[derive(Debug, Deserialize, IntoParams)]
struct ChannelPacksQuery {
    #[serde(default = "default_channel_pack_limit")]
    limit: u64,
    #[serde(default)]
    offset: u64,
}

fn default_channel_pack_limit() -> u64 {
    20
}

#[utoipa::path(get, path = "/api/v1/channels/{id}/packs", params(ChannelPacksQuery), responses((status = 200, body = [ChannelPackSummary])))]
async fn channel_packs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<ChannelPacksQuery>,
) -> Result<Json<Vec<ChannelPackSummary>>, AppError> {
    if state.db.get_channel(&id).await?.is_none() {
        return Err(AppError::not_found(
            "channel_not_found",
            "Channel was not found",
        ));
    }
    Ok(Json(
        state
            .db
            .list_channel_packs(&id, query.limit, query.offset)
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/v1/channel-packs/{id}", responses((status = 200, body = ChannelPack)))]
async fn channel_pack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<ChannelPack>, AppError> {
    let fingerprint =
        channel::preference_fingerprint(&state, &state.db.get_runtime_preferences().await?)?;
    let mut pack = state
        .db
        .get_channel_pack(id, &fingerprint)
        .await?
        .ok_or_else(|| {
            AppError::not_found("channel_pack_not_found", "Channel pack was not found")
        })?;
    hydrate_pack_jobs(&state, &mut pack).await?;
    channel::hydrate_pack_downloads(&state, &mut pack.items).await?;
    Ok(Json(pack))
}

#[utoipa::path(post, path = "/api/v1/channel-packs/{id}/replan", responses((status = 200, body = ChannelPack)))]
async fn replan_channel_pack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<ChannelPack>, AppError> {
    let preferences = state.db.get_runtime_preferences().await?;
    let fingerprint = channel::preference_fingerprint(&state, &preferences)?;
    let pack = state
        .db
        .get_channel_pack(id, &fingerprint)
        .await?
        .ok_or_else(|| {
            AppError::not_found("channel_pack_not_found", "Channel pack was not found")
        })?;
    if pack.decision != ChannelPackDecision::Open {
        return Err(AppError::conflict(
            "channel_pack_decided",
            "Only open packs can be replanned",
        ));
    }
    let items = channel::replan_items(&state, pack.items).await?;
    state
        .db
        .replace_channel_plan(id, &fingerprint, &items)
        .await?;
    let mut pack = state
        .db
        .get_channel_pack(id, &fingerprint)
        .await?
        .context("channel pack disappeared")?;
    hydrate_pack_jobs(&state, &mut pack).await?;
    channel::hydrate_pack_downloads(&state, &mut pack.items).await?;
    Ok(Json(pack))
}

#[utoipa::path(post, path = "/api/v1/channel-packs/{id}/items/{ordinal}/attach", request_body = AttachChannelPackItem, responses((status = 200, body = ChannelPack)))]
async fn attach_channel_pack_item(
    State(state): State<Arc<AppState>>,
    Path((id, ordinal)): Path<(Uuid, u32)>,
    Json(request): Json<AttachChannelPackItem>,
) -> Result<Json<ChannelPack>, AppError> {
    let preferences = state.db.get_runtime_preferences().await?;
    let fingerprint = channel::preference_fingerprint(&state, &preferences)?;
    let mut pack = load_open_pack(&state, id, request.plan_version, &fingerprint).await?;
    let index = pack
        .items
        .iter()
        .position(|item| item.ordinal == ordinal)
        .ok_or_else(|| {
            AppError::not_found("channel_pack_item_not_found", "Pack item was not found")
        })?;
    let source = pack.items[index].source.clone();
    pack.items[index] = channel::resolve_release(&state, source, request.release_id, &preferences)
        .await
        .map_err(|error| AppError::bad_request("invalid_channel_match", error))?;
    channel::coordinate_pack_plan(&state, &mut pack.items, &preferences).await;
    state
        .db
        .replace_channel_plan(id, &fingerprint, &pack.items)
        .await?;
    let mut pack = state
        .db
        .get_channel_pack(id, &fingerprint)
        .await?
        .context("channel pack disappeared")?;
    hydrate_pack_jobs(&state, &mut pack).await?;
    channel::hydrate_pack_downloads(&state, &mut pack.items).await?;
    Ok(Json(pack))
}

#[utoipa::path(post, path = "/api/v1/channel-packs/{id}/reject", request_body = DecideChannelPack, responses((status = 200, body = ChannelPack)))]
async fn reject_channel_pack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<DecideChannelPack>,
) -> Result<Json<ChannelPack>, AppError> {
    let fingerprint =
        channel::preference_fingerprint(&state, &state.db.get_runtime_preferences().await?)?;
    let pack = load_open_pack(&state, id, request.plan_version, &fingerprint).await?;
    state
        .db
        .decide_channel_pack(pack.id, ChannelPackDecision::Rejected)
        .await?;
    Ok(Json(
        state
            .db
            .get_channel_pack(id, &fingerprint)
            .await?
            .context("channel pack disappeared")?,
    ))
}

#[utoipa::path(post, path = "/api/v1/channel-packs/{id}/accept", request_body = DecideChannelPack, responses((status = 202, body = ChannelBatchResult)))]
async fn accept_channel_pack(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(request): Json<DecideChannelPack>,
) -> Result<(StatusCode, Json<ChannelBatchResult>), AppError> {
    let preferences = state.db.get_runtime_preferences().await?;
    let fingerprint = channel::preference_fingerprint(&state, &preferences)?;
    let mut pack = load_open_pack(&state, id, request.plan_version, &fingerprint).await?;
    let selected = request
        .ordinals
        .map(|ordinals| ordinals.into_iter().collect::<HashSet<_>>());
    if selected.as_ref().is_some_and(HashSet::is_empty) {
        return Err(AppError::bad_request(
            "empty_channel_selection",
            "Select at least one planned release to accept",
        ));
    }
    if let Some(selected) = &selected {
        let actionable = pack
            .items
            .iter()
            .filter(|item| channel_item_is_actionable(item) && selected.contains(&item.ordinal))
            .count();
        if actionable != selected.len() {
            return Err(AppError::bad_request(
                "invalid_channel_selection",
                "The selection contains an item that has no replacement action",
            ));
        }
    }
    let mut jobs = Vec::new();
    let mut submitted = 0;
    for item in &mut pack.items {
        if !channel_item_is_actionable(item) {
            continue;
        }
        if selected
            .as_ref()
            .is_some_and(|ordinals| !ordinals.contains(&item.ordinal))
        {
            item.plan_state = crate::model::PackItemPlanState::Excluded;
            item.plan = None;
            item.reason = Some("Excluded from the accepted pack by the user".into());
            state.db.update_channel_pack_item(pack.id, item).await?;
            continue;
        }
        let mut job = None;
        if let Some(plan) = &item.plan {
            let request = CreateDownload {
                tracker: plan.tracker.clone(),
                torrent_id: plan.torrent_id,
                profile: plan.profile.clone(),
                use_token: plan.use_token,
            };
            let key = format!(
                "channel-pack:{}:item:{}:plan:{}",
                pack.id, item.ordinal, pack.plan_version
            );
            job = Some(enqueue_download(state.clone(), request, Some(&key)).await?);
        }
        if let Some(replacement) = item.replacement.as_ref() {
            let target_download = replacement
                .downloads
                .iter()
                .find(|download| download.live.progress >= 1.0)
                .or_else(|| replacement.downloads.first());
            let import_id = state
                .db
                .create_replacement_import(CreateReplacementImport {
                    download_job_id: job.as_ref().map(|job| job.id),
                    target_client: target_download.map(|download| download.live.client.as_str()),
                    target_info_hash: target_download
                        .map(|download| download.live.info_hash.as_str()),
                    release_id: item.release.as_ref().and_then(|release| release.id),
                    tracker: &replacement.tracker,
                    torrent_id: replacement.torrent_id,
                    display_name: &item.source.title,
                    target_complete: target_download
                        .is_some_and(|download| download.live.progress >= 1.0),
                    sources: &item.source.trumped_downloads,
                    cleanup_mode: preferences.imports.trumped_cleanup,
                })
                .await?;
            background::enqueue_import_processing(&state, import_id).await?;
        }
        item.plan_state = crate::model::PackItemPlanState::Submitted;
        item.job_id = job.as_ref().map(|job| job.id);
        item.job = job.clone();
        state.db.update_channel_pack_item(pack.id, item).await?;
        if let Some(job) = job {
            jobs.push(job);
        }
        submitted += 1;
    }
    state
        .db
        .decide_channel_pack(pack.id, ChannelPackDecision::Accepted)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ChannelBatchResult {
            pack_id: pack.id,
            submitted,
            skipped: pack.items.len().saturating_sub(submitted),
            jobs,
        }),
    ))
}

fn channel_item_is_actionable(item: &crate::model::ChannelPackItem) -> bool {
    item.plan_state == crate::model::PackItemPlanState::Executable
        || (item.replacement.is_some()
            && matches!(
                item.plan_state,
                crate::model::PackItemPlanState::CleanupReady
                    | crate::model::PackItemPlanState::AlreadyDownloading
            ))
}

async fn hydrate_pack_jobs(state: &AppState, pack: &mut ChannelPack) -> Result<(), AppError> {
    for item in &mut pack.items {
        if let Some(id) = item.job_id {
            item.job = state.db.get_job(id).await?;
        }
    }
    Ok(())
}

async fn load_open_pack(
    state: &Arc<AppState>,
    id: Uuid,
    plan_version: i32,
    fingerprint: &str,
) -> Result<ChannelPack, AppError> {
    let pack = state
        .db
        .get_channel_pack(id, fingerprint)
        .await?
        .ok_or_else(|| {
            AppError::not_found("channel_pack_not_found", "Channel pack was not found")
        })?;
    if pack.decision != ChannelPackDecision::Open {
        return Err(AppError::conflict(
            "channel_pack_decided",
            "This pack has already been accepted or rejected",
        ));
    }
    if pack.plan_version != plan_version || pack.plan_stale {
        return Err(AppError::conflict(
            "plan_stale",
            "The download plan has changed; replan before deciding",
        ));
    }
    Ok(pack)
}

fn hydrate_channel_config(state: &AppState, channel: &mut ChannelConfig) -> Result<(), AppError> {
    channel.credential_configured =
        !matches!(channel.kind, ChannelKind::Lastfm) || state.lastfm_api_key.is_some();
    channel.next_refresh_at = channel::next_refresh_at(channel, Utc::now()).ok();
    Ok(())
}

async fn start_channel_run(
    state: Arc<AppState>,
    config: ChannelConfig,
    trigger: ChannelRunTrigger,
) -> Result<ChannelRun, AppError> {
    channel::validate_channel_refresh(&config, state.lastfm_api_key.is_some())
        .map_err(|error| AppError::bad_request("invalid_channel", error))?;
    let run = state
        .db
        .create_channel_run(&config.id, trigger)
        .await?
        .ok_or_else(|| {
            AppError::conflict(
                "channel_refresh_running",
                "A refresh is already running for this channel",
            )
        })?;
    if let Some(mut current) = state.db.get_channel(&config.id).await? {
        current.last_attempt_at = Some(Utc::now());
        current.last_error = None;
        current.updated_at = Utc::now();
        state.db.put_channel(&current).await?;
    }
    let task_state = state.clone();
    let task_run = run.clone();
    tokio::spawn(async move {
        let outcome =
            channel::refresh_channel(task_state.clone(), config.clone(), task_run.id).await;
        match outcome {
            Ok((pack_id, status)) => {
                let _ = task_state
                    .db
                    .finish_channel_run(task_run.id, status.clone(), Some(pack_id), None)
                    .await;
                if let Ok(Some(mut current)) = task_state.db.get_channel(&config.id).await {
                    if status == ChannelRunStatus::Successful {
                        current.last_successful_at = Some(Utc::now());
                    }
                    current.last_error = None;
                    current.failure_count = 0;
                    current.updated_at = Utc::now();
                    let _ = task_state.db.put_channel(&current).await;
                }
            }
            Err(error) => {
                let message = truncate(&error.to_string(), 500);
                tracing::error!(channel = %config.id, %message, "channel refresh failed");
                let _ = task_state
                    .db
                    .finish_channel_run(task_run.id, ChannelRunStatus::Failed, None, Some(&message))
                    .await;
                if let Ok(Some(mut current)) = task_state.db.get_channel(&config.id).await {
                    current.last_error = Some(message);
                    current.failure_count = current.failure_count.saturating_add(1);
                    current.updated_at = Utc::now();
                    let _ = task_state.db.put_channel(&current).await;
                }
            }
        }
    });
    Ok(run)
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

#[utoipa::path(
    get, path = "/api/v1/accounts",
    responses((status = 200, body = Vec<crate::model::TrackerAccount>))
)]
async fn accounts(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<crate::model::TrackerAccount>>, AppError> {
    let mut names = state.trackers.keys().cloned().collect::<Vec<_>>();
    names.sort();
    let mut values = Vec::new();
    for tracker_name in names {
        let tracker = &state.trackers[&tracker_name];
        let cached = state
            .db
            .get_snapshot::<Account>(&tracker_name, "account", "current")
            .await?;
        if let Some(cached) = cached
            .as_ref()
            .filter(|cached| cached.expires_at > Utc::now())
        {
            values.push(crate::model::TrackerAccount {
                tracker: tracker_name.clone(),
                account: cached.value.clone(),
                provenance: provenance(&tracker_name, cached.fetched_at, false),
                error: None,
            });
            continue;
        }
        match tracker.account().await {
            Ok((account, raw)) => {
                let cached = store(
                    &state.db,
                    &tracker_name,
                    "account",
                    "current",
                    account.clone(),
                    raw,
                    60,
                )
                .await?;
                values.push(crate::model::TrackerAccount {
                    tracker: tracker_name.clone(),
                    account,
                    provenance: provenance(&tracker_name, cached.fetched_at, false),
                    error: None,
                });
            }
            Err(error) => {
                if let Some(cached) = cached {
                    values.push(crate::model::TrackerAccount {
                        tracker: tracker_name.clone(),
                        account: cached.value,
                        provenance: provenance(&tracker_name, cached.fetched_at, true),
                        error: Some(error.to_string()),
                    });
                }
            }
        }
    }
    if values.is_empty() {
        return Err(AppError::unavailable(
            "all_trackers_unavailable",
            "No tracker account could be loaded",
        ));
    }
    Ok(Json(values))
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
    if query
        .tracker
        .as_deref()
        .is_none_or(|tracker| tracker == "all")
        && state.trackers.len() > 1
    {
        return federated_search(&state, query).await;
    }
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
        let mut response = envelope(tracker_name, cached, false);
        assign_search_ids(&state.db, &mut response.data).await?;
        enrich_search_downloads(&state, &mut response.data).await?;
        apply_search_eligibility(&state, &mut response.data).await?;
        enrich_search_deduplication(&state, tracker_name, &mut response.data).await?;
        return Ok(Json(response));
    }
    match tracker.search(&request).await {
        Ok((mut value, raw)) => {
            cache_search_canonical(&state.db, tracker_name, &value).await?;
            let cached = store(
                &state.db,
                tracker_name,
                "search",
                &key,
                value.clone(),
                raw,
                300,
            )
            .await?;
            assign_search_ids(&state.db, &mut value).await?;
            enrich_search_downloads(&state, &mut value).await?;
            apply_search_eligibility(&state, &mut value).await?;
            enrich_search_deduplication(&state, tracker_name, &mut value).await?;
            Ok(Json(ApiEnvelope {
                data: value,
                provenance: provenance(tracker_name, cached.fetched_at, false),
            }))
        }
        Err(error) => {
            let mut response = stale_or_error(&state.db, tracker_name, "search", &key, error)
                .await?
                .0;
            assign_search_ids(&state.db, &mut response.data).await?;
            enrich_search_downloads(&state, &mut response.data).await?;
            apply_search_eligibility(&state, &mut response.data).await?;
            enrich_search_deduplication(&state, tracker_name, &mut response.data).await?;
            Ok(Json(response))
        }
    }
}

async fn federated_search(
    state: &Arc<AppState>,
    query: SearchQuery,
) -> Result<Json<ApiEnvelope<SearchPage>>, AppError> {
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
    let preferences = state.db.get_runtime_preferences().await?;
    let mut tasks = tokio::task::JoinSet::new();
    let mut pages = std::collections::HashMap::new();
    let mut source_status = Vec::new();
    for (name, tracker) in &state.trackers {
        if !query.refresh
            && let Some(cached) = state
                .db
                .get_snapshot::<SearchPage>(name, "search", &key)
                .await?
            && cached.expires_at > Utc::now()
        {
            let mut page = cached.value;
            assign_search_ids(&state.db, &mut page).await?;
            enrich_search_downloads(state, &mut page).await?;
            apply_search_eligibility(state, &mut page).await?;
            enrich_search_deduplication(state, name, &mut page).await?;
            source_status.push(crate::model::SourceLoadStatus {
                tracker: name.clone(),
                state: "ready".into(),
                error: None,
            });
            pages.insert(name.clone(), page);
            continue;
        }
        let name = name.clone();
        let tracker = tracker.clone();
        let request = request.clone();
        tasks.spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(10), tracker.search(&request))
                .await
                .map_err(|_| anyhow!("tracker search timed out after 10 seconds"))
                .and_then(|result| result);
            (name, result)
        });
    }

    let mut errors = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (tracker_name, result) =
            result.map_err(|error| AppError::unavailable("tracker_task_failed", error))?;
        match result {
            Ok((mut page, raw)) => {
                source_status.push(crate::model::SourceLoadStatus {
                    tracker: tracker_name.clone(),
                    state: "ready".into(),
                    error: None,
                });
                cache_search_canonical(&state.db, &tracker_name, &page).await?;
                let key = search_cache_key(&request);
                let _ = store(
                    &state.db,
                    &tracker_name,
                    "search",
                    &key,
                    page.clone(),
                    raw,
                    300,
                )
                .await?;
                assign_search_ids(&state.db, &mut page).await?;
                enrich_search_downloads(state, &mut page).await?;
                apply_search_eligibility(state, &mut page).await?;
                enrich_search_deduplication(state, &tracker_name, &mut page).await?;
                pages.insert(tracker_name, page);
            }
            Err(error) => {
                tracing::warn!(tracker = %tracker_name, %error, "federated search source failed");
                let message = error.to_string();
                if let Some(cached) = state
                    .db
                    .get_snapshot::<SearchPage>(&tracker_name, "search", &key)
                    .await?
                {
                    let mut page = cached.value;
                    assign_search_ids(&state.db, &mut page).await?;
                    enrich_search_downloads(state, &mut page).await?;
                    apply_search_eligibility(state, &mut page).await?;
                    enrich_search_deduplication(state, &tracker_name, &mut page).await?;
                    source_status.push(crate::model::SourceLoadStatus {
                        tracker: tracker_name.clone(),
                        state: "stale".into(),
                        error: Some(message),
                    });
                    pages.insert(tracker_name, page);
                } else {
                    errors.push((tracker_name.clone(), message.clone()));
                    source_status.push(crate::model::SourceLoadStatus {
                        tracker: tracker_name,
                        state: "unavailable".into(),
                        error: Some(message),
                    });
                }
            }
        }
    }
    if pages.is_empty() {
        return Err(AppError::unavailable(
            "all_trackers_unavailable",
            errors
                .into_iter()
                .map(|(tracker, error)| format!("{tracker}: {error}"))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }

    let mut order = preferences.release.tracker_order.clone();
    for tracker in state.trackers.keys() {
        if !order
            .iter()
            .any(|known| known.eq_ignore_ascii_case(tracker))
        {
            order.push(tracker.clone());
        }
    }
    let total_pages = pages
        .values()
        .map(|page| page.total_pages)
        .max()
        .unwrap_or(1);
    let reported_total_results: i64 = pages.values().filter_map(|page| page.total_results).sum();
    let current_page = pages
        .values()
        .map(|page| page.current_page)
        .max()
        .unwrap_or(1);
    let mut groups: Vec<crate::model::SearchGroup> = Vec::new();
    for tracker in order {
        let Some(page) = pages.remove(&tracker) else {
            continue;
        };
        for group in page.groups {
            let matched = groups.iter().enumerate().find_map(|(index, known)| {
                if known.tracker.eq_ignore_ascii_case(&group.tracker) {
                    return None;
                }
                let score = crate::release_matcher::group_score(known, &group);
                (score >= crate::release_matcher::AUTO_MERGE_THRESHOLD).then_some((index, score))
            });
            if let Some((index, score)) = matched {
                crate::release_matcher::merge_search_group(&mut groups[index], group, score);
            } else {
                groups.push(group);
            }
        }
    }
    for page in pages.into_values() {
        groups.extend(page.groups);
    }
    for group in &groups {
        persist_release_match(state, &group.sources).await?;
    }
    for group in &mut groups {
        group.id = state.db.merge_release_sources(&group.sources).await?;
        if let Some(id) = group.id
            && let Some(detail) = state.db.get_release_detail(id).await?
        {
            group.name = detail.release.title;
            group.artist = detail.release.artist;
            group.year = detail.release.year;
            group.release_type = detail.release.release_type;
            group.image = detail.release.artwork;
            group.sources = detail.release.sources;
        }
    }
    groups.sort_by(|left, right| {
        let left_popularity = left
            .torrents
            .iter()
            .map(|torrent| torrent.seeders.unwrap_or_default())
            .max()
            .unwrap_or_default();
        let right_popularity = right
            .torrents
            .iter()
            .map(|torrent| torrent.seeders.unwrap_or_default())
            .max()
            .unwrap_or_default();
        right_popularity
            .cmp(&left_popularity)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    let stale = source_status.iter().any(|source| source.state != "ready");
    Ok(Json(ApiEnvelope {
        data: SearchPage {
            current_page,
            total_pages,
            total_results: Some(reported_total_results.max(groups.len() as i64)),
            groups,
            deduplication: Default::default(),
            source_status,
        },
        provenance: provenance("all", Utc::now(), stale),
    }))
}

async fn persist_release_match(
    state: &Arc<AppState>,
    sources: &[crate::model::ReleaseSource],
) -> Result<(), AppError> {
    if sources.len() < 2 {
        return Ok(());
    }
    state.db.merge_release_sources(sources).await?;
    let record = crate::model::ReleaseMatchRecord {
        matcher_version: crate::release_matcher::MATCHER_VERSION,
        sources: sources.to_vec(),
    };
    for source in sources {
        let _ = store(
            &state.db,
            &source.tracker,
            "release_match",
            &source.group_id.to_string(),
            record.clone(),
            json!({ "matcherVersion": crate::release_matcher::MATCHER_VERSION }),
            2_592_000,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn assign_search_ids(db: &Database, page: &mut SearchPage) -> Result<()> {
    for group in &mut page.groups {
        group.id = db
            .release_id_for_source(&group.tracker, group.group_id)
            .await?;
        if let Some(id) = group.id
            && let Some(detail) = db.get_release_detail(id).await?
        {
            group.name = detail.release.title;
            group.artist = detail.release.artist;
            group.year = detail.release.year;
            group.release_type = detail.release.release_type;
            group.image = detail.release.artwork;
            group.sources = detail.release.sources;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, IntoParams)]
struct GroupQuery {
    #[serde(default)]
    refresh: bool,
    torrent: Option<i64>,
}

#[utoipa::path(
    get, path = "/api/v1/releases/{id}",
    params(
        ("id" = Uuid, Path, description = "Canonical release UUID"),
        GroupQuery
    ),
    responses(
        (status = 200, body = inline(ApiEnvelope<ReleaseDetail>)),
        (status = 404, description = "Canonical release not found")
    )
)]
async fn release(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
    Query(query): Query<GroupQuery>,
) -> Result<Json<ApiEnvelope<ReleaseDetail>>, AppError> {
    let mut detail = state
        .db
        .get_release_detail(id)
        .await?
        .ok_or_else(|| AppError::not_found("release_not_found", "Release was not found"))?;
    enrich_variant_downloads(&state, &mut detail.variants).await?;
    for source in detail.release.sources.clone() {
        enrich_variant_library(
            &state,
            &source.tracker,
            source.group_id,
            &mut detail.variants,
        )
        .await?;
    }
    if let Some(torrent_id) = query.torrent
        && let Some(variant) = detail
            .variants
            .iter()
            .find(|variant| variant.torrent_id == torrent_id)
            .cloned()
        && !detail
            .variants
            .iter()
            .any(|known| known.tracker == variant.tracker && known.torrent_id == torrent_id)
    {
        detail.variants.push(variant);
    }
    apply_download_eligibility(&state, &mut detail.variants).await?;
    if query.refresh {
        for source in detail.release.sources.clone() {
            if state.trackers.contains_key(&source.tracker) {
                let refresh_state = state.clone();
                tokio::spawn(async move {
                    if let Err(error) =
                        refresh_group(refresh_state, source.tracker, source.group_id).await
                    {
                        tracing::warn!(%error, "canonical release source refresh failed");
                    }
                });
            }
        }
    }
    Ok(Json(ApiEnvelope {
        data: detail,
        provenance: provenance("canonical", Utc::now(), false),
    }))
}

async fn update_release_metadata(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
    Json(overrides): Json<Value>,
) -> Result<StatusCode, AppError> {
    if state.db.set_release_overrides(id, overrides).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found(
            "release_not_found",
            "Release was not found",
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnlinkReleaseSource {
    tracker: String,
    group_id: i64,
}

async fn unlink_release_source(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
    Json(source): Json<UnlinkReleaseSource>,
) -> Result<Json<Value>, AppError> {
    let new_id = state
        .db
        .unlink_release_source(id, &source.tracker, source.group_id)
        .await?
        .ok_or_else(|| {
            AppError::not_found(
                "release_source_not_found",
                "That source is not attached to this release",
            )
        })?;
    Ok(Json(json!({ "releaseId": new_id })))
}

async fn canonical_index(
    State(state): State<Arc<AppState>>,
) -> Result<Json<crate::db::CanonicalBackfillProgress>, AppError> {
    Ok(Json(state.db.canonical_backfill_progress().await?))
}

#[derive(Debug, Deserialize)]
struct MatchQuery {
    kind: Option<String>,
    status: Option<String>,
    #[serde(default = "default_match_limit")]
    limit: u64,
}

fn default_match_limit() -> u64 {
    100
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MatchCandidateView {
    id: String,
    kind: String,
    left_id: String,
    right_id: String,
    score: f64,
    status: String,
    evidence: Value,
    left: Value,
    right: Value,
    created_at: String,
    updated_at: String,
}

async fn match_candidates(
    State(state): State<Arc<AppState>>,
    Query(query): Query<MatchQuery>,
) -> Result<Json<Vec<MatchCandidateView>>, AppError> {
    let rows = state
        .db
        .list_match_candidates(query.kind.as_deref(), query.status.as_deref(), query.limit)
        .await?;
    let mut items = Vec::new();
    for row in rows {
        let left_id = uuid::Uuid::parse_str(&row.left_id)?;
        let right_id = uuid::Uuid::parse_str(&row.right_id)?;
        let (left, right) = if row.kind == "release" {
            (
                state
                    .db
                    .get_release_detail(left_id)
                    .await?
                    .map(|detail| serde_json::to_value(detail.release))
                    .transpose()?
                    .unwrap_or(Value::Null),
                state
                    .db
                    .get_release_detail(right_id)
                    .await?
                    .map(|detail| serde_json::to_value(detail.release))
                    .transpose()?
                    .unwrap_or(Value::Null),
            )
        } else {
            (
                state
                    .db
                    .get_canonical_artist(left_id)
                    .await?
                    .map(|artist| json!({ "id": artist.id, "name": artist.name }))
                    .unwrap_or(Value::Null),
                state
                    .db
                    .get_canonical_artist(right_id)
                    .await?
                    .map(|artist| json!({ "id": artist.id, "name": artist.name }))
                    .unwrap_or(Value::Null),
            )
        };
        items.push(MatchCandidateView {
            id: row.id,
            kind: row.kind,
            left_id: row.left_id,
            right_id: row.right_id,
            score: row.score,
            status: row.status,
            evidence: row.evidence_json,
            left,
            right,
            created_at: row.created_at,
            updated_at: row.updated_at,
        });
    }
    Ok(Json(items))
}

async fn accept_match(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<StatusCode, AppError> {
    if state.db.decide_match_candidate(id, true).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found(
            "match_not_found",
            "Match candidate was not found",
        ))
    }
}

async fn reject_match(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<StatusCode, AppError> {
    if state.db.decide_match_candidate(id, false).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found(
            "match_not_found",
            "Match candidate was not found",
        ))
    }
}

#[utoipa::path(
    get, path = "/api/v1/groups/{tracker}/{id}",
    params(
        ("tracker" = String, Path, description = "Configured tracker name"),
        ("id" = i64, Path, description = "Tracker release group ID"),
        GroupQuery
    ),
    responses((status = 200, body = inline(ApiEnvelope<ReleaseDetail>)))
)]
#[allow(dead_code)]
async fn group(
    State(state): State<Arc<AppState>>,
    Path((tracker_name, id)): Path<(String, i64)>,
    Query(query): Query<GroupQuery>,
) -> Result<Json<ApiEnvelope<ReleaseDetail>>, AppError> {
    let (_, tracker) = get_tracker(&state, Some(&tracker_name))?;
    let key = id.to_string();
    if !query.refresh
        && let Some(cached) = state
            .db
            .get_snapshot::<ReleaseDetail>(&tracker_name, "group", &key)
            .await?
    {
        let stale = cached.expires_at <= Utc::now();
        let mut response = envelope(&tracker_name, cached, stale);
        enrich_cross_tracker_detail(&state, &tracker_name, &mut response.data).await?;
        enrich_requested_variant(
            &state,
            &tracker_name,
            id,
            query.torrent,
            &mut response.data.variants,
        )
        .await?;
        enrich_variant_downloads(&state, &mut response.data.variants).await?;
        enrich_variant_library(&state, &tracker_name, id, &mut response.data.variants).await?;
        for source in response.data.release.sources.clone() {
            if !source.tracker.eq_ignore_ascii_case(&tracker_name) {
                enrich_variant_library(
                    &state,
                    &source.tracker,
                    source.group_id,
                    &mut response.data.variants,
                )
                .await?;
            }
        }
        apply_download_eligibility(&state, &mut response.data.variants).await?;
        if stale {
            let refresh_state = state.clone();
            let refresh_tracker = tracker_name.clone();
            tokio::spawn(async move {
                if let Err(error) = refresh_group(refresh_state, refresh_tracker, id).await {
                    tracing::warn!(%error, "asynchronous release refresh failed");
                }
            });
        }
        return Ok(Json(response));
    }
    match tracker.group(id).await {
        Ok((mut value, raw)) => {
            let track_index = track_index_from_group(&tracker_name, &value, &raw);
            state.db.enqueue_track_index(&tracker_name, id).await?;
            state.db.put_track_index(&track_index).await?;
            cache_release_detail(&state.db, &value).await?;
            let cached = store(
                &state.db,
                &tracker_name,
                "group",
                &key,
                value.clone(),
                raw,
                86_400,
            )
            .await?;
            enrich_cross_tracker_detail(&state, &tracker_name, &mut value).await?;
            enrich_requested_variant(
                &state,
                &tracker_name,
                id,
                query.torrent,
                &mut value.variants,
            )
            .await?;
            enrich_variant_downloads(&state, &mut value.variants).await?;
            enrich_variant_library(&state, &tracker_name, id, &mut value.variants).await?;
            for source in value.release.sources.clone() {
                if !source.tracker.eq_ignore_ascii_case(&tracker_name) {
                    enrich_variant_library(
                        &state,
                        &source.tracker,
                        source.group_id,
                        &mut value.variants,
                    )
                    .await?;
                }
            }
            apply_download_eligibility(&state, &mut value.variants).await?;
            Ok(Json(ApiEnvelope {
                data: value,
                provenance: provenance(&tracker_name, cached.fetched_at, false),
            }))
        }
        Err(error) => {
            let mut response =
                stale_or_error::<ReleaseDetail>(&state.db, &tracker_name, "group", &key, error)
                    .await?;
            enrich_cross_tracker_detail(&state, &tracker_name, &mut response.0.data).await?;
            enrich_requested_variant(
                &state,
                &tracker_name,
                id,
                query.torrent,
                &mut response.0.data.variants,
            )
            .await?;
            enrich_variant_downloads(&state, &mut response.0.data.variants).await?;
            enrich_variant_library(&state, &tracker_name, id, &mut response.0.data.variants)
                .await?;
            apply_download_eligibility(&state, &mut response.0.data.variants).await?;
            Ok(response)
        }
    }
}

#[utoipa::path(
    get, path = "/api/v1/releases/{id}/cross-seed-plans",
    params(
        ("id" = Uuid, Path, description = "Canonical release UUID")
    ),
    responses((status = 200, body = Vec<crate::model::CrossSeedPlan>))
)]
async fn cross_seed_plans(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Vec<crate::model::CrossSeedPlan>>, AppError> {
    let mut detail = state
        .db
        .get_release_detail(id)
        .await?
        .ok_or_else(|| AppError::not_found("release_not_found", "Release was not found"))?;
    enrich_variant_downloads(&state, &mut detail.variants).await?;
    apply_download_eligibility(&state, &mut detail.variants).await?;

    let sources = detail
        .variants
        .iter()
        .filter_map(|variant| {
            variant
                .downloads
                .iter()
                .find(|download| download.progress >= 1.0)
                .map(|download| (variant, download))
        })
        .collect::<Vec<_>>();
    let mut raw_groups = std::collections::HashMap::new();
    let mut plans = Vec::new();
    for target in detail
        .variants
        .iter()
        .filter(|variant| variant.downloads.is_empty())
    {
        let Some((source, download)) = sources
            .iter()
            .find(|(source, _)| !source.tracker.eq_ignore_ascii_case(&target.tracker))
        else {
            continue;
        };
        let Some(client) = state.download_clients.get(&download.client) else {
            continue;
        };
        let source_files = client.files(&download.info_hash).await?;
        if let std::collections::hash_map::Entry::Vacant(entry) =
            raw_groups.entry((target.tracker.clone(), target.group_id))
        {
            let Some(tracker) = state.trackers.get(&target.tracker) else {
                continue;
            };
            let (_, raw) = tracker.group(target.group_id).await?;
            entry.insert(raw);
        }
        let target_files = raw_groups
            .get(&(target.tracker.clone(), target.group_id))
            .map(|raw| torrent_manifest(raw, target.torrent_id))
            .unwrap_or_default();
        let mut available = source_files
            .iter()
            .map(|file| (manifest_name(&file.name), file.size))
            .collect::<Vec<_>>();
        let mut missing = Vec::new();
        let mut matched = 0;
        for (name, size) in &target_files {
            if let Some(index) = available
                .iter()
                .position(|candidate| candidate.0 == *name && candidate.1 == *size)
            {
                available.swap_remove(index);
                matched += 1;
            } else {
                missing.push(name.clone());
            }
        }
        let compatible = !target_files.is_empty()
            && missing.is_empty()
            && source_files.iter().all(|file| file.progress >= 1.0);
        let policy_eligible = target
            .eligibility
            .as_ref()
            .is_some_and(|eligibility| eligibility.eligible);
        plans.push(crate::model::CrossSeedPlan {
            source_tracker: source.tracker.clone(),
            source_torrent_id: source.torrent_id,
            source_client: download.client.clone(),
            source_info_hash: download.info_hash.clone(),
            source_path: download.save_path.clone(),
            target_tracker: target.tracker.clone(),
            target_torrent_id: target.torrent_id,
            compatible,
            matched_files: matched,
            target_files: target_files.len(),
            missing_files: missing,
            policy_eligible,
            summary: if compatible {
                "All target files are already present; this is a dry plan only.".into()
            } else {
                "The target manifest is not a complete file-for-file match.".into()
            },
            dry_run: true,
        });
    }
    Ok(Json(plans))
}

fn torrent_manifest(raw: &Value, torrent_id: i64) -> Vec<(String, i64)> {
    let response = raw.get("response").unwrap_or(raw);
    response
        .get("torrents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|torrent| value_i64(torrent, &["id", "torrentId"]) == Some(torrent_id))
        .and_then(|torrent| torrent.get("fileList").and_then(Value::as_str))
        .map(|files| {
            files
                .split("|||")
                .filter_map(|entry| {
                    let (path, size) = entry.rsplit_once("{{{")?;
                    let size = size.trim_end_matches("}}}").parse().ok()?;
                    Some((manifest_name(path), size))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn manifest_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .trim()
        .to_lowercase()
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
        Ok((value, canonical, raw)) => {
            state
                .db
                .put_canonical(
                    &canonical,
                    Utc::now(),
                    Utc::now() + ChronoDuration::hours(24),
                )
                .await?;
            let cached = store(&state.db, &tracker_name, "torrent", &key, value, raw, 900).await?;
            Ok(Json(envelope(&tracker_name, cached, false)))
        }
        Err(error) => stale_or_error(&state.db, &tracker_name, "torrent", &key, error).await,
    }
}

#[utoipa::path(
    get, path = "/api/v1/artists/{tracker}/{id}/releases",
    params(
        ("tracker" = String, Path),
        ("id" = i64, Path),
        RefreshQuery
    ),
    responses((status = 200, body = inline(ApiEnvelope<ArtistCatalogPage>)))
)]
#[allow(dead_code)]
async fn artist_catalog(
    State(state): State<Arc<AppState>>,
    Path((tracker_name, id)): Path<(String, i64)>,
    Query(query): Query<RefreshQuery>,
) -> Result<Json<ApiEnvelope<ArtistCatalogPage>>, AppError> {
    let (_, tracker) = get_tracker(&state, Some(&tracker_name))?;
    let key = id.to_string();
    if !query.refresh
        && let Some(cached) = state
            .db
            .get_snapshot::<ArtistCatalogPage>(&tracker_name, "artist", &key)
            .await?
    {
        let stale = cached.expires_at <= Utc::now();
        let mut response = envelope(&tracker_name, cached, stale);
        enrich_artist_catalog(&state, &tracker_name, id, &mut response.data).await?;
        enrich_cross_tracker_artist_catalog(&state, &tracker_name, &mut response.data).await?;
        enrich_artist_catalog(&state, &tracker_name, id, &mut response.data).await?;
        if stale {
            let job =
                background::enqueue_artist_catalog_refresh(&state, &tracker_name, id, false, false)
                    .await?;
            if let Some(source) = response.provenance.sources.first_mut() {
                source.refresh_job_id = Some(job.id);
                source.refresh_state = Some(job.state);
                source.retry_at = job.next_run_at;
                source.error_code = job.last_error_code;
            }
        }
        return Ok(Json(response));
    }
    let tracker_result = tokio::time::timeout(Duration::from_secs(10), tracker.artist_catalog(id))
        .await
        .map_err(|_| anyhow!("tracker artist catalog request timed out after 10 seconds"))
        .and_then(|result| result);
    match tracker_result {
        Ok((mut value, raw)) => {
            cache_artist_catalog(&state.db, &value).await?;
            let cached = store(
                &state.db,
                &tracker_name,
                "artist",
                &key,
                value.clone(),
                raw,
                86_400,
            )
            .await?;
            enrich_artist_catalog(&state, &tracker_name, id, &mut value).await?;
            enrich_cross_tracker_artist_catalog(&state, &tracker_name, &mut value).await?;
            enrich_artist_catalog(&state, &tracker_name, id, &mut value).await?;
            Ok(Json(ApiEnvelope {
                data: value,
                provenance: provenance(&tracker_name, cached.fetched_at, false),
            }))
        }
        Err(error) => {
            let mut response = stale_or_error::<ArtistCatalogPage>(
                &state.db,
                &tracker_name,
                "artist",
                &key,
                error,
            )
            .await?
            .0;
            enrich_artist_catalog(&state, &tracker_name, id, &mut response.data).await?;
            enrich_cross_tracker_artist_catalog(&state, &tracker_name, &mut response.data).await?;
            enrich_artist_catalog(&state, &tracker_name, id, &mut response.data).await?;
            Ok(Json(response))
        }
    }
}

#[utoipa::path(
    get, path = "/api/v1/artists/{id}",
    params(("id" = String, Path), RefreshQuery),
    responses(
        (status = 200, body = inline(ApiEnvelope<ArtistCatalogPage>)),
        (status = 404, description = "Canonical artist not found")
    )
)]
async fn canonical_artist_catalog(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
    Query(query): Query<RefreshQuery>,
) -> Result<Json<ApiEnvelope<ArtistCatalogPage>>, AppError> {
    let artist = state
        .db
        .get_canonical_artist(id)
        .await?
        .ok_or_else(|| AppError::not_found("artist_not_found", "Artist was not found"))?;
    let sources = state.db.artist_sources_for(id).await?;
    let mut snapshot_groups = Vec::new();
    let mut source_provenance = Vec::new();
    for source in sources {
        let provider_id = format!("tracker:{}", source.tracker);
        let Some(artist_id) = source.artist_id else {
            source_provenance.push(SourceProvenance {
                provider_id,
                tracker: source.tracker,
                state: SnapshotState::Missing,
                fetched_at: None,
                cache_age_seconds: None,
                refresh_job_id: None,
                refresh_state: None,
                retry_at: None,
                error_code: Some("artist_source_unresolved".into()),
            });
            continue;
        };
        let cached = state
            .db
            .get_snapshot::<ArtistCatalogPage>(&source.tracker, "artist", &artist_id.to_string())
            .await?;
        let fetched_at = cached.as_ref().map(|cached| cached.fetched_at);
        let source_state = match cached.as_ref() {
            Some(cached) if cached.expires_at > Utc::now() => SnapshotState::Fresh,
            Some(_) => SnapshotState::Stale,
            None => SnapshotState::Missing,
        };
        if let Some(cached) = cached {
            snapshot_groups.extend(cached.value.groups);
        }
        let refresh_job = if (query.refresh || source_state != SnapshotState::Fresh)
            && state.trackers.contains_key(&source.tracker)
        {
            Some(
                background::enqueue_artist_catalog_refresh(
                    &state,
                    &source.tracker,
                    artist_id,
                    query.refresh || source_state == SnapshotState::Missing,
                    query.refresh,
                )
                .await?,
            )
        } else {
            None
        };
        let provider = state.providers.status(&provider_id).await;
        source_provenance.push(SourceProvenance {
            provider_id,
            tracker: source.tracker,
            state: source_state,
            fetched_at,
            cache_age_seconds: fetched_at.map(|value| (Utc::now() - value).num_seconds().max(0)),
            refresh_job_id: refresh_job.as_ref().map(|job| job.id),
            refresh_state: refresh_job.as_ref().map(|job| job.state),
            retry_at: refresh_job
                .as_ref()
                .and_then(|job| job.next_run_at)
                .or_else(|| provider.as_ref().and_then(|status| status.retry_at)),
            error_code: refresh_job
                .and_then(|job| job.last_error_code)
                .or_else(|| provider.and_then(|status| status.reason_code)),
        });
    }

    let source_keys = snapshot_groups
        .iter()
        .map(|group| {
            (
                group.release.tracker.to_ascii_lowercase(),
                group.release.group_id,
            )
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let release_ids_by_source = state.db.release_ids_for_sources(&source_keys).await?;
    let mut groups: HashMap<String, ArtistCatalogRelease> = HashMap::new();
    for mut group in snapshot_groups {
        group.release.id = release_ids_by_source
            .get(&(
                group.release.tracker.to_ascii_lowercase(),
                group.release.group_id,
            ))
            .copied();
        let identity = group
            .release
            .id
            .map(|release_id| release_id.to_string())
            .unwrap_or_else(|| format!("{}:{}", group.release.tracker, group.release.group_id));
        if let Some(known) = groups.get_mut(&identity) {
            for variant in group.variants.drain(..) {
                if !known.variants.iter().any(|candidate| {
                    candidate.tracker == variant.tracker
                        && candidate.torrent_id == variant.torrent_id
                }) {
                    known.variants.push(variant);
                }
            }
            for tag in group.tags.drain(..) {
                if !known
                    .tags
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(&tag))
                {
                    known.tags.push(tag);
                }
            }
            for role in group.roles.drain(..) {
                if !known.roles.contains(&role) {
                    known.roles.push(role);
                }
            }
        } else {
            groups.insert(identity, group);
        }
    }

    let mut catalog_groups = groups.into_values().collect::<Vec<_>>();
    let release_ids = catalog_groups
        .iter()
        .filter_map(|group| group.release.id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let details = state.db.get_release_details(&release_ids).await?;
    for group in &mut catalog_groups {
        if let Some(release_id) = group.release.id
            && let Some(detail) = details.get(&release_id)
        {
            group.release = detail.release.clone();
            group.variants = detail.variants.clone();
        }
    }

    let library = load_library_releases_for_ids(&state, &release_ids).await?;
    let library_by_id = library
        .into_iter()
        .filter_map(|release| release.release.id.map(|id| (id, release)))
        .collect::<HashMap<_, _>>();
    let hashes = catalog_groups
        .iter()
        .flat_map(|group| &group.variants)
        .filter_map(|variant| variant.info_hash.clone())
        .collect::<Vec<_>>();
    let live = live_downloads_by_hash(&state, &hashes).await;
    let preferences = state.db.get_runtime_preferences().await?;
    for group in &mut catalog_groups {
        let library_release = group
            .release
            .id
            .and_then(|release_id| library_by_id.get(&release_id));
        if let Some(library_release) = library_release {
            group.library_availability = Some(library_release.availability);
            group.library_added_at = Some(library_release.added_at);
        }
        for variant in &mut group.variants {
            variant.downloads = variant
                .info_hash
                .as_ref()
                .and_then(|hash| live.get(&hash.to_ascii_lowercase()).cloned())
                .unwrap_or_default();
            variant.library = library_release.and_then(|release| {
                release
                    .variants
                    .iter()
                    .find(|library_variant| {
                        library_variant
                            .tracker
                            .eq_ignore_ascii_case(&variant.tracker)
                            && library_variant.torrent_id == variant.torrent_id
                    })
                    .and_then(|library_variant| library_variant.library.clone())
            });
            variant.eligibility = Some(preferences.release.eligibility(
                &variant.tracker,
                variant.format.as_deref(),
                variant.encoding.as_deref(),
                variant.media.as_deref(),
                variant.size,
                variant.leech_status,
                variant.can_use_token || !variant.token_eligibility_known,
            ));
        }
    }
    catalog_groups.sort_by(|left, right| {
        right
            .release
            .year
            .unwrap_or_default()
            .cmp(&left.release.year.unwrap_or_default())
            .then_with(|| left.release.title.cmp(&right.release.title))
    });
    let primary_count = catalog_groups
        .iter()
        .filter(|group| group.roles.contains(&ArtistCatalogRole::Primary))
        .count();
    let appearance_count = catalog_groups.len().saturating_sub(primary_count);
    let mut page = ArtistCatalogPage {
        artist: crate::model::ArtistCatalogArtist {
            id: Some(id),
            tracker: String::new(),
            artist_id: 0,
            name: artist.name.clone(),
            artwork: artist.artwork.clone(),
        },
        groups: catalog_groups,
        primary_count,
        appearance_count,
        deduplication: Default::default(),
    };
    enrich_artist_deduplication_batched(&state, &mut page, &preferences.release).await?;
    let fetched_at = source_provenance
        .iter()
        .filter_map(|source| source.fetched_at)
        .min();
    let stale = source_provenance
        .iter()
        .any(|source| source.state != SnapshotState::Fresh);
    Ok(Json(ApiEnvelope {
        data: page,
        provenance: Provenance {
            tracker: "canonical".into(),
            fetched_at,
            cache_age_seconds: fetched_at.map(|value| (Utc::now() - value).num_seconds().max(0)),
            stale,
            sources: source_provenance,
        },
    }))
}

async fn update_artist_metadata(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
    Json(overrides): Json<Value>,
) -> Result<StatusCode, AppError> {
    if state.db.set_artist_overrides(id, overrides).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found(
            "artist_not_found",
            "Artist was not found",
        ))
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
struct LibraryQuery {
    q: Option<String>,
    tracker: Option<String>,
    format: Option<String>,
    availability: Option<String>,
    sort: Option<String>,
    #[serde(default = "default_library_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_library_limit() -> usize {
    50
}

struct LibraryReleaseBuild {
    release: ReleaseSummary,
    variants: HashMap<(String, i64), (TorrentVariant, Vec<LibraryCopy>)>,
    release_copies: Vec<LibraryCopy>,
    added_at: chrono::DateTime<Utc>,
    fetched_at: chrono::DateTime<Utc>,
    stale: bool,
}

async fn load_library_releases(state: &AppState) -> Result<Vec<LibraryRelease>, AppError> {
    {
        let cache = state.library_cache.read().await;
        if let Some(cache) = cache.as_ref()
            && cache.loaded_at.elapsed() < Duration::from_secs(30)
        {
            return Ok(cache.releases.clone());
        }
    }
    let mut cache = state.library_cache.write().await;
    if let Some(cached) = cache.as_ref()
        && cached.loaded_at.elapsed() < Duration::from_secs(30)
    {
        return Ok(cached.releases.clone());
    }
    let records = state.db.list_library_records().await?;
    let mut releases = build_library_releases(records);
    let release_ids = releases
        .iter()
        .filter_map(|release| release.release.id)
        .collect::<Vec<_>>();
    let details = state.db.get_release_details(&release_ids).await?;
    for release in &mut releases {
        if let Some(id) = release.release.id
            && let Some(detail) = details.get(&id)
        {
            release.release = detail.release.clone();
            release.provenance.tracker = "canonical".into();
        }
    }
    enrich_release_coverages(state, &mut releases).await?;
    sort_library_releases(&mut releases, "year_desc");
    *cache = Some(LibraryCache {
        loaded_at: std::time::Instant::now(),
        releases: releases.clone(),
    });
    Ok(releases)
}

async fn load_library_releases_for_ids(
    state: &AppState,
    release_ids: &[uuid::Uuid],
) -> Result<Vec<LibraryRelease>, AppError> {
    let records = state
        .db
        .list_library_records_for_releases(release_ids)
        .await?;
    Ok(build_library_releases(records))
}

fn build_library_releases(records: Vec<crate::db::LibraryRecord>) -> Vec<LibraryRelease> {
    let mut groups: HashMap<String, LibraryReleaseBuild> = HashMap::new();
    for record in records {
        let tracker = record.release.value.tracker.clone();
        let group_id = record.release.value.group_id;
        let release_key = record
            .release
            .value
            .id
            .map(|id| id.to_string())
            .unwrap_or_else(|| format!("{tracker}:{group_id}"));
        let copy = LibraryCopy {
            client: record.client,
            info_hash: record.info_hash,
            present: record.present,
            completed_at: record.completed_at,
            last_seen_at: record.last_seen_at,
            missing_since: record.missing_since,
        };
        let entry = groups
            .entry(release_key)
            .or_insert_with(|| LibraryReleaseBuild {
                release: record.release.value.clone(),
                variants: HashMap::new(),
                release_copies: Vec::new(),
                added_at: record.library_added_at,
                fetched_at: record.release.fetched_at,
                stale: record.release.expires_at <= Utc::now(),
            });
        if entry.release.artists.is_empty() && !record.release.value.artists.is_empty() {
            entry.release = record.release.value.clone();
        }
        entry.added_at = entry.added_at.min(record.library_added_at);
        entry.fetched_at = entry.fetched_at.max(record.release.fetched_at);
        entry.stale |= record.release.expires_at <= Utc::now();
        if let Some(record_variant) = record.variant {
            let torrent_id = record_variant.torrent_id;
            let variant = entry
                .variants
                .entry((tracker, torrent_id))
                .or_insert_with(|| {
                    let mut variant = record_variant;
                    variant.downloads.clear();
                    variant.library = None;
                    (variant, Vec::new())
                });
            variant.1.push(copy);
        } else {
            entry.release_copies.push(copy);
        }
    }

    groups
        .into_values()
        .map(|mut group| {
            if group.release.artists.is_empty() {
                let display = group
                    .release
                    .artist
                    .clone()
                    .unwrap_or_else(|| "Unknown artist".to_owned());
                group.release.artists =
                    vec![fallback_artist_credit(&group.release.tracker, &display)];
            }
            let mut variants = group
                .variants
                .into_values()
                .map(|(mut variant, mut copies)| {
                    copies.sort_by(|left, right| {
                        right
                            .present
                            .cmp(&left.present)
                            .then_with(|| left.client.cmp(&right.client))
                    });
                    let availability = if copies.iter().any(|copy| copy.present) {
                        LibraryAvailability::Present
                    } else {
                        LibraryAvailability::Missing
                    };
                    variant.library = Some(LibraryVariantState {
                        availability,
                        copies,
                    });
                    variant
                })
                .collect::<Vec<_>>();
            variants.sort_by_key(|variant| variant.torrent_id);
            let present = variants
                .iter()
                .filter(|variant| {
                    variant
                        .library
                        .as_ref()
                        .is_some_and(|library| library.availability == LibraryAvailability::Present)
                })
                .count();
            let release_copy_present = group.release_copies.iter().any(|copy| copy.present);
            let availability = if release_copy_present || (present > 0 && present == variants.len())
            {
                LibraryAvailability::Present
            } else if present == 0 {
                LibraryAvailability::Missing
            } else {
                LibraryAvailability::Partial
            };
            LibraryRelease {
                provenance: provenance(&group.release.tracker, group.fetched_at, group.stale),
                release: group.release,
                variants,
                release_copies: group.release_copies,
                availability,
                added_at: group.added_at,
            }
        })
        .collect()
}

fn library_release_matches(release: &LibraryRelease, query: &LibraryQuery) -> bool {
    if query
        .tracker
        .as_deref()
        .filter(|value| !value.is_empty())
        .is_some_and(|tracker| {
            !release
                .release
                .sources
                .iter()
                .any(|source| source.tracker.eq_ignore_ascii_case(tracker))
        })
    {
        return false;
    }
    if query
        .format
        .as_deref()
        .filter(|value| !value.is_empty())
        .is_some_and(|format| {
            !release.variants.iter().any(|variant| {
                variant
                    .format
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(format))
            })
        })
    {
        return false;
    }
    if let Some(availability) = query
        .availability
        .as_deref()
        .filter(|value| !value.is_empty() && *value != "all")
    {
        let matches = matches!(
            (availability, release.availability),
            ("present", LibraryAvailability::Present)
                | ("partial", LibraryAvailability::Partial)
                | ("missing", LibraryAvailability::Missing)
        );
        if !matches {
            return false;
        }
    }
    true
}

fn artist_sort_name(name: &str) -> String {
    let normalized = name.trim().to_lowercase();
    normalized
        .strip_prefix("the ")
        .unwrap_or(&normalized)
        .to_owned()
}

fn is_compilation(release: &ReleaseSummary) -> bool {
    let primary_artist_count = release
        .artists
        .iter()
        .filter(|artist| artist.role == ArtistRole::Primary)
        .count();
    release.release_type.as_deref().is_some_and(|release_type| {
        matches!(
            release_type.trim().to_ascii_lowercase().as_str(),
            "compilation" | "sampler"
        )
    }) || primary_artist_count > 3
        || release.artist.as_deref().is_some_and(|artist| {
            matches!(
                artist.trim().to_ascii_lowercase().as_str(),
                "various" | "various artists" | "various artistes"
            )
        })
}

fn artist_summary(
    tracker: &str,
    key: &str,
    artist: &ArtistCredit,
    releases: &[&LibraryRelease],
) -> LibraryArtistSummary {
    let mut artworks = releases
        .iter()
        .filter_map(|release| release.release.artwork.clone())
        .collect::<Vec<_>>();
    let mut seen_artwork = HashSet::new();
    artworks.retain(|artwork| seen_artwork.insert(artwork.clone()));
    artworks.truncate(4);
    LibraryArtistSummary {
        id: artist.canonical_id,
        key: key.to_owned(),
        tracker: tracker.to_owned(),
        artist_id: artist.artist_id,
        credit_source: artist.source.clone(),
        name: artist.name.clone(),
        release_count: releases.len(),
        missing_count: releases
            .iter()
            .filter(|release| release.availability != LibraryAvailability::Present)
            .count(),
        artworks,
    }
}

fn build_artist_summaries<'a>(releases: &'a [LibraryRelease]) -> Vec<LibraryArtistSummary> {
    type ArtistGroupKey = (
        String,
        String,
        String,
        String,
        Option<i64>,
        Option<uuid::Uuid>,
        ArtistCreditSource,
    );
    let primary_artists = releases
        .iter()
        .filter(|release| !is_compilation(&release.release))
        .flat_map(|release| release.release.artists.iter())
        .filter(|artist| artist.role == ArtistRole::Primary)
        .map(|artist| {
            artist
                .canonical_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| {
                    format!("{}:{}", artist.tracker.to_ascii_lowercase(), artist.key)
                })
        })
        .collect::<HashSet<_>>();
    let mut grouped: HashMap<ArtistGroupKey, Vec<&'a LibraryRelease>> = HashMap::new();
    for release in releases
        .iter()
        .filter(|release| !is_compilation(&release.release))
    {
        let mut seen = HashSet::new();
        for artist in &release.release.artists {
            let identity = artist
                .canonical_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| {
                    format!("{}:{}", artist.tracker.to_ascii_lowercase(), artist.key)
                });
            if primary_artists.contains(&identity) && seen.insert(identity.clone()) {
                grouped
                    .entry((
                        identity,
                        artist.tracker.clone(),
                        artist.key.clone(),
                        artist.name.clone(),
                        artist.artist_id,
                        artist.canonical_id,
                        artist.source.clone(),
                    ))
                    .or_default()
                    .push(release);
            }
        }
    }
    let mut artists = grouped
        .into_iter()
        .map(
            |((_identity, tracker, key, name, artist_id, canonical_id, source), releases)| {
                let artist = ArtistCredit {
                    canonical_id,
                    key: key.clone(),
                    tracker: tracker.clone(),
                    artist_id,
                    name,
                    role: ArtistRole::Primary,
                    source,
                };
                artist_summary(&tracker, &key, &artist, &releases)
            },
        )
        .collect::<Vec<_>>();
    artists.sort_by(|left, right| {
        artist_sort_name(&left.name)
            .cmp(&artist_sort_name(&right.name))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.tracker.cmp(&right.tracker))
    });
    artists
}

fn sort_library_releases(releases: &mut [LibraryRelease], sort: &str) {
    match sort {
        "title" => releases.sort_by(|left, right| {
            left.release
                .title
                .to_lowercase()
                .cmp(&right.release.title.to_lowercase())
        }),
        "added_desc" => {
            releases.sort_by_key(|release| std::cmp::Reverse(release.added_at));
        }
        _ => releases.sort_by(|left, right| {
            right
                .release
                .year
                .unwrap_or_default()
                .cmp(&left.release.year.unwrap_or_default())
                .then_with(|| {
                    left.release
                        .title
                        .to_lowercase()
                        .cmp(&right.release.title.to_lowercase())
                })
        }),
    }
}

async fn library_index_status(
    state: &Arc<AppState>,
    releases: &[LibraryRelease],
) -> Result<LibraryIndexStatus, AppError> {
    let mut deduplication = DeduplicationIndexStatus::default();
    let single_keys = releases
        .iter()
        .filter(|release| {
            release
                .release
                .release_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        })
        .map(|release| (release.release.tracker.clone(), release.release.group_id))
        .collect::<Vec<_>>();
    let coverages = state.db.get_single_coverages(&single_keys).await?;
    let mut missing = Vec::new();
    let mut artists = HashSet::new();
    for release in releases.iter().filter(|release| {
        release
            .release
            .release_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
    }) {
        deduplication.total += 1;
        if release.release.album_coverage.is_some() {
            deduplication.checked += 1;
            deduplication.hidden += 1;
            continue;
        }
        let key = (release.release.tracker.clone(), release.release.group_id);
        let state_name = coverages.get(&key).map(|stored| stored.state.as_str());
        match state_name {
            Some("ready") => deduplication.checked += 1,
            Some("resolving") => deduplication.resolving += 1,
            Some("failed") => deduplication.failed += 1,
            _ => deduplication.pending += 1,
        }
        if state_name != Some("ready") {
            for artist in release
                .release
                .artists
                .iter()
                .filter(|artist| artist.role == ArtistRole::Primary)
            {
                let Some(artist_id) = artist.artist_id else {
                    continue;
                };
                let tracker = artist.tracker.to_ascii_lowercase();
                if state.trackers.contains_key(&tracker) {
                    artists.insert((tracker, artist_id));
                }
            }
        }
        if state_name.is_none() {
            missing.push(key);
        }
    }
    let artists = artists.into_iter().collect::<Vec<_>>();
    if !artists.is_empty() {
        state.db.ensure_artist_catalog_refreshes(&artists).await?;
    }
    if !missing.is_empty() {
        seed_single_deduplications(state, &missing).await?;
    } else if !artists.is_empty() {
        state.background_jobs.wake();
    }
    enrich_deduplication_queue_status(state, &mut deduplication).await?;
    Ok(LibraryIndexStatus {
        last_successful_scan_at: state.db.last_successful_download_scan().await?,
        unresolved_credits: releases
            .iter()
            .filter(|release| {
                release
                    .release
                    .artists
                    .iter()
                    .all(|artist| artist.source == ArtistCreditSource::DisplayFallback)
            })
            .count(),
        deduplication,
    })
}

#[utoipa::path(
    get, path = "/api/v1/library/artists", params(LibraryQuery),
    responses((status = 200, body = LibraryArtistsPage))
)]
async fn library_artists(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LibraryQuery>,
) -> Result<Json<LibraryArtistsPage>, AppError> {
    let all = load_library_releases(&state).await?;
    let filtered = all
        .into_iter()
        .filter(|release| library_release_matches(release, &query))
        .collect::<Vec<_>>();
    let needle = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let mut artists = build_artist_summaries(&filtered);
    if let Some(needle) = &needle {
        artists.retain(|artist| artist.name.to_lowercase().contains(needle));
    }
    let artist_total = artists.len();
    let limit = query.limit.clamp(1, 5_000);
    let offset = query.offset.min(100_000);
    let artists = artists.into_iter().skip(offset).take(limit).collect();

    let mut releases = if let Some(needle) = &needle {
        filtered
            .iter()
            .filter(|release| release.release.title.to_lowercase().contains(needle))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    sort_library_releases(&mut releases, query.sort.as_deref().unwrap_or("year_desc"));
    let release_total = releases.len();
    let releases = releases.into_iter().skip(offset).take(limit).collect();
    let index = library_index_status(&state, &filtered).await?;
    Ok(Json(LibraryArtistsPage {
        artists,
        releases,
        artist_total,
        release_total,
        index,
    }))
}

#[utoipa::path(
    get, path = "/api/v1/library/artists/{id}",
    params(
        ("id" = String, Path),
        LibraryQuery
    ),
    responses(
        (status = 200, body = LibraryArtistPage),
        (status = 404, description = "Library artist not found")
    )
)]
async fn library_artist(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
    Query(query): Query<LibraryQuery>,
) -> Result<Json<LibraryArtistPage>, AppError> {
    let all = load_library_releases(&state).await?;
    let artist_releases = all
        .iter()
        .filter(|release| !is_compilation(&release.release))
        .filter(|release| {
            release
                .release
                .artists
                .iter()
                .any(|artist| artist.canonical_id == Some(id))
        })
        .collect::<Vec<_>>();
    let artist = artist_releases
        .iter()
        .find_map(|release| {
            release.release.artists.iter().find(|artist| {
                artist.canonical_id == Some(id) && artist.role == ArtistRole::Primary
            })
        })
        .ok_or_else(|| AppError::not_found("artist_not_found", "Library artist not found"))?;
    let summary = artist_summary(&artist.tracker, &artist.key, artist, &artist_releases);
    let needle = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let mut items = artist_releases
        .into_iter()
        .filter(|release| library_release_matches(release, &query))
        .filter(|release| {
            needle
                .as_ref()
                .is_none_or(|needle| release.release.title.to_lowercase().contains(needle))
        })
        .cloned()
        .collect::<Vec<_>>();
    sort_library_releases(&mut items, query.sort.as_deref().unwrap_or("year_desc"));
    let total = items.len();
    let limit = query.limit.clamp(1, 5_000);
    let offset = query.offset.min(100_000);
    let items = items.into_iter().skip(offset).take(limit).collect();
    let index = library_index_status(&state, &all).await?;
    Ok(Json(LibraryArtistPage {
        artist: summary,
        items,
        total,
        index,
    }))
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
    responses((status = 200, body = DownloadsPage))
)]
async fn downloads(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DownloadsQuery>,
) -> Result<Json<DownloadsPage>, AppError> {
    let limit = query.limit.clamp(1, 500);
    let offset = query.offset.min(10_000);
    if let Some(name) = query.client.as_deref()
        && !state.download_clients.contains_key(name)
    {
        return Err(AppError::bad_request(
            "unknown_download_client",
            "Unknown download client",
        ));
    }
    let (indexed, total) = state
        .db
        .list_indexed_downloads(query.client.as_deref(), u64::from(limit), u64::from(offset))
        .await?;
    let mut hashes_by_client: HashMap<String, Vec<String>> = HashMap::new();
    for download in &indexed {
        hashes_by_client
            .entry(download.client.clone())
            .or_default()
            .push(download.info_hash.clone());
    }
    let mut refreshed = HashMap::new();
    let mut observations = Vec::new();
    for (name, hashes) in hashes_by_client {
        let Some(client) = state.download_clients.get(&name) else {
            continue;
        };
        match tokio::time::timeout(
            Duration::from_secs(5),
            client.downloads_by_hashes_with_class(&hashes, RequestClass::Interactive),
        )
        .await
        {
            Ok(Ok(downloads)) => {
                for download in downloads {
                    observations.push(DownloadObservation {
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
                    });
                    refreshed.insert(
                        (name.clone(), download.live.info_hash.clone()),
                        download.live,
                    );
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(client = %name, %error, "using cached download status");
            }
            Err(_) => {
                tracing::warn!(client = %name, "download status refresh timed out; using cache")
            }
        }
    }
    for chunk in observations.chunks(100) {
        if let Err(error) = state.db.observe_downloads(chunk).await {
            tracing::warn!(%error, "could not persist refreshed download page");
            break;
        }
    }
    if !observations.is_empty() {
        state.background_jobs.wake();
    }
    let release_ids = indexed
        .iter()
        .filter_map(|download| download.release.value.id)
        .collect::<Vec<_>>();
    let release_details = state.db.get_release_details(&release_ids).await?;
    let mut items = Vec::new();
    for indexed_download in indexed {
        let refreshed_live = refreshed.remove(&(
            indexed_download.client.clone(),
            indexed_download.info_hash.clone(),
        ));
        let live_stale = refreshed_live.is_none();
        let Some(live) = refreshed_live.or(indexed_download.live) else {
            continue;
        };
        let cached_release = indexed_download.release;
        let stale = cached_release.expires_at <= Utc::now();
        if stale && indexed_download.variant.is_some() {
            background::enqueue_hash_resolution(
                &state,
                &cached_release.value.tracker,
                &indexed_download.info_hash,
            )
            .await?;
        }
        let variant = indexed_download.variant.map(|mut variant| {
            variant.downloads = vec![live.clone()];
            variant
        });
        let release = match cached_release.value.id {
            Some(id) => release_details
                .get(&id)
                .map(|detail| detail.release.clone())
                .unwrap_or(cached_release.value),
            None => cached_release.value,
        };
        items.push(CanonicalDownload {
            release,
            variant,
            download: live,
            provenance: provenance("canonical", cached_release.fetched_at, stale),
            live_observed_at: if live_stale {
                indexed_download.observed_at
            } else {
                Some(Utc::now())
            },
            live_stale,
        });
    }
    Ok(Json(DownloadsPage {
        items,
        total,
        index: state.db.index_counts().await?,
    }))
}

#[utoipa::path(
    get, path = "/api/v1/imports", params(DownloadsQuery),
    responses((status = 200, body = ImportsPage))
)]
async fn imports(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DownloadsQuery>,
) -> Result<Json<ImportsPage>, AppError> {
    let mut page = state
        .db
        .list_imports(
            u64::from(query.limit.clamp(1, 500)),
            u64::from(query.offset.min(10_000)),
        )
        .await?;
    let hashes = page
        .items
        .iter()
        .flat_map(|task| {
            task.info_hash.iter().cloned().chain(
                task.supersessions
                    .iter()
                    .map(|source| source.source_info_hash.clone()),
            )
        })
        .collect::<Vec<_>>();
    let live = live_downloads_by_hash(&state, &hashes).await;
    for task in &mut page.items {
        if let (Some(client), Some(info_hash)) = (&task.client, &task.info_hash) {
            task.download = live
                .get(&info_hash.to_ascii_lowercase())
                .and_then(|downloads| downloads.iter().find(|download| download.client == *client))
                .cloned()
                .or_else(|| task.download.clone());
        }
        for source in &mut task.supersessions {
            source.download = live
                .get(&source.source_info_hash.to_ascii_lowercase())
                .and_then(|downloads| {
                    downloads
                        .iter()
                        .find(|download| download.client == source.source_client)
                })
                .cloned()
                .or_else(|| source.download.clone());
        }
    }
    Ok(Json(page))
}

async fn retry_import(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    if state.db.import_task_models(id).await?.is_none() {
        return Err(AppError::not_found(
            "import_not_found",
            "Import task was not found",
        ));
    }
    state
        .db
        .set_import_state(id, ImportTaskState::Ready, Some("Retry requested"), None)
        .await?;
    let key = format!("process-import:{id}:v1");
    if !state.db.retry_background_job_by_key(&key).await? {
        background::enqueue_import_processing(&state, id).await?;
    } else {
        state.background_jobs.wake();
    }
    Ok(StatusCode::ACCEPTED)
}

async fn dismiss_import(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    if state.db.import_task_models(id).await?.is_none() {
        return Err(AppError::not_found(
            "import_not_found",
            "Import task was not found",
        ));
    }
    state
        .db
        .set_import_state(
            id,
            ImportTaskState::Dismissed,
            Some("Dismissed by the user"),
            None,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn retry_download_link(
    State(state): State<Arc<AppState>>,
    Path((client_name, info_hash)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    if !state.download_clients.contains_key(&client_name) {
        return Err(AppError::bad_request(
            "unknown_download_client",
            "Unknown download client",
        ));
    }
    if !state.db.retry_link(&client_name, &info_hash).await? {
        return Err(AppError::bad_request(
            "resolution_not_retryable",
            "This torrent does not have a failed configured-tracker resolution",
        ));
    }
    if let Some(link) = state.db.get_link(&client_name, &info_hash).await?
        && let Some(tracker) = link.tracker
    {
        background::retry_hash_resolution(&state, &tracker, &info_hash).await?;
    }
    Ok(StatusCode::ACCEPTED)
}

#[utoipa::path(
    get, path = "/api/v1/downloads/{client}/{info_hash}",
    params(("client" = String, Path), ("info_hash" = String, Path)),
    responses(
        (status = 200, body = CanonicalDownload),
        (status = 409, description = "Release has not been resolved")
    )
)]
async fn download_detail_compatibility(
    State(state): State<Arc<AppState>>,
    Path((client_name, info_hash)): Path<(String, String)>,
) -> Result<Json<CanonicalDownload>, AppError> {
    let link = state
        .db
        .get_link(&client_name, &info_hash)
        .await?
        .ok_or_else(|| {
            AppError::not_found("download_not_found", "Download was not found in the index")
        })?;
    if link.resolution_state != "linked" {
        return Err(AppError::conflict(
            "release_unresolved",
            "This download has not yet been linked to a canonical release",
        ));
    }
    let (Some(tracker), Some(torrent_id)) = (link.tracker.as_deref(), link.torrent_id) else {
        return Err(AppError::conflict(
            "release_unresolved",
            "This download has no resolved source variant",
        ));
    };
    let canonical = state
        .db
        .get_canonical(tracker, torrent_id)
        .await?
        .ok_or_else(|| {
            AppError::conflict(
                "release_unresolved",
                "Canonical metadata is still being indexed",
            )
        })?;
    let client = state.download_clients.get(&client_name).ok_or_else(|| {
        AppError::not_found("download_client_not_found", "Download client was not found")
    })?;
    let live = client
        .downloads_by_hashes(&[info_hash.to_ascii_lowercase()])
        .await?
        .into_iter()
        .next()
        .map(|download| download.live)
        .ok_or_else(|| AppError::not_found("download_not_found", "Download was not found"))?;
    let mut variant = canonical.value.variant;
    variant.downloads = vec![live.clone()];
    let release = match canonical.value.release.id {
        Some(id) => state
            .db
            .get_release_detail(id)
            .await?
            .map(|detail| detail.release)
            .unwrap_or(canonical.value.release),
        None => canonical.value.release,
    };
    Ok(Json(CanonicalDownload {
        release,
        variant: Some(variant),
        download: live,
        provenance: provenance(
            "canonical",
            canonical.fetched_at,
            canonical.expires_at <= Utc::now(),
        ),
        live_observed_at: Some(Utc::now()),
        live_stale: false,
    }))
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
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok());
    let job = enqueue_download(state, request, idempotency_key).await?;
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn enqueue_download(
    state: Arc<AppState>,
    request: CreateDownload,
    idempotency_key: Option<&str>,
) -> Result<DownloadJob, AppError> {
    get_tracker(&state, Some(&request.tracker))?;
    if !state.profiles.contains_key(&request.profile) {
        return Err(AppError::bad_request(
            "unknown_download_profile",
            "Unknown download profile",
        ));
    }
    let (job, _created) = state
        .db
        .create_job(
            &request.tracker,
            request.torrent_id,
            &request.profile,
            request.use_token,
            idempotency_key,
        )
        .await?;
    state.background_jobs.wake();
    Ok(job)
}

#[utoipa::path(
    get, path = "/api/v1/download-jobs/{id}",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = DownloadJob))
)]
async fn download_job(
    State(state): State<Arc<AppState>>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<DownloadJob>, AppError> {
    state
        .db
        .get_job(id)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found("download_job_not_found", "Download job not found"))
}

fn download_stage_path(state: &AppState, job_id: Uuid) -> PathBuf {
    state.download_staging_dir.join(format!("{job_id}.torrent"))
}

fn write_staged_download(path: &StdPath, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("download staging path has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("persist staged torrent {}", path.display()))?;
    Ok(())
}

pub(crate) async fn cleanup_download_stage(state: &AppState, job_id: Uuid) {
    let path = download_stage_path(state, job_id);
    if let Err(error) = tokio::fs::remove_file(&path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(job_id = %job_id, %error, path = %path.display(), "could not remove staged torrent");
    }
}

async fn cleanup_orphaned_download_stages(state: &AppState) -> Result<()> {
    let incomplete = state
        .db
        .list_jobs()
        .await?
        .into_iter()
        .filter(|job| {
            matches!(
                job.state,
                DownloadState::Queued | DownloadState::FetchingMetadata | DownloadState::Submitting
            )
        })
        .map(|job| job.id)
        .collect::<HashSet<_>>();
    let mut entries = tokio::fs::read_dir(&state.download_staging_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let job_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| Uuid::parse_str(value).ok());
        if job_id.is_none_or(|job_id| !incomplete.contains(&job_id)) {
            tokio::fs::remove_file(&path)
                .await
                .with_context(|| format!("remove orphaned download stage {}", path.display()))?;
        }
    }
    Ok(())
}

pub(crate) async fn process_download(state: Arc<AppState>, job: DownloadJob) -> Result<()> {
    let profile = state
        .profiles
        .get(&job.profile)
        .ok_or_else(|| anyhow!("download profile disappeared from configuration"))?;
    let client = state
        .download_clients
        .get(&profile.client)
        .ok_or_else(|| anyhow!("download client disappeared from configuration"))?;
    let stage_path = download_stage_path(&state, job.id);
    let staged_payload = match tokio::fs::read(&stage_path).await {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("read staged torrent"),
    };

    if let Some(info_hash) = job.info_hash.as_deref() {
        if let Some(existing) = client
            .download_with_class(info_hash, RequestClass::Download)
            .await?
        {
            let state_value = if existing.live.progress >= 1.0 {
                DownloadState::Complete
            } else {
                DownloadState::Active
            };
            state
                .db
                .update_progress(
                    job.id,
                    state_value,
                    existing.live.progress,
                    existing.live.download_speed,
                    existing.live.upload_speed,
                    existing.live.eta,
                )
                .await?;
            cleanup_download_stage(&state, job.id).await;
            return Ok(());
        }
        if let Some(bytes) = staged_payload.as_ref() {
            let file_name = format!("{}-{}.torrent", job.tracker, job.torrent_id);
            client
                .add_torrent(bytes.clone(), &file_name, profile)
                .await?;
            state
                .db
                .set_job_state(job.id, DownloadState::Active, None)
                .await?;
            cleanup_download_stage(&state, job.id).await;
            return Ok(());
        }
    }

    state
        .db
        .set_job_state(job.id, DownloadState::FetchingMetadata, None)
        .await?;
    let tracker = state
        .trackers
        .get(&job.tracker)
        .ok_or_else(|| anyhow!("tracker disappeared from configuration"))?;

    let (mut metadata, mut canonical, raw) = tracker
        .torrent_with_class(job.torrent_id, RequestClass::Download)
        .await?;
    let preferences = state.db.get_runtime_preferences().await?;
    let token_available_or_unknown = metadata.can_use_token || !metadata.token_eligibility_known;
    let eligibility = preferences.release.eligibility(
        &job.tracker,
        canonical.variant.format.as_deref(),
        canonical.variant.encoding.as_deref(),
        canonical.variant.media.as_deref(),
        canonical.variant.size,
        canonical.variant.leech_status,
        token_available_or_unknown,
    );
    if !eligibility.eligible {
        let message = match eligibility.reason {
            crate::model::DownloadEligibilityReason::BelowQualityCutoff => format!(
                "release quality '{}' is below the configured cutoff",
                crate::model::ReleasePreferences::quality_class(
                    canonical.variant.format.as_deref(),
                    canonical.variant.encoding.as_deref(),
                )
            ),
            crate::model::DownloadEligibilityReason::BelowMediaCutoff => format!(
                "release media '{}' is below the configured cutoff",
                canonical.variant.media.as_deref().unwrap_or("Other")
            ),
            crate::model::DownloadEligibilityReason::TrackerDisabled => {
                format!("downloads from {} are disabled by preferences", job.tracker)
            }
            crate::model::DownloadEligibilityReason::FreeleechRequired => {
                format!(
                    "{} is configured for already-free torrents only",
                    job.tracker
                )
            }
            crate::model::DownloadEligibilityReason::TokenUnavailable => {
                "a freeleech token is required but unavailable for this torrent".into()
            }
            crate::model::DownloadEligibilityReason::TokenCostUnknown => {
                "an OPS freeleech token was requested, but the torrent size is unavailable".into()
            }
            crate::model::DownloadEligibilityReason::Eligible => {
                "torrent is not eligible under the configured tracker policy".into()
            }
        };
        return Err(anyhow!(message));
    }
    if eligibility.requires_token && !job.use_token {
        return Err(anyhow!(
            "a freeleech token is required by the configured tracker policy"
        ));
    }
    if job.use_token && !eligibility.requires_token {
        let policy = preferences.release.tracker_policy(&job.tracker);
        if matches!(
            policy.mode,
            crate::model::TrackerDownloadMode::Disabled
                | crate::model::TrackerDownloadMode::FreeleechOnly
        ) {
            return Err(anyhow!(
                "freeleech token use is disabled by the configured tracker policy"
            ));
        }
    }
    if job.use_token && eligibility.token_cost.is_none() {
        return Err(anyhow!(
            "freeleech token cost is unknown because the tracker has no supported cost model or the torrent size is unavailable"
        ));
    }
    if job.use_token && metadata.token_eligibility_known && !metadata.can_use_token {
        return Err(anyhow!("torrent is not eligible for a freeleech token"));
    }
    let mut payload = staged_payload;
    let mut submission_started = false;
    let info_hash = if let Some(info_hash) = metadata
        .info_hash
        .clone()
        .or_else(|| canonical.variant.info_hash.clone())
    {
        info_hash.to_ascii_lowercase()
    } else if let Some(bytes) = payload.as_ref() {
        torrent_info_hash(bytes)?
    } else {
        state
            .db
            .set_job_state(job.id, DownloadState::Submitting, None)
            .await?;
        submission_started = true;
        let bytes = tracker.download_torrent(job.torrent_id, false).await?;
        let info_hash = torrent_info_hash(&bytes)?;
        if !job.use_token {
            payload = Some(bytes);
        }
        info_hash
    };
    metadata.info_hash = Some(info_hash.clone());
    canonical.variant.info_hash = Some(info_hash.clone());
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
        .put_canonical(
            &canonical,
            Utc::now(),
            Utc::now() + ChronoDuration::hours(24),
        )
        .await?;
    state
        .db
        .update_job_metadata(job.id, metadata.group_id, &info_hash, &metadata.name)
        .await?;
    state
        .db
        .seed_download_link(
            &profile.client,
            &info_hash,
            &job.tracker,
            metadata.group_id,
            metadata.torrent_id,
            true,
        )
        .await?;

    if let Some(existing) = client
        .download_with_class(&info_hash, RequestClass::Download)
        .await?
    {
        let state_value = if existing.live.progress >= 1.0 {
            DownloadState::Complete
        } else {
            DownloadState::Active
        };
        state
            .db
            .update_progress(
                job.id,
                state_value,
                existing.live.progress,
                existing.live.download_speed,
                existing.live.upload_speed,
                existing.live.eta,
            )
            .await?;
        cleanup_download_stage(&state, job.id).await;
        return Ok(());
    }

    let bytes = if let Some(payload) = payload {
        payload
    } else {
        if !submission_started {
            state
                .db
                .set_job_state(job.id, DownloadState::Submitting, None)
                .await?;
        }
        tracker
            .download_torrent(job.torrent_id, job.use_token)
            .await?
    };
    if !stage_path.exists() {
        write_staged_download(&stage_path, &bytes)?;
    }
    let file_name = format!("{}-{}.torrent", job.tracker, job.torrent_id);
    client.add_torrent(bytes, &file_name, profile).await?;
    state
        .db
        .set_job_state(job.id, DownloadState::Active, None)
        .await?;
    cleanup_download_stage(&state, job.id).await;
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
            let mut unavailable_clients = HashSet::new();
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
                if unavailable_clients.contains(&profile.client) {
                    continue;
                }
                match client
                    .download_with_class(info_hash, RequestClass::Background)
                    .await
                {
                    Ok(Some(status)) => {
                        let next = if status.live.progress >= 1.0 {
                            DownloadState::Complete
                        } else {
                            DownloadState::Active
                        };
                        let _ = state
                            .db
                            .update_progress(
                                job.id,
                                next,
                                status.live.progress,
                                status.live.download_speed,
                                status.live.upload_speed,
                                status.live.eta,
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
                        if is_provider_unavailable(&error) {
                            unavailable_clients.insert(profile.client.clone());
                        }
                        tracing::warn!(job_id = %job.id, %error, "qBittorrent reconciliation failed")
                    }
                }
            }
        }
    });
}

pub fn spawn_channel_scheduler(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let channels = match state.db.list_channels().await {
                Ok(channels) => channels,
                Err(error) => {
                    tracing::warn!(%error, "channel scheduler could not load configuration");
                    continue;
                }
            };
            for config in channels {
                let due = channel::channel_is_due(&config, Utc::now()).unwrap_or(false);
                if !due
                    || state
                        .db
                        .active_channel_run(&config.id)
                        .await
                        .ok()
                        .flatten()
                        .is_some()
                {
                    continue;
                }
                if let Err(error) =
                    start_channel_run(state.clone(), config, ChannelRunTrigger::Scheduled).await
                {
                    tracing::warn!(
                        message = %error.body.error.message,
                        "scheduled channel refresh could not start"
                    );
                }
            }
        }
    });
}

async fn seed_catalog_deduplication(
    state: &AppState,
    tracker: &str,
    catalog: &ArtistCatalogPage,
) -> Result<()> {
    state
        .db
        .replace_catalog_memberships(tracker, catalog)
        .await?;
    for group in catalog.groups.iter().filter(|group| {
        group.roles.contains(&ArtistCatalogRole::Primary)
            && group.listed_on_tracker
            && !group.variants.is_empty()
            && group.release.release_type.as_deref().is_some_and(|kind| {
                kind.eq_ignore_ascii_case("single") || kind.eq_ignore_ascii_case("album")
            })
    }) {
        let is_album = group
            .release
            .release_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("album"));
        state
            .db
            .enqueue_track_index_with_priority(
                tracker,
                group.release.group_id,
                if is_album { 20 } else { 0 },
            )
            .await?;
        if group
            .release
            .release_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        {
            state
                .db
                .ensure_single_coverage(tracker, group.release.group_id)
                .await?;
        }
    }
    state.background_jobs.wake();
    Ok(())
}

async fn seed_single_deduplication(state: &AppState, tracker: &str, group_id: i64) -> Result<()> {
    seed_single_deduplications(state, &[(tracker.to_owned(), group_id)]).await
}

async fn seed_single_deduplications(state: &AppState, singles: &[(String, i64)]) -> Result<()> {
    state.db.seed_single_deduplications(singles).await?;
    state.background_jobs.wake();
    Ok(())
}

async fn observe_download(
    state: &AppState,
    download: &crate::model::ObservedDownload,
) -> Result<()> {
    let tracker = download
        .announce_host
        .as_ref()
        .and_then(|host| state.announce_hosts.get(host));
    let plex_target = state
        .plex
        .as_ref()
        .and_then(|plex| plex.target_for_path(&download.live.save_path));
    state
        .db
        .observe_download(
            &download.live,
            download.announce_host.as_deref(),
            tracker.map(String::as_str),
            plex_target.as_ref(),
        )
        .await?;
    state.background_jobs.wake();
    Ok(())
}

async fn seed_existing_job_links(state: &Arc<AppState>) -> Result<()> {
    for job in state.db.list_jobs().await? {
        let (Some(info_hash), Some(profile)) =
            (job.info_hash.as_deref(), state.profiles.get(&job.profile))
        else {
            continue;
        };
        let linked = state
            .db
            .get_canonical(&job.tracker, job.torrent_id)
            .await?
            .is_some();
        state
            .db
            .seed_download_link(
                &profile.client,
                info_hash,
                &job.tracker,
                job.group_id,
                job.torrent_id,
                linked,
            )
            .await?;
    }
    Ok(())
}

pub(crate) async fn cache_search_canonical(
    db: &Database,
    tracker: &str,
    page: &SearchPage,
) -> Result<()> {
    let now = Utc::now();
    for group in &page.groups {
        let release = ReleaseSummary {
            id: group.id,
            tracker: tracker.to_owned(),
            group_id: group.group_id,
            title: group.name.clone(),
            artist: group.artist.clone(),
            artists: group
                .artist
                .as_deref()
                .map(|artist| vec![fallback_artist_credit(tracker, artist)])
                .unwrap_or_default(),
            year: group.year,
            artwork: group.image.clone(),
            release_type: group.release_type.clone(),
            sources: vec![crate::model::ReleaseSource {
                tracker: tracker.to_owned(),
                group_id: group.group_id,
                match_score: 1.0,
            }],
            album_coverage: None,
        };
        for torrent in &group.torrents {
            let canonical = CanonicalTorrent {
                release: release.clone(),
                variant: TorrentVariant {
                    tracker: tracker.to_owned(),
                    torrent_id: torrent.torrent_id,
                    group_id: group.group_id,
                    info_hash: torrent.info_hash.clone(),
                    format: torrent.format.clone(),
                    encoding: torrent.encoding.clone(),
                    media: torrent.media.clone(),
                    size: torrent.size,
                    seeders: torrent.seeders,
                    leechers: torrent.leechers,
                    snatched: torrent.snatched,
                    freeleech: torrent.freeleech,
                    leech_status: torrent.leech_status,
                    can_use_token: torrent.can_use_token,
                    token_eligibility_known: true,
                    eligibility: None,
                    remaster_title: torrent.remaster_title.clone(),
                    downloads: Vec::new(),
                    library: None,
                },
                tags: group.tags.clone(),
                description: None,
                record_label: None,
            };
            db.put_canonical(&canonical, now, now + ChronoDuration::hours(24))
                .await?;
        }
    }
    Ok(())
}

async fn cache_release_detail(db: &Database, detail: &ReleaseDetail) -> Result<()> {
    let now = Utc::now();
    for variant in &detail.variants {
        db.put_canonical(
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
    Ok(())
}

async fn cache_artist_catalog(db: &Database, catalog: &ArtistCatalogPage) -> Result<()> {
    let now = Utc::now();
    for group in &catalog.groups {
        if group.variants.is_empty() {
            db.put_release_summary(&group.release, now, now + ChronoDuration::hours(24))
                .await?;
        }
        for variant in &group.variants {
            db.put_canonical(
                &CanonicalTorrent {
                    release: group.release.clone(),
                    variant: variant.clone(),
                    tags: group.tags.clone(),
                    description: None,
                    record_label: None,
                },
                now,
                now + ChronoDuration::hours(24),
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(dead_code)]
async fn enrich_artist_catalog(
    state: &Arc<AppState>,
    tracker: &str,
    artist_id: i64,
    catalog: &mut ArtistCatalogPage,
) -> Result<(), AppError> {
    catalog
        .groups
        .retain(|group| !is_compilation(&group.release));
    let library = load_library_releases(state)
        .await?
        .into_iter()
        .filter(|release| {
            !is_compilation(&release.release)
                && release.release.tracker.eq_ignore_ascii_case(tracker)
                && release
                    .release
                    .artists
                    .iter()
                    .any(|artist| artist.artist_id == Some(artist_id))
        })
        .collect::<Vec<_>>();
    let canonical = state
        .db
        .list_canonical_for_tracker(tracker)
        .await?
        .into_iter()
        .map(|item| (item.variant.torrent_id, item))
        .collect::<HashMap<_, _>>();
    let library_by_group = library
        .iter()
        .map(|release| (release.release.group_id, release))
        .collect::<HashMap<_, _>>();

    for group in &mut catalog.groups {
        if let Some(library_release) = library_by_group.get(&group.release.group_id) {
            group.library_availability = Some(library_release.availability);
            group.library_added_at = Some(library_release.added_at);
        }
        for variant in &mut group.variants {
            if let Some(known) = canonical.get(&variant.torrent_id) {
                if variant.info_hash.is_none() {
                    variant.info_hash = known.variant.info_hash.clone();
                }
                if !variant.token_eligibility_known && known.variant.token_eligibility_known {
                    variant.can_use_token = known.variant.can_use_token;
                    variant.token_eligibility_known = true;
                }
            }
            if let Some(library_variant) =
                library_by_group
                    .get(&group.release.group_id)
                    .and_then(|release| {
                        release
                            .variants
                            .iter()
                            .find(|item| item.torrent_id == variant.torrent_id)
                    })
            {
                if variant.info_hash.is_none() {
                    variant.info_hash = library_variant.info_hash.clone();
                }
                variant.library = library_variant.library.clone();
            }
        }
        if let Some(library_release) = library_by_group.get(&group.release.group_id) {
            for variant in &library_release.variants {
                if !group
                    .variants
                    .iter()
                    .any(|item| item.torrent_id == variant.torrent_id)
                {
                    group.variants.push(variant.clone());
                }
            }
        }
    }

    let catalog_group_ids = catalog
        .groups
        .iter()
        .map(|group| group.release.group_id)
        .collect::<HashSet<_>>();
    for release in library {
        if catalog_group_ids.contains(&release.release.group_id) {
            continue;
        }
        let roles = release
            .release
            .artists
            .iter()
            .find(|artist| artist.artist_id == Some(artist_id))
            .map(|artist| match artist.role {
                ArtistRole::Primary => vec![ArtistCatalogRole::Primary],
                ArtistRole::Guest => vec![ArtistCatalogRole::Guest],
            })
            .unwrap_or_else(|| vec![ArtistCatalogRole::Guest]);
        catalog.groups.push(ArtistCatalogRelease {
            release: release.release,
            tags: Vec::new(),
            variants: release.variants,
            roles,
            listed_on_tracker: false,
            library_availability: Some(release.availability),
            library_added_at: Some(release.added_at),
        });
    }

    let variants = catalog
        .groups
        .iter_mut()
        .flat_map(|group| group.variants.iter_mut())
        .collect::<Vec<_>>();
    let hashes = variants
        .iter()
        .filter_map(|variant| variant.info_hash.clone())
        .collect::<Vec<_>>();
    let live = live_downloads_by_hash(state, &hashes).await;
    for variant in variants {
        variant.downloads = variant
            .info_hash
            .as_ref()
            .and_then(|hash| live.get(&hash.to_ascii_lowercase()).cloned())
            .unwrap_or_default();
    }
    catalog.groups.sort_by(|left, right| {
        right
            .release
            .year
            .unwrap_or_default()
            .cmp(&left.release.year.unwrap_or_default())
            .then_with(|| {
                left.release
                    .title
                    .to_lowercase()
                    .cmp(&right.release.title.to_lowercase())
            })
    });
    catalog.primary_count = catalog
        .groups
        .iter()
        .filter(|group| group.roles.contains(&ArtistCatalogRole::Primary))
        .count();
    catalog.appearance_count = catalog.groups.len().saturating_sub(catalog.primary_count);
    seed_catalog_deduplication(state, tracker, catalog).await?;
    let preferences = state.db.get_runtime_preferences().await?;
    let mut status = DeduplicationIndexStatus::default();
    for group in &mut catalog.groups {
        if group
            .release
            .release_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        {
            annotate_single_coverage(
                state,
                tracker,
                group.release.group_id,
                &preferences.release,
                &mut group.release.album_coverage,
                &mut status,
            )
            .await?;
        }
    }
    enrich_deduplication_queue_status(state, &mut status).await?;
    catalog.deduplication = status;
    Ok(())
}

async fn enrich_release_coverages(
    state: &AppState,
    releases: &mut [LibraryRelease],
) -> Result<(), AppError> {
    let preferences = state.db.get_runtime_preferences().await?;
    let single_keys = releases
        .iter()
        .filter(|release| {
            release
                .release
                .release_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        })
        .map(|release| (release.release.tracker.clone(), release.release.group_id))
        .collect::<Vec<_>>();
    let coverages = state.db.get_single_coverages(&single_keys).await?;
    for release in releases {
        if release
            .release
            .release_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        {
            release.release.album_coverage = coverages
                .get(&(release.release.tracker.clone(), release.release.group_id))
                .and_then(|stored| {
                    (stored.state == "ready")
                        .then(|| stored.coverage.clone())
                        .flatten()
                })
                .and_then(|coverage| coverage.resolve(&preferences.release));
        }
    }
    Ok(())
}

async fn enrich_search_deduplication(
    state: &AppState,
    tracker: &str,
    page: &mut SearchPage,
) -> Result<(), AppError> {
    let preferences = state.db.get_runtime_preferences().await?;
    let mut status = DeduplicationIndexStatus::default();
    for group in &mut page.groups {
        if group
            .release_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        {
            annotate_single_coverage(
                state,
                tracker,
                group.group_id,
                &preferences.release,
                &mut group.album_coverage,
                &mut status,
            )
            .await?;
        }
    }
    enrich_deduplication_queue_status(state, &mut status).await?;
    page.deduplication = status;
    Ok(())
}

async fn enrich_artist_deduplication_batched(
    state: &Arc<AppState>,
    catalog: &mut ArtistCatalogPage,
    preferences: &crate::model::ReleasePreferences,
) -> Result<(), AppError> {
    let singles = catalog
        .groups
        .iter()
        .filter(|group| {
            group
                .release
                .release_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        })
        .map(|group| {
            (
                group.release.tracker.to_ascii_lowercase(),
                group.release.group_id,
            )
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let coverages = state.db.get_single_coverages(&singles).await?;
    let mut status = DeduplicationIndexStatus::default();
    let mut missing = Vec::new();
    for group in &mut catalog.groups {
        if !group
            .release
            .release_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        {
            continue;
        }
        status.total += 1;
        let key = (
            group.release.tracker.to_ascii_lowercase(),
            group.release.group_id,
        );
        let Some(stored) = coverages.get(&key) else {
            status.pending += 1;
            missing.push(key);
            continue;
        };
        match stored.state.as_str() {
            "ready" => {
                status.checked += 1;
                group.release.album_coverage = stored
                    .coverage
                    .as_ref()
                    .and_then(|coverage| coverage.resolve(preferences));
                if group.release.album_coverage.is_some() {
                    status.hidden += 1;
                }
            }
            "resolving" => status.resolving += 1,
            "failed" => status.failed += 1,
            _ => status.pending += 1,
        }
    }
    if !missing.is_empty() {
        seed_single_deduplications(state, &missing).await?;
    }
    enrich_deduplication_queue_status(state, &mut status).await?;
    catalog.deduplication = status;
    Ok(())
}

async fn enrich_deduplication_queue_status(
    state: &AppState,
    status: &mut DeduplicationIndexStatus,
) -> Result<(), AppError> {
    let progress = state.db.track_index_progress().await?;
    status.tracklists_indexed = progress.indexed;
    status.tracklists_pending = progress.pending;
    status.tracklists_resolving = progress.resolving;
    status.tracklists_failed = progress.failed;
    status.tracklists_total =
        progress.indexed + progress.pending + progress.resolving + progress.failed;
    Ok(())
}

async fn annotate_single_coverage(
    state: &AppState,
    tracker: &str,
    group_id: i64,
    preferences: &crate::model::ReleasePreferences,
    target: &mut Option<crate::model::AlbumCoverage>,
    status: &mut DeduplicationIndexStatus,
) -> Result<(), AppError> {
    status.total += 1;
    seed_single_deduplication(state, tracker, group_id).await?;
    let Some(stored) = state.db.get_single_coverage(tracker, group_id).await? else {
        status.pending += 1;
        return Ok(());
    };
    match stored.state.as_str() {
        "ready" => {
            status.checked += 1;
            *target = stored
                .coverage
                .and_then(|coverage| coverage.resolve(preferences));
            if target.is_some() {
                status.hidden += 1;
            }
        }
        "resolving" => status.resolving += 1,
        "failed" => status.failed += 1,
        _ => status.pending += 1,
    }
    Ok(())
}

async fn enrich_search_downloads(
    state: &Arc<AppState>,
    page: &mut SearchPage,
) -> Result<(), AppError> {
    let mut hashes = Vec::new();
    for group in &page.groups {
        for torrent in &group.torrents {
            if let Some(hash) = &torrent.info_hash {
                hashes.push(hash.clone());
            }
        }
    }
    let live = live_downloads_by_hash(state, &hashes).await;
    for group in &mut page.groups {
        for torrent in &mut group.torrents {
            torrent.downloads = torrent
                .info_hash
                .as_ref()
                .and_then(|hash| live.get(&hash.to_ascii_lowercase()).cloned())
                .unwrap_or_default();
        }
    }
    Ok(())
}

async fn apply_search_eligibility(
    state: &Arc<AppState>,
    page: &mut SearchPage,
) -> Result<(), AppError> {
    let preferences = state.db.get_runtime_preferences().await?;
    for group in &mut page.groups {
        for torrent in &mut group.torrents {
            let tracker = if torrent.tracker.is_empty() {
                &group.tracker
            } else {
                &torrent.tracker
            };
            torrent.eligibility = Some(preferences.release.eligibility(
                tracker,
                torrent.format.as_deref(),
                torrent.encoding.as_deref(),
                torrent.media.as_deref(),
                torrent.size,
                torrent.leech_status,
                torrent.can_use_token,
            ));
        }
    }
    Ok(())
}

async fn enrich_variant_downloads(
    state: &Arc<AppState>,
    variants: &mut [TorrentVariant],
) -> Result<(), AppError> {
    let hashes = variants
        .iter()
        .filter_map(|variant| variant.info_hash.clone())
        .collect::<Vec<_>>();
    let live = live_downloads_by_hash(state, &hashes).await;
    for variant in variants {
        variant.downloads = variant
            .info_hash
            .as_ref()
            .and_then(|hash| live.get(&hash.to_ascii_lowercase()).cloned())
            .unwrap_or_default();
    }
    Ok(())
}

#[allow(dead_code)]
async fn enrich_requested_variant(
    state: &Arc<AppState>,
    tracker: &str,
    group_id: i64,
    torrent_id: Option<i64>,
    variants: &mut Vec<TorrentVariant>,
) -> Result<(), AppError> {
    let Some(torrent_id) = torrent_id else {
        return Ok(());
    };
    let Some(cached) = state.db.get_canonical(tracker, torrent_id).await? else {
        return Ok(());
    };
    merge_requested_variant(variants, tracker, group_id, torrent_id, cached.value);
    Ok(())
}

fn merge_requested_variant(
    variants: &mut Vec<TorrentVariant>,
    tracker: &str,
    group_id: i64,
    torrent_id: i64,
    canonical: CanonicalTorrent,
) -> bool {
    if !canonical.release.tracker.eq_ignore_ascii_case(tracker)
        || canonical.release.group_id != group_id
        || canonical.variant.torrent_id != torrent_id
    {
        return false;
    }
    if let Some(variant) = variants
        .iter_mut()
        .find(|variant| variant.torrent_id == torrent_id)
    {
        let mut changed = false;
        if variant.info_hash.is_none() && canonical.variant.info_hash.is_some() {
            variant.info_hash = canonical.variant.info_hash;
            changed = true;
        }
        if !variant.token_eligibility_known && canonical.variant.token_eligibility_known {
            variant.can_use_token = canonical.variant.can_use_token;
            variant.token_eligibility_known = true;
            changed = true;
        }
        return changed;
    }
    variants.push(canonical.variant);
    true
}

async fn enrich_variant_library(
    state: &Arc<AppState>,
    tracker: &str,
    group_id: i64,
    variants: &mut [TorrentVariant],
) -> Result<(), AppError> {
    let release = load_library_releases(state)
        .await?
        .into_iter()
        .find(|release| {
            release.release.tracker.eq_ignore_ascii_case(tracker)
                && release.release.group_id == group_id
        });
    let Some(release) = release else {
        return Ok(());
    };
    for variant in variants {
        variant.library = release
            .variants
            .iter()
            .find(|library_variant| library_variant.torrent_id == variant.torrent_id)
            .and_then(|library_variant| library_variant.library.clone());
    }
    Ok(())
}

pub(crate) async fn enrich_library_artist_credits(state: &Arc<AppState>) -> Result<()> {
    let mut groups = HashSet::new();
    for record in state.db.list_library_records().await? {
        if record
            .release
            .value
            .artists
            .iter()
            .any(|artist| artist.source == ArtistCreditSource::Structured)
        {
            continue;
        }
        let tracker = record.release.value.tracker.clone();
        let group_id = record.release.value.group_id;
        groups.insert((tracker, group_id));
    }
    for (tracker, group_id) in groups.into_iter().take(5) {
        if state
            .db
            .get_snapshot::<ReleaseDetail>(&tracker, "group", &group_id.to_string())
            .await?
            .is_some_and(|cached| cached.expires_at > Utc::now())
        {
            continue;
        }
        if let Err(error) = refresh_group(state.clone(), tracker.clone(), group_id).await {
            let rate_limited = error
                .to_string()
                .to_ascii_lowercase()
                .contains("rate limit");
            tracing::warn!(
                tracker = %tracker,
                group_id,
                %error,
                "library artist credit enrichment failed"
            );
            if rate_limited {
                break;
            }
        }
    }
    Ok(())
}

pub(crate) async fn live_downloads_by_hash(
    state: &Arc<AppState>,
    hashes: &[String],
) -> HashMap<String, Vec<LiveDownloadStatus>> {
    let hashes = hashes
        .iter()
        .map(|hash| hash.to_ascii_lowercase())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut result: HashMap<String, Vec<LiveDownloadStatus>> = HashMap::new();
    for (name, client) in &state.download_clients {
        match client.downloads_by_hashes(&hashes).await {
            Ok(downloads) => {
                for download in downloads {
                    if let Err(error) = observe_download(state, &download).await {
                        tracing::warn!(client = %name, %error, "could not index enriched download");
                    }
                    result
                        .entry(download.live.info_hash.clone())
                        .or_default()
                        .push(download.live);
                }
            }
            Err(error) => {
                tracing::warn!(client = %name, %error, "could not enrich tracker response with live state")
            }
        }
    }
    result
}

async fn refresh_group(state: Arc<AppState>, tracker_name: String, id: i64) -> Result<()> {
    let tracker = state
        .trackers
        .get(&tracker_name)
        .ok_or_else(|| anyhow!("tracker disappeared from configuration"))?;
    let (detail, raw) = tracker
        .group_with_class(id, RequestClass::Background)
        .await?;
    let track_index = track_index_from_group(&tracker_name, &detail, &raw);
    state.db.enqueue_track_index(&tracker_name, id).await?;
    state.db.put_track_index(&track_index).await?;
    cache_release_detail(&state.db, &detail).await?;
    let now = Utc::now();
    state
        .db
        .put_snapshot(
            &tracker_name,
            "group",
            &id.to_string(),
            &detail,
            &raw,
            now,
            now + ChronoDuration::hours(24),
        )
        .await
}

pub(crate) async fn refresh_artist_catalog(
    state: Arc<AppState>,
    tracker_name: String,
    id: i64,
    class: RequestClass,
) -> Result<()> {
    let tracker = state
        .trackers
        .get(&tracker_name)
        .ok_or_else(|| anyhow!("tracker disappeared from configuration"))?;
    let (catalog, raw) = tracker.artist_catalog_with_class(id, class).await?;
    cache_artist_catalog(&state.db, &catalog).await?;
    seed_catalog_deduplication(&state, &tracker_name, &catalog).await?;
    let now = Utc::now();
    state
        .db
        .put_snapshot(
            &tracker_name,
            "artist",
            &id.to_string(),
            &catalog,
            &raw,
            now,
            now + ChronoDuration::hours(24),
        )
        .await
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
        provenance: provenance(tracker, cached.fetched_at, stale),
    }
}

fn provenance(tracker: &str, fetched_at: chrono::DateTime<Utc>, stale: bool) -> Provenance {
    let cache_age_seconds = (Utc::now() - fetched_at).num_seconds().max(0);
    Provenance {
        tracker: tracker.to_owned(),
        fetched_at: Some(fetched_at),
        cache_age_seconds: Some(cache_age_seconds),
        stale,
        sources: vec![SourceProvenance {
            provider_id: format!("tracker:{tracker}"),
            tracker: tracker.to_owned(),
            state: if stale {
                SnapshotState::Stale
            } else {
                SnapshotState::Fresh
            },
            fetched_at: Some(fetched_at),
            cache_age_seconds: Some(cache_age_seconds),
            refresh_job_id: None,
            refresh_state: None,
            retry_at: None,
            error_code: None,
        }],
    }
}

#[allow(dead_code)]
async fn enrich_cross_tracker_detail(
    state: &Arc<AppState>,
    origin_tracker: &str,
    detail: &mut ReleaseDetail,
) -> Result<(), AppError> {
    if !detail
        .release
        .sources
        .iter()
        .any(|source| source.tracker.eq_ignore_ascii_case(origin_tracker))
    {
        detail.release.sources.insert(
            0,
            crate::model::ReleaseSource {
                tracker: origin_tracker.to_owned(),
                group_id: detail.release.group_id,
                match_score: 1.0,
            },
        );
    }
    if state.trackers.len() < 2 {
        return Ok(());
    }
    let key = detail.release.group_id.to_string();
    if let Some(cached) = state
        .db
        .get_snapshot::<crate::model::ReleaseMatchRecord>(origin_tracker, "release_match", &key)
        .await?
        && cached.expires_at > Utc::now()
    {
        for source in cached
            .value
            .sources
            .into_iter()
            .filter(|source| !source.tracker.eq_ignore_ascii_case(origin_tracker))
        {
            if let Some(other) = get_or_fetch_group(state, &source.tracker, source.group_id).await?
            {
                crate::release_matcher::merge_release_detail(detail, other, source.match_score);
            }
        }
        return Ok(());
    }

    let request = SearchRequest {
        query: Some(detail.release.title.clone()),
        artist: detail.release.artist.clone(),
        ..Default::default()
    };
    let mut best: Option<(String, i64, f64)> = None;
    for (tracker_name, tracker) in &state.trackers {
        if tracker_name.eq_ignore_ascii_case(origin_tracker) {
            continue;
        }
        let Ok((page, _)) = tracker.search(&request).await else {
            continue;
        };
        for group in page.groups {
            let candidate = ReleaseDetail {
                release: ReleaseSummary {
                    id: group.id,
                    tracker: tracker_name.clone(),
                    group_id: group.group_id,
                    title: group.name,
                    artist: group.artist,
                    artists: Vec::new(),
                    year: group.year,
                    artwork: group.image,
                    release_type: group.release_type,
                    sources: group.sources,
                    album_coverage: None,
                },
                field_provenance: json!({}),
                tags: group.tags,
                description: None,
                record_label: None,
                variants: Vec::new(),
            };
            let score = crate::release_matcher::detail_score(detail, &candidate);
            if score >= crate::release_matcher::AUTO_MERGE_THRESHOLD
                && best.as_ref().is_none_or(|known| score > known.2)
            {
                best = Some((tracker_name.clone(), candidate.release.group_id, score));
            }
        }
    }

    let mut sources = vec![crate::model::ReleaseSource {
        tracker: origin_tracker.to_owned(),
        group_id: detail.release.group_id,
        match_score: 1.0,
    }];
    if let Some((tracker, group_id, score)) = best
        && let Some(other) = get_or_fetch_group(state, &tracker, group_id).await?
    {
        sources.push(crate::model::ReleaseSource {
            tracker: tracker.clone(),
            group_id,
            match_score: score,
        });
        crate::release_matcher::merge_release_detail(detail, other, score);
        let reverse = crate::model::ReleaseMatchRecord {
            matcher_version: crate::release_matcher::MATCHER_VERSION,
            sources: sources.clone(),
        };
        let _ = store(
            &state.db,
            &tracker,
            "release_match",
            &group_id.to_string(),
            reverse,
            json!({ "matcherVersion": crate::release_matcher::MATCHER_VERSION }),
            2_592_000,
        )
        .await?;
    }
    let record = crate::model::ReleaseMatchRecord {
        matcher_version: crate::release_matcher::MATCHER_VERSION,
        sources,
    };
    let _ = store(
        &state.db,
        origin_tracker,
        "release_match",
        &key,
        record,
        json!({ "matcherVersion": crate::release_matcher::MATCHER_VERSION }),
        2_592_000,
    )
    .await?;
    Ok(())
}

#[allow(dead_code)]
async fn get_or_fetch_group(
    state: &Arc<AppState>,
    tracker_name: &str,
    group_id: i64,
) -> Result<Option<ReleaseDetail>, AppError> {
    let key = group_id.to_string();
    if let Some(cached) = state
        .db
        .get_snapshot::<ReleaseDetail>(tracker_name, "group", &key)
        .await?
        && cached.expires_at > Utc::now()
    {
        return Ok(Some(cached.value));
    }
    let Some(tracker) = state.trackers.get(tracker_name) else {
        return Ok(None);
    };
    let (detail, raw) = tracker
        .group_with_class(group_id, RequestClass::Background)
        .await?;
    cache_release_detail(&state.db, &detail).await?;
    let _ = store(
        &state.db,
        tracker_name,
        "group",
        &key,
        detail.clone(),
        raw,
        86_400,
    )
    .await?;
    Ok(Some(detail))
}

async fn apply_download_eligibility(
    state: &Arc<AppState>,
    variants: &mut [TorrentVariant],
) -> Result<()> {
    let preferences = state.db.get_runtime_preferences().await?;
    for variant in variants {
        variant.eligibility = Some(preferences.release.eligibility(
            &variant.tracker,
            variant.format.as_deref(),
            variant.encoding.as_deref(),
            variant.media.as_deref(),
            variant.size,
            variant.leech_status,
            variant.can_use_token || !variant.token_eligibility_known,
        ));
    }
    Ok(())
}

#[allow(dead_code)]
async fn enrich_cross_tracker_artist_catalog(
    state: &Arc<AppState>,
    origin_tracker: &str,
    catalog: &mut ArtistCatalogPage,
) -> Result<(), AppError> {
    if state.trackers.len() < 2 {
        return Ok(());
    }
    let match_key = catalog.artist.artist_id.to_string();
    if let Some(cached) = state
        .db
        .get_snapshot::<crate::model::ArtistMatchRecord>(origin_tracker, "artist_match", &match_key)
        .await?
        && cached.expires_at > Utc::now()
    {
        for source in cached
            .value
            .sources
            .into_iter()
            .filter(|source| !source.tracker.eq_ignore_ascii_case(origin_tracker))
        {
            if !state.trackers.contains_key(&source.tracker) {
                continue;
            }
            let key = source.artist_id.to_string();
            if let Some(cached) = state
                .db
                .get_snapshot::<ArtistCatalogPage>(&source.tracker, "artist", &key)
                .await?
            {
                merge_artist_catalog(catalog, cached.value);
            }
        }
        return Ok(());
    }

    let pending = crate::model::ArtistMatchRecord {
        matcher_version: crate::release_matcher::MATCHER_VERSION,
        sources: vec![crate::model::ArtistSource {
            tracker: origin_tracker.to_owned(),
            artist_id: catalog.artist.artist_id,
        }],
    };
    let _ = store(
        &state.db,
        origin_tracker,
        "artist_match",
        &match_key,
        pending,
        json!({
            "matcherVersion": crate::release_matcher::MATCHER_VERSION,
            "state": "resolving"
        }),
        60,
    )
    .await?;

    let discovery_state = state.clone();
    let discovery_tracker = origin_tracker.to_owned();
    let discovery_catalog = catalog.clone();
    tokio::spawn(async move {
        if let Err(error) = discover_cross_tracker_artist_catalog(
            discovery_state,
            discovery_tracker,
            discovery_catalog,
        )
        .await
        {
            tracing::warn!(
                error_code = error.body.error.code,
                error = %error.body.error.message,
                "cross-tracker artist discovery failed"
            );
        }
    });
    Ok(())
}

#[allow(dead_code)]
async fn discover_cross_tracker_artist_catalog(
    state: Arc<AppState>,
    origin_tracker: String,
    catalog: ArtistCatalogPage,
) -> Result<(), AppError> {
    let artist_name = catalog.artist.name.clone();
    let normalized_artist = crate::release_matcher::normalized(&artist_name);
    let match_key = catalog.artist.artist_id.to_string();
    let mut transient_failure = false;
    let mut sources = vec![crate::model::ArtistSource {
        tracker: origin_tracker.to_owned(),
        artist_id: catalog.artist.artist_id,
    }];
    for (tracker_name, tracker) in &state.trackers {
        if tracker_name.eq_ignore_ascii_case(&origin_tracker) {
            continue;
        }
        let request = SearchRequest {
            artist: Some(artist_name.clone()),
            ..Default::default()
        };
        let page = match tracker.search(&request).await {
            Ok((page, _)) => page,
            Err(error) => {
                transient_failure = true;
                tracing::warn!(
                    tracker = %tracker_name,
                    artist = %artist_name,
                    %error,
                    "cross-tracker artist search failed"
                );
                continue;
            }
        };
        let mut counterpart_id = None;
        for group in page.groups.into_iter().take(6) {
            let Some(detail) = get_or_fetch_group(&state, tracker_name, group.group_id).await?
            else {
                continue;
            };
            counterpart_id = detail
                .release
                .artists
                .iter()
                .find(|artist| {
                    artist.role == crate::model::ArtistRole::Primary
                        && crate::release_matcher::normalized(&artist.name) == normalized_artist
                })
                .and_then(|artist| artist.artist_id);
            if counterpart_id.is_some() {
                break;
            }
        }
        let Some(counterpart_id) = counterpart_id else {
            continue;
        };
        let (other, raw) = match tracker
            .artist_catalog_with_class(counterpart_id, RequestClass::Background)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                transient_failure = true;
                tracing::warn!(
                    tracker = %tracker_name,
                    artist_id = counterpart_id,
                    %error,
                    "matched artist catalog fetch failed"
                );
                continue;
            }
        };
        cache_artist_catalog(&state.db, &other).await?;
        let _ = store(
            &state.db,
            tracker_name,
            "artist",
            &counterpart_id.to_string(),
            other.clone(),
            raw,
            86_400,
        )
        .await?;
        sources.push(crate::model::ArtistSource {
            tracker: tracker_name.clone(),
            artist_id: counterpart_id,
        });
    }
    let _ = store(
        &state.db,
        &origin_tracker,
        "artist_match",
        &match_key,
        crate::model::ArtistMatchRecord {
            matcher_version: crate::release_matcher::MATCHER_VERSION,
            sources,
        },
        json!({
            "matcherVersion": crate::release_matcher::MATCHER_VERSION,
            "state": if transient_failure { "retry" } else { "complete" }
        }),
        if transient_failure { 60 } else { 2_592_000 },
    )
    .await?;
    Ok(())
}

#[allow(dead_code)]
fn merge_artist_catalog(primary: &mut ArtistCatalogPage, secondary: ArtistCatalogPage) {
    if primary.artist.artwork.is_none() {
        primary.artist.artwork = secondary.artist.artwork;
    }
    for mut group in secondary.groups {
        let matched = primary
            .groups
            .iter()
            .enumerate()
            .find_map(|(index, known)| {
                let score = crate::release_matcher::summary_score(&known.release, &group.release);
                (score >= crate::release_matcher::AUTO_MERGE_THRESHOLD).then_some((index, score))
            });
        if let Some((index, score)) = matched {
            let known = &mut primary.groups[index];
            known.release.sources.push(crate::model::ReleaseSource {
                tracker: group.release.tracker.clone(),
                group_id: group.release.group_id,
                match_score: score,
            });
            known.variants.append(&mut group.variants);
            for tag in group.tags {
                if !known
                    .tags
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(&tag))
                {
                    known.tags.push(tag);
                }
            }
            for role in group.roles {
                if !known.roles.contains(&role) {
                    known.roles.push(role);
                }
            }
        } else {
            primary.groups.push(group);
        }
    }
    primary.primary_count = primary
        .groups
        .iter()
        .filter(|group| {
            group
                .roles
                .contains(&crate::model::ArtistCatalogRole::Primary)
        })
        .count();
    primary.appearance_count = primary.groups.len().saturating_sub(primary.primary_count);
    primary.groups.sort_by(|left, right| {
        right
            .release
            .year
            .unwrap_or_default()
            .cmp(&left.release.year.unwrap_or_default())
            .then_with(|| left.release.title.cmp(&right.release.title))
    });
}

fn get_tracker<'a>(
    state: &'a AppState,
    requested: Option<&str>,
) -> Result<(&'a str, &'a Arc<dyn TrackerClient>), AppError> {
    let name = requested
        .or_else(|| {
            (state.trackers.len() == 1)
                .then(|| state.trackers.keys().next().map(String::as_str))
                .flatten()
        })
        .ok_or_else(|| {
            if state.trackers.is_empty() {
                AppError::unavailable("tracker_unconfigured", "No tracker is configured")
            } else {
                AppError::bad_request(
                    "tracker_required",
                    "Choose a tracker for this source-specific operation",
                )
            }
        })?;
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
    let requested_asset = UI.get_file(path);
    let asset = requested_asset.or_else(|| UI.get_file("index.html"));
    let Some(asset) = asset else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let is_index = asset.path() == std::path::Path::new("index.html");
    let body = if is_index {
        let index = asset.contents_utf8().unwrap_or_default();
        Body::from(render_ui_index(index, &state.base_path))
    } else {
        Body::from(asset.contents().to_vec())
    };
    let mime = mime_guess::from_path(asset.path()).first_or_octet_stream();
    let mut response = Response::new(body);
    if requested_asset.is_none() && !is_ui_route(path) {
        *response.status_mut() = StatusCode::NOT_FOUND;
    }
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

fn render_ui_index(index: &str, base_path: &str) -> String {
    let base = if base_path == "/" {
        "/".to_owned()
    } else {
        format!("{base_path}/")
    };
    let injection = format!(
        r#"<head><base href="{base}"><script>window.__WOTBOX_CONFIG__={};</script>"#,
        json!({ "basePath": base_path }),
    );
    index.replacen("<head>", &injection, 1)
}

fn is_ui_route(path: &str) -> bool {
    let segments: Vec<_> = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    match segments.as_slice() {
        []
        | ["search"]
        | ["library"]
        | ["downloads"]
        | ["channels"]
        | ["matches"]
        | ["preferences"] => true,
        ["library", "artists", id] | ["releases", id] => uuid::Uuid::parse_str(id).is_ok(),
        ["channels", channel, "packs", id] => {
            matches!(*channel, "country_chart" | "lastfm" | "trumped_downloads")
                && uuid::Uuid::parse_str(id).is_ok()
        }
        _ => false,
    }
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

    fn conflict(code: &'static str, message: impl ToString) -> Self {
        Self::new(StatusCode::CONFLICT, code, message, true)
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
        if let Some(provider) = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ProviderRequestError>())
        {
            let (status, code) = match provider {
                ProviderRequestError::Busy { .. } => {
                    (StatusCode::TOO_MANY_REQUESTS, "provider_busy")
                }
                ProviderRequestError::Unknown(_) => (StatusCode::NOT_FOUND, "provider_not_found"),
                _ if provider.is_unavailable() => {
                    (StatusCode::SERVICE_UNAVAILABLE, "provider_unavailable")
                }
                _ => (StatusCode::BAD_GATEWAY, "provider_error"),
            };
            return Self::new(status, code, provider, true);
        }
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
    paths(
        live, ready, preferences, update_preferences, providers, pause_provider, resume_provider,
        plex_status, scan_plex,
        background_jobs, cancel_background_job, retry_background_job,
        account, accounts, search, release, cross_seed_plans, downloads, imports, download_detail_compatibility, create_download,
        download_job,
        library_artists, library_artist, artist_catalog, channels, update_channel, refresh_channel,
        channel_run, channel_packs, channel_pack, replan_channel_pack, attach_channel_pack_item,
        accept_channel_pack, reject_channel_pack
    ),
    components(schemas(
        Health, Account, crate::model::TrackerAccount, Provenance, RuntimePreferences,
        crate::model::ImportPreferences, crate::model::ImportCleanupMode,
        crate::model::ApiPreferences, crate::model::ProviderPolicyOverride,
        crate::model::ProviderCircuitState, crate::model::ProviderQueueCounts, ProviderStatus,
        crate::model::BackgroundJobState, crate::model::BackgroundJobStatus,
        crate::model::BackgroundJobCounts, BackgroundJobsOverview,
        PlexIntegrationStatus, PlexScanQueued,
        crate::model::ReleasePreferences, crate::model::TrackerPreference,
        crate::model::TrackerDownloadMode, crate::model::LeechStatus,
        crate::model::DownloadEligibility, crate::model::DownloadEligibilityReason,
        SearchPage, TorrentMetadata, DownloadProfile,
        LiveDownloadStatus, crate::model::DownloadDiagnostic, ClientDownloadState,
        CreateDownload, DownloadJob, DownloadState,
        PublicConfig, ArtistRole, ArtistCreditSource, ArtistCredit, ReleaseSummary,
        TorrentVariant, ReleaseDetail, CanonicalDownload, crate::model::ReleaseSource,
        crate::model::DownloadFile, crate::model::CrossSeedPlan,
        DownloadsPage, ImportsPage, crate::model::ImportTask, crate::model::ImportTaskState,
        crate::model::ImportTaskCounts, crate::model::ImportSupersession,
        LibraryAvailability, LibraryCopy, LibraryVariantState, LibraryRelease,
        LibraryArtistSummary, LibraryIndexStatus, LibraryArtistsPage, LibraryArtistPage,
        ArtistCatalogRole, crate::model::ArtistCatalogArtist, ArtistCatalogRelease,
        ArtistCatalogPage,
        ChannelKind, crate::model::ChannelSchedule, crate::model::CountryChartChannelSettings,
        crate::model::LastfmChannelSettings, ChannelConfig, ChannelRunStatus, ChannelRunTrigger,
        ChannelRun, ChannelPackDecision, crate::model::RecommendationMatchState,
        crate::model::PackItemPlanState, crate::model::RecommendationSource,
        crate::model::TrumpedDownloadRef, crate::model::PlannedDownload,
        crate::model::ReplacementTargetState, crate::model::ReplacementTarget,
        crate::model::ChannelPackItem,
        crate::model::ChannelPlanSummary, ChannelPack, ChannelPackSummary, ChannelOverview,
        DecideChannelPack, AttachChannelPackItem, ChannelBatchResult,
        ErrorBody, ErrorDetail
    )),
    tags((name = "wotbox", description = "Wotbox API"))
)]
struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_torrents_are_persisted_privately() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("job.torrent");
        write_staged_download(&path, b"torrent payload").expect("stage torrent");
        assert_eq!(
            std::fs::read(&path).expect("read stage"),
            b"torrent payload"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("stage metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    fn credit(id: i64, name: &str, role: ArtistRole) -> ArtistCredit {
        ArtistCredit {
            canonical_id: None,
            key: format!("id:{id}"),
            tracker: "ops".into(),
            artist_id: Some(id),
            name: name.into(),
            role,
            source: ArtistCreditSource::Structured,
        }
    }

    fn library_release(group_id: i64, artists: Vec<ArtistCredit>) -> LibraryRelease {
        let now = Utc::now();
        LibraryRelease {
            release: ReleaseSummary {
                id: None,
                tracker: "ops".into(),
                group_id,
                title: format!("Release {group_id}"),
                artist: None,
                artists,
                year: None,
                artwork: None,
                release_type: None,
                sources: vec![crate::model::ReleaseSource {
                    tracker: "ops".into(),
                    group_id,
                    match_score: 1.0,
                }],
                album_coverage: None,
            },
            variants: Vec::new(),
            release_copies: Vec::new(),
            availability: LibraryAvailability::Present,
            added_at: now,
            provenance: provenance("ops", now, false),
        }
    }

    fn torrent_variant(torrent_id: i64, group_id: i64) -> TorrentVariant {
        TorrentVariant {
            tracker: "ops".into(),
            torrent_id,
            group_id,
            info_hash: Some(format!("HASH{torrent_id}")),
            format: Some("FLAC".into()),
            encoding: Some("Lossless".into()),
            media: Some("WEB".into()),
            size: Some(1_000),
            seeders: Some(1),
            leechers: Some(0),
            snatched: Some(1),
            freeleech: false,
            leech_status: crate::model::LeechStatus::Regular,
            can_use_token: false,
            token_eligibility_known: true,
            eligibility: None,
            remaster_title: None,
            downloads: Vec::new(),
            library: None,
        }
    }

    #[test]
    fn library_artist_index_excludes_guest_only_and_compilation_only_artists() {
        let mut compilation = library_release(
            3,
            vec![
                credit(10, "Primary Artist", ArtistRole::Primary),
                credit(40, "Compilation Only", ArtistRole::Primary),
            ],
        );
        compilation.release.release_type = Some("Compilation".into());
        let releases = vec![
            library_release(
                1,
                vec![
                    credit(10, "Primary Artist", ArtistRole::Primary),
                    credit(20, "Guest Only", ArtistRole::Guest),
                ],
            ),
            library_release(
                2,
                vec![
                    credit(30, "Another Primary", ArtistRole::Primary),
                    credit(10, "Primary Artist", ArtistRole::Guest),
                ],
            ),
            compilation,
        ];

        let summaries = build_artist_summaries(&releases);
        assert_eq!(summaries.len(), 2);
        assert!(
            summaries
                .iter()
                .all(|artist| !matches!(artist.name.as_str(), "Guest Only" | "Compilation Only"))
        );
        assert_eq!(
            summaries
                .iter()
                .find(|artist| artist.name == "Primary Artist")
                .expect("primary artist")
                .release_count,
            2
        );

        let mut various_artists = library_release(
            4,
            vec![credit(50, "Soundtrack Appearance", ArtistRole::Primary)],
        );
        various_artists.release.artist = Some("Various artists".into());
        assert!(is_compilation(&various_artists.release));
    }

    #[test]
    fn ui_route_inventory_accepts_only_real_application_views() {
        for path in [
            "",
            "search",
            "library",
            "library/artists/080bca00-45b3-4d6b-a6c6-ee3312cbff9a",
            "downloads",
            "channels",
            "channels/country_chart/packs/080bca00-45b3-4d6b-a6c6-ee3312cbff9a",
            "channels/trumped_downloads/packs/080bca00-45b3-4d6b-a6c6-ee3312cbff9a",
            "matches",
            "preferences",
            "releases/d243a33e-93c5-4f85-b750-5aa301fbe1b5",
        ] {
            assert!(is_ui_route(path), "{path} should be a UI route");
        }
        for path in [
            "unknown",
            "library/artists/ops",
            "downloads/music",
            "downloads/music/abc123",
            "channels/unknown/packs/080bca00-45b3-4d6b-a6c6-ee3312cbff9a",
            "channels/lastfm/packs/not-a-uuid",
            "preferences/advanced",
            "releases/ops",
            "releases/ops/445818",
        ] {
            assert!(!is_ui_route(path), "{path} should not be a UI route");
        }
    }

    #[test]
    fn ui_base_precedes_relative_assets_for_deep_links() {
        let index = r#"<html><head><script src="./assets/app.js"></script><link href="./assets/app.css"></head><body></body></html>"#;
        let rendered = render_ui_index(index, "/media/music/wotbox");

        let base_position = rendered
            .find(r#"<base href="/media/music/wotbox/">"#)
            .expect("base element");
        let script_position = rendered
            .find(r#"src="./assets/app.js""#)
            .expect("relative script");
        let stylesheet_position = rendered
            .find(r#"href="./assets/app.css""#)
            .expect("relative stylesheet");

        assert!(base_position < script_position);
        assert!(base_position < stylesheet_position);
        assert!(
            rendered.contains(r#"window.__WOTBOX_CONFIG__={"basePath":"/media/music/wotbox"};"#)
        );
    }

    #[test]
    fn requested_canonical_variant_fills_partial_tracker_variants() {
        let release = library_release(176023, Vec::new()).release;
        let canonical = CanonicalTorrent {
            release,
            variant: torrent_variant(345678, 176023),
            tags: Vec::new(),
            description: None,
            record_label: None,
        };
        let mut variants = vec![torrent_variant(111, 176023)];
        assert!(merge_requested_variant(
            &mut variants,
            "OPS",
            176023,
            345678,
            canonical.clone()
        ));
        assert_eq!(
            variants
                .iter()
                .map(|variant| variant.torrent_id)
                .collect::<Vec<_>>(),
            vec![111, 345678]
        );
        assert!(!merge_requested_variant(
            &mut variants,
            "ops",
            176023,
            345678,
            canonical.clone()
        ));
        assert!(!merge_requested_variant(
            &mut Vec::new(),
            "ops",
            999,
            345678,
            canonical
        ));

        let mut partial = torrent_variant(345678, 176023);
        partial.info_hash = None;
        partial.token_eligibility_known = false;
        let mut variants = vec![partial];
        let canonical = CanonicalTorrent {
            release: library_release(176023, Vec::new()).release,
            variant: torrent_variant(345678, 176023),
            tags: Vec::new(),
            description: None,
            record_label: None,
        };
        assert!(merge_requested_variant(
            &mut variants,
            "ops",
            176023,
            345678,
            canonical
        ));
        assert_eq!(variants[0].info_hash.as_deref(), Some("HASH345678"));
        assert!(variants[0].token_eligibility_known);
    }

    #[test]
    fn openapi_exposes_release_detail_instead_of_legacy_download_detail() {
        let document = serde_json::to_value(ApiDoc::openapi()).expect("serialize OpenAPI");
        assert!(document["paths"].get("/api/v1/releases/{id}").is_some());
        assert!(
            document["paths"]
                .get("/api/v1/downloads/{client}/{info_hash}")
                .is_some()
        );
        assert!(
            document["paths"]
                .get("/api/v1/download-jobs/{id}")
                .is_some()
        );
    }
}
