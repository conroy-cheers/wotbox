use std::{
    collections::{HashMap, HashSet},
    io::Write,
    sync::Arc,
    time::Duration,
};

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
    config::{Config, DownloadClientKind, TrackerKind},
    db::{Cached, Database},
    dedupe::{compute_raw_coverage, track_index_from_group},
    model::{
        Account, ApiEnvelope, ArtistCatalogPage, ArtistCatalogRelease, ArtistCatalogRole,
        ArtistCredit, ArtistCreditSource, ArtistRole, CanonicalDownload, CanonicalTorrent,
        ClientDownloadState, CreateDownload, DeduplicationIndexStatus, DownloadJob,
        DownloadProfile, DownloadState, DownloadsPage, LibraryArtistPage, LibraryArtistSummary,
        LibraryArtistsPage, LibraryAvailability, LibraryCopy, LibraryIndexStatus, LibraryRelease,
        LibraryVariantState, LiveDownloadStatus, Provenance, PublicConfig, ReleaseDetail,
        ReleaseSummary, RuntimePreferences, SearchPage, TorrentMetadata, TorrentVariant,
    },
    qbittorrent::{DownloadClient, QbittorrentClient},
    tracker::{
        GazelleTrackerClient, SearchRequest, TrackerClient, fallback_artist_credit,
        search_cache_key,
    },
};

static UI: Dir<'_> = include_dir!("$OUT_DIR/ui");

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub base_path: String,
    pub trackers: HashMap<String, Arc<dyn TrackerClient>>,
    pub download_clients: HashMap<String, Arc<dyn DownloadClient>>,
    pub profiles: HashMap<String, DownloadProfile>,
    pub announce_hosts: HashMap<String, String>,
}

impl AppState {
    pub async fn new(config: &Config) -> Result<Arc<Self>> {
        let db = Database::open(&config.database_path).await?;
        let mut trackers: HashMap<String, Arc<dyn TrackerClient>> = HashMap::new();
        let mut announce_hosts = HashMap::new();
        for (name, tracker) in &config.trackers {
            trackers.insert(
                name.clone(),
                Arc::new(GazelleTrackerClient::new(name.clone(), tracker)?),
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
        let state = Arc::new(Self {
            db,
            base_path: config.base_path.clone(),
            trackers,
            download_clients,
            profiles,
            announce_hosts,
        });
        state.db.recover_resolving_links().await?;
        state.db.recover_track_indexes().await?;
        seed_existing_job_links(&state).await?;
        Ok(state)
    }
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
        .route("/api/v1/account", get(account))
        .route("/api/v1/search", get(search))
        .route("/api/v1/groups/{tracker}/{id}", get(group))
        .route("/api/v1/torrents/{tracker}/{id}", get(torrent))
        .route(
            "/api/v1/artists/{tracker}/{id}/releases",
            get(artist_catalog),
        )
        .route("/api/v1/download-profiles", get(download_profiles))
        .route("/api/v1/library/artists", get(library_artists))
        .route(
            "/api/v1/library/artists/{tracker}/{artist_key}",
            get(library_artist),
        )
        .route("/api/v1/downloads", get(downloads).post(create_download))
        .route("/api/v1/downloads/{client}/{info_hash}", get(download))
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
    Json(preferences): Json<RuntimePreferences>,
) -> Result<Json<RuntimePreferences>, AppError> {
    preferences
        .release
        .validate()
        .map_err(|message| AppError::bad_request("invalid_preferences", message))?;
    state.db.put_runtime_preferences(&preferences).await?;
    Ok(Json(preferences))
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
        let mut response = envelope(tracker_name, cached, false);
        enrich_search_downloads(&state, &mut response.data).await?;
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
            enrich_search_downloads(&state, &mut value).await?;
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
            enrich_search_downloads(&state, &mut response.data).await?;
            enrich_search_deduplication(&state, tracker_name, &mut response.data).await?;
            Ok(Json(response))
        }
    }
}

async fn group(
    State(state): State<Arc<AppState>>,
    Path((tracker_name, id)): Path<(String, i64)>,
    Query(query): Query<RefreshQuery>,
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
        enrich_variant_downloads(&state, &mut response.data.variants).await?;
        enrich_variant_library(&state, &tracker_name, id, &mut response.data.variants).await?;
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
            enrich_variant_downloads(&state, &mut value.variants).await?;
            enrich_variant_library(&state, &tracker_name, id, &mut value.variants).await?;
            Ok(Json(ApiEnvelope {
                data: value,
                provenance: provenance(&tracker_name, cached.fetched_at, false),
            }))
        }
        Err(error) => {
            let mut response =
                stale_or_error::<ReleaseDetail>(&state.db, &tracker_name, "group", &key, error)
                    .await?;
            enrich_variant_library(&state, &tracker_name, id, &mut response.0.data.variants)
                .await?;
            Ok(response)
        }
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
        if stale {
            let refresh_state = state.clone();
            let refresh_tracker = tracker_name.clone();
            tokio::spawn(async move {
                if let Err(error) = refresh_artist_catalog(refresh_state, refresh_tracker, id).await
                {
                    tracing::warn!(%error, "asynchronous artist catalog refresh failed");
                }
            });
        }
        return Ok(Json(response));
    }
    match tracker.artist_catalog(id).await {
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
            Ok(Json(response))
        }
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
    variants: HashMap<i64, (TorrentVariant, Vec<LibraryCopy>)>,
    added_at: chrono::DateTime<Utc>,
    fetched_at: chrono::DateTime<Utc>,
    stale: bool,
}

async fn load_library_releases(state: &AppState) -> Result<Vec<LibraryRelease>, AppError> {
    let records = state.db.list_library_records().await?;
    let mut groups: HashMap<(String, i64), LibraryReleaseBuild> = HashMap::new();
    for record in records {
        let tracker = record.canonical.value.release.tracker.clone();
        let group_id = record.canonical.value.release.group_id;
        let torrent_id = record.canonical.value.variant.torrent_id;
        let copy = LibraryCopy {
            client: record.client,
            info_hash: record.info_hash,
            present: record.present,
            completed_at: record.completed_at,
            last_seen_at: record.last_seen_at,
            missing_since: record.missing_since,
        };
        let entry = groups
            .entry((tracker, group_id))
            .or_insert_with(|| LibraryReleaseBuild {
                release: record.canonical.value.release.clone(),
                variants: HashMap::new(),
                added_at: record.library_added_at,
                fetched_at: record.canonical.fetched_at,
                stale: record.canonical.expires_at <= Utc::now(),
            });
        if entry.release.artists.is_empty() && !record.canonical.value.release.artists.is_empty() {
            entry.release = record.canonical.value.release.clone();
        }
        entry.added_at = entry.added_at.min(record.library_added_at);
        entry.fetched_at = entry.fetched_at.max(record.canonical.fetched_at);
        entry.stale |= record.canonical.expires_at <= Utc::now();
        let variant = entry.variants.entry(torrent_id).or_insert_with(|| {
            let mut variant = record.canonical.value.variant.clone();
            variant.downloads.clear();
            variant.library = None;
            (variant, Vec::new())
        });
        variant.1.push(copy);
    }

    let mut releases = groups
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
            let availability = if present == variants.len() {
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
                availability,
                added_at: group.added_at,
            }
        })
        .collect::<Vec<_>>();
    enrich_release_coverages(state, &mut releases).await?;
    sort_library_releases(&mut releases, "year_desc");
    Ok(releases)
}

fn library_release_matches(release: &LibraryRelease, query: &LibraryQuery) -> bool {
    if query
        .tracker
        .as_deref()
        .filter(|value| !value.is_empty())
        .is_some_and(|tracker| !release.release.tracker.eq_ignore_ascii_case(tracker))
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
    type ArtistGroupKey = (String, String, String, Option<i64>, ArtistCreditSource);
    let primary_artists = releases
        .iter()
        .filter(|release| !is_compilation(&release.release))
        .flat_map(|release| release.release.artists.iter())
        .filter(|artist| artist.role == ArtistRole::Primary)
        .map(|artist| (artist.tracker.to_ascii_lowercase(), artist.key.clone()))
        .collect::<HashSet<_>>();
    let mut grouped: HashMap<ArtistGroupKey, Vec<&'a LibraryRelease>> = HashMap::new();
    for release in releases
        .iter()
        .filter(|release| !is_compilation(&release.release))
    {
        let mut seen = HashSet::new();
        for artist in &release.release.artists {
            if primary_artists.contains(&(artist.tracker.to_ascii_lowercase(), artist.key.clone()))
                && seen.insert(artist.key.clone())
            {
                grouped
                    .entry((
                        artist.tracker.clone(),
                        artist.key.clone(),
                        artist.name.clone(),
                        artist.artist_id,
                        artist.source.clone(),
                    ))
                    .or_default()
                    .push(release);
            }
        }
    }
    let mut artists = grouped
        .into_iter()
        .map(|((tracker, key, name, artist_id, source), releases)| {
            let artist = ArtistCredit {
                key: key.clone(),
                tracker: tracker.clone(),
                artist_id,
                name,
                role: ArtistRole::Primary,
                source,
            };
            artist_summary(&tracker, &key, &artist, &releases)
        })
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
    state: &AppState,
    releases: &[LibraryRelease],
) -> Result<LibraryIndexStatus, AppError> {
    let mut deduplication = DeduplicationIndexStatus::default();
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
        match state
            .db
            .get_single_coverage(&release.release.tracker, release.release.group_id)
            .await?
            .map(|stored| stored.state)
            .as_deref()
        {
            Some("ready") => deduplication.checked += 1,
            Some("resolving") => deduplication.resolving += 1,
            Some("failed") => deduplication.failed += 1,
            _ => deduplication.pending += 1,
        }
    }
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
    get, path = "/api/v1/library/artists/{tracker}/{artist_key}",
    params(
        ("tracker" = String, Path),
        ("artist_key" = String, Path),
        LibraryQuery
    ),
    responses(
        (status = 200, body = LibraryArtistPage),
        (status = 404, description = "Library artist not found")
    )
)]
async fn library_artist(
    State(state): State<Arc<AppState>>,
    Path((tracker, artist_key)): Path<(String, String)>,
    Query(query): Query<LibraryQuery>,
) -> Result<Json<LibraryArtistPage>, AppError> {
    let all = load_library_releases(&state).await?;
    let artist_releases = all
        .iter()
        .filter(|release| !is_compilation(&release.release))
        .filter(|release| {
            release.release.artists.iter().any(|artist| {
                artist.tracker.eq_ignore_ascii_case(&tracker) && artist.key == artist_key
            })
        })
        .collect::<Vec<_>>();
    let artist = artist_releases
        .iter()
        .find_map(|release| {
            release.release.artists.iter().find(|artist| {
                artist.tracker.eq_ignore_ascii_case(&tracker)
                    && artist.key == artist_key
                    && artist.role == ArtistRole::Primary
            })
        })
        .ok_or_else(|| AppError::not_found("artist_not_found", "Library artist not found"))?;
    let summary = artist_summary(&tracker, &artist_key, artist, &artist_releases);
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
    let mut observed = Vec::new();
    for (name, client) in clients {
        let mut client_offset = 0;
        loop {
            let mut page = client
                .downloads(500, client_offset)
                .await
                .map_err(|error| {
                    AppError::unavailable("download_client_unavailable", format!("{name}: {error}"))
                })?;
            let count = page.len();
            for download in &page {
                observe_download(&state, download).await?;
            }
            observed.append(&mut page);
            if count < 500 || client_offset >= 100_000 {
                break;
            }
            client_offset += 500;
        }
    }
    observed.sort_by(|left, right| {
        right
            .live
            .added_at
            .cmp(&left.live.added_at)
            .then_with(|| left.live.info_hash.cmp(&right.live.info_hash))
    });
    let mut items = Vec::new();
    for download in observed {
        let Some(link) = state
            .db
            .get_link(&download.live.client, &download.live.info_hash)
            .await?
        else {
            continue;
        };
        if link.resolution_state != "linked" {
            continue;
        }
        let (Some(tracker), Some(torrent_id)) = (link.tracker.as_deref(), link.torrent_id) else {
            continue;
        };
        let Some(canonical) = state.db.get_canonical(tracker, torrent_id).await? else {
            continue;
        };
        let stale = canonical.expires_at <= Utc::now();
        if stale {
            let refresh_state = state.clone();
            let refresh_tracker = tracker.to_owned();
            let refresh_hash = download.live.info_hash.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    refresh_canonical_by_hash(refresh_state, refresh_tracker, refresh_hash).await
                {
                    tracing::warn!(%error, "asynchronous canonical torrent refresh failed");
                }
            });
        }
        let mut variant = canonical.value.variant.clone();
        variant.downloads = vec![download.live.clone()];
        items.push(CanonicalDownload {
            release: canonical.value.release,
            variant,
            download: download.live,
            provenance: provenance(tracker, canonical.fetched_at, stale),
        });
    }
    let items = items
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    Ok(Json(DownloadsPage {
        items,
        index: state.db.index_counts().await?,
    }))
}

#[utoipa::path(
    get, path = "/api/v1/downloads/{client}/{info_hash}",
    params(
        ("client" = String, Path, description = "Configured download client name"),
        ("info_hash" = String, Path, description = "Torrent info hash")
    ),
    responses(
        (status = 200, body = CanonicalDownload),
        (status = 404, description = "Torrent is missing or its tracker release is unresolved")
    )
)]
async fn download(
    State(state): State<Arc<AppState>>,
    Path((client_name, info_hash)): Path<(String, String)>,
) -> Result<Json<CanonicalDownload>, AppError> {
    let client = state.download_clients.get(&client_name).ok_or_else(|| {
        AppError::bad_request("unknown_download_client", "Unknown download client")
    })?;
    let observed = client
        .download(&info_hash)
        .await
        .map_err(|error| AppError::unavailable("download_client_unavailable", error))?
        .ok_or_else(|| {
            AppError::not_found(
                "download_not_found",
                "Torrent is not present in the download client",
            )
        })?;
    observe_download(&state, &observed).await?;
    let link = state
        .db
        .get_link(&client_name, &info_hash)
        .await?
        .filter(|link| link.resolution_state == "linked")
        .ok_or_else(|| {
            AppError::not_found(
                "release_unresolved",
                "The tracker release for this torrent has not been resolved",
            )
        })?;
    let canonical = state
        .db
        .get_canonical(
            link.tracker.as_deref().unwrap_or_default(),
            link.torrent_id.unwrap_or_default(),
        )
        .await?
        .ok_or_else(|| {
            AppError::not_found(
                "release_unresolved",
                "The tracker release for this torrent has not been resolved",
            )
        })?;
    let stale = canonical.expires_at <= Utc::now();
    if stale {
        let refresh_state = state.clone();
        let refresh_tracker = link.tracker.clone().unwrap_or_default();
        let refresh_hash = observed.live.info_hash.clone();
        tokio::spawn(async move {
            if let Err(error) =
                refresh_canonical_by_hash(refresh_state, refresh_tracker, refresh_hash).await
            {
                tracing::warn!(%error, "asynchronous canonical torrent refresh failed");
            }
        });
    }
    let mut variant = canonical.value.variant.clone();
    variant.downloads = vec![observed.live.clone()];
    Ok(Json(CanonicalDownload {
        release: canonical.value.release,
        variant,
        download: observed.live,
        provenance: provenance(
            link.tracker.as_deref().unwrap_or_default(),
            canonical.fetched_at,
            stale,
        ),
    }))
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
    let task_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = resolve_due_links(&task_state).await {
            tracing::warn!(%error, "manual release resolution retry failed");
        }
    });
    Ok(StatusCode::ACCEPTED)
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

    let (metadata, canonical, raw) = tracker.torrent(job.torrent_id).await?;
    let preferences = state.db.get_runtime_preferences().await?;
    if !preferences.release.allows(
        canonical.variant.format.as_deref(),
        canonical.variant.encoding.as_deref(),
    ) {
        return Err(anyhow!(
            "release quality '{}' is below the configured '{}' cutoff",
            crate::model::ReleasePreferences::quality_class(
                canonical.variant.format.as_deref(),
                canonical.variant.encoding.as_deref(),
            ),
            preferences.release.minimum_quality
        ));
    }
    if job.use_token && metadata.token_eligibility_known && !metadata.can_use_token {
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
        .put_canonical(
            &canonical,
            Utc::now(),
            Utc::now() + ChronoDuration::hours(24),
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
    state
        .db
        .seed_download_link(
            &profile.client,
            &metadata.info_hash,
            &job.tracker,
            metadata.group_id,
            metadata.torrent_id,
            true,
        )
        .await?;

    if let Some(existing) = client.download(&metadata.info_hash).await? {
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
                        tracing::warn!(job_id = %job.id, %error, "qBittorrent reconciliation failed")
                    }
                }
            }
        }
    });
}

pub fn spawn_download_indexer(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut tick = 0_u64;
        loop {
            interval.tick().await;
            if tick.is_multiple_of(5)
                && let Err(error) = scan_download_clients(&state).await
            {
                tracing::warn!(%error, "download release index scan failed");
            }
            if let Err(error) = enrich_library_artist_credits(&state).await {
                tracing::warn!(%error, "library artist enrichment pass failed");
            }
            if let Err(error) = resolve_due_links(&state).await {
                tracing::warn!(%error, "download release resolution pass failed");
            }
            tick = tick.wrapping_add(1);
        }
    });
}

pub fn spawn_deduplication_indexer(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = resolve_due_track_indexes(&state).await {
                tracing::warn!(%error, "single deduplication indexing pass failed");
            }
            if let Err(error) = recompute_single_coverages(&state).await {
                tracing::warn!(%error, "single album coverage recomputation failed");
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
    Ok(())
}

async fn seed_single_deduplication(state: &AppState, tracker: &str, group_id: i64) -> Result<()> {
    state.db.enqueue_track_index(tracker, group_id).await?;
    state.db.ensure_single_coverage(tracker, group_id).await
}

async fn resolve_due_track_indexes(state: &Arc<AppState>) -> Result<()> {
    for job in state.db.due_track_indexes(1).await? {
        let Some(tracker) = state.trackers.get(&job.tracker) else {
            continue;
        };
        state
            .db
            .set_track_index_resolving(&job.tracker, job.group_id)
            .await?;
        let result: Result<()> = async {
            let (detail, raw) = tracker.group(job.group_id).await?;
            let index = track_index_from_group(&job.tracker, &detail, &raw);
            cache_release_detail(&state.db, &detail).await?;
            let now = Utc::now();
            state
                .db
                .put_snapshot(
                    &job.tracker,
                    "group",
                    &job.group_id.to_string(),
                    &detail,
                    &raw,
                    now,
                    now + ChronoDuration::hours(24),
                )
                .await?;
            state.db.put_track_index(&index).await?;

            if detail
                .release
                .release_type
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
            {
                state
                    .db
                    .ensure_single_coverage(&job.tracker, job.group_id)
                    .await?;
                let artist_ids = detail
                    .release
                    .artists
                    .iter()
                    .filter(|artist| artist.role == ArtistRole::Primary)
                    .filter_map(|artist| artist.artist_id)
                    .collect::<HashSet<_>>();
                for artist_id in artist_ids {
                    let key = artist_id.to_string();
                    let catalog = if let Some(cached) = state
                        .db
                        .get_snapshot::<ArtistCatalogPage>(&job.tracker, "artist", &key)
                        .await?
                        .filter(|cached| cached.expires_at > Utc::now())
                    {
                        cached.value
                    } else {
                        let (catalog, raw) = tracker.artist_catalog(artist_id).await?;
                        cache_artist_catalog(&state.db, &catalog).await?;
                        let now = Utc::now();
                        state
                            .db
                            .put_snapshot(
                                &job.tracker,
                                "artist",
                                &key,
                                &catalog,
                                &raw,
                                now,
                                now + ChronoDuration::hours(24),
                            )
                            .await?;
                        catalog
                    };
                    seed_catalog_deduplication(state, &job.tracker, &catalog).await?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            let rate_limited = error
                .to_string()
                .to_ascii_lowercase()
                .contains("rate limit");
            state
                .db
                .fail_track_index(&job.tracker, job.group_id, &error.to_string())
                .await?;
            tracing::warn!(
                tracker = %job.tracker,
                group_id = job.group_id,
                %error,
                "release track indexing failed"
            );
            if rate_limited {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }
    Ok(())
}

async fn recompute_single_coverages(state: &Arc<AppState>) -> Result<()> {
    let memberships = state.db.list_catalog_memberships().await?;
    let indexes = state
        .db
        .list_track_indexes()
        .await?
        .into_iter()
        .map(|index| ((index.tracker.clone(), index.group_id), index))
        .collect::<HashMap<_, _>>();
    let mut groups: HashMap<(String, i64), (ArtistCatalogRelease, HashSet<i64>)> = HashMap::new();
    for membership in memberships
        .into_iter()
        .filter(|membership| membership.group.roles.contains(&ArtistCatalogRole::Primary))
    {
        let key = (
            membership.group.release.tracker.clone(),
            membership.group.release.group_id,
        );
        let entry = groups
            .entry(key)
            .or_insert_with(|| (membership.group.clone(), HashSet::new()));
        entry.1.insert(membership.artist_id);
    }
    let albums = groups
        .iter()
        .filter(|(_, (group, _))| {
            group.listed_on_tracker
                && !group.variants.is_empty()
                && group
                    .release
                    .release_type
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("album"))
        })
        .collect::<Vec<_>>();
    for ((tracker, group_id), (_single, single_artists)) in
        groups.iter().filter(|(_, (group, _))| {
            group.listed_on_tracker
                && group
                    .release
                    .release_type
                    .as_deref()
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        })
    {
        state.db.ensure_single_coverage(tracker, *group_id).await?;
        let candidates = albums
            .iter()
            .filter(|((album_tracker, _), (_, artists))| {
                album_tracker.eq_ignore_ascii_case(tracker) && !single_artists.is_disjoint(artists)
            })
            .collect::<Vec<_>>();
        let required = std::iter::once((tracker.clone(), *group_id))
            .chain(candidates.iter().map(|(key, _)| (*key).clone()))
            .collect::<Vec<_>>();
        let states = required
            .iter()
            .filter_map(|key| indexes.get(key))
            .map(|index| index.state.as_str())
            .collect::<Vec<_>>();
        if states.len() != required.len()
            || states
                .iter()
                .any(|state| matches!(*state, "pending" | "resolving"))
        {
            state
                .db
                .put_single_coverage(tracker, *group_id, "pending", None)
                .await?;
            continue;
        }
        if states.contains(&"failed") {
            state
                .db
                .put_single_coverage(tracker, *group_id, "failed", None)
                .await?;
            continue;
        }
        let Some(single_index) = indexes
            .get(&(tracker.clone(), *group_id))
            .and_then(|index| index.index.as_ref())
        else {
            continue;
        };
        let album_indexes = candidates
            .iter()
            .filter_map(|(key, (group, _))| {
                indexes
                    .get(*key)
                    .and_then(|index| index.index.clone())
                    .map(|index| (index, group.clone()))
            })
            .collect::<Vec<_>>();
        let coverage = compute_raw_coverage(single_index, &album_indexes);
        state
            .db
            .put_single_coverage(tracker, *group_id, "ready", Some(&coverage))
            .await?;
    }
    Ok(())
}

async fn scan_download_clients(state: &Arc<AppState>) -> Result<()> {
    const PAGE_SIZE: u32 = 200;
    for (name, client) in &state.download_clients {
        let scan_started_at = Utc::now();
        let mut offset = 0;
        loop {
            let downloads = client.downloads(PAGE_SIZE, offset).await?;
            let count = downloads.len();
            for download in &downloads {
                observe_download(state, download).await?;
            }
            if count < PAGE_SIZE as usize || offset >= 100_000 {
                break;
            }
            offset += PAGE_SIZE;
        }
        state.db.complete_client_scan(name, scan_started_at).await?;
        tracing::debug!(client = %name, indexed = offset, "scanned download client");
    }
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
    state
        .db
        .observe_download(
            &download.live,
            download.announce_host.as_deref(),
            tracker.map(String::as_str),
        )
        .await?;
    Ok(())
}

async fn resolve_due_links(state: &Arc<AppState>) -> Result<()> {
    let mut batches = HashMap::new();
    for link in state.db.due_links(100).await? {
        let Some(tracker_name) = link.tracker.clone() else {
            continue;
        };
        batches
            .entry((tracker_name, link.info_hash.clone()))
            .or_insert_with(Vec::new)
            .push(link);
    }
    for ((tracker_name, info_hash), links) in batches.into_iter().take(5) {
        let Some(tracker) = state.trackers.get(&tracker_name) else {
            continue;
        };
        for link in &links {
            state
                .db
                .set_link_resolving(&link.client, &link.info_hash)
                .await?;
        }
        match tracker.torrent_by_hash(&info_hash).await {
            Ok((canonical, _raw)) => {
                let now = Utc::now();
                state
                    .db
                    .put_canonical(&canonical, now, now + ChronoDuration::hours(24))
                    .await?;
                for link in &links {
                    state
                        .db
                        .set_linked(&link.client, &link.info_hash, &canonical)
                        .await?;
                }
            }
            Err(error) => {
                let message = error.to_string();
                let normalized = message.to_ascii_lowercase();
                let not_found = normalized.contains("not found")
                    || normalized.contains("does not exist")
                    || normalized.contains("bad hash");
                for link in &links {
                    state
                        .db
                        .set_link_failure(&link.client, &link.info_hash, not_found, &message)
                        .await?;
                }
                tracing::warn!(
                    clients = links.len(),
                    tracker = %tracker_name,
                    info_hash = %info_hash,
                    %error,
                    "tracker hash resolution failed"
                );
            }
        }
    }
    Ok(())
}

async fn refresh_canonical_by_hash(
    state: Arc<AppState>,
    tracker_name: String,
    info_hash: String,
) -> Result<()> {
    let tracker = state
        .trackers
        .get(&tracker_name)
        .ok_or_else(|| anyhow!("tracker disappeared from configuration"))?;
    let (canonical, _) = tracker.torrent_by_hash(&info_hash).await?;
    let now = Utc::now();
    state
        .db
        .put_canonical(&canonical, now, now + ChronoDuration::hours(24))
        .await
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

async fn cache_search_canonical(db: &Database, tracker: &str, page: &SearchPage) -> Result<()> {
    let now = Utc::now();
    for group in &page.groups {
        let release = ReleaseSummary {
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
                    can_use_token: torrent.can_use_token,
                    token_eligibility_known: true,
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
    catalog.deduplication = status;
    Ok(())
}

async fn enrich_release_coverages(
    state: &AppState,
    releases: &mut [LibraryRelease],
) -> Result<(), AppError> {
    let preferences = state.db.get_runtime_preferences().await?;
    let mut ignored = DeduplicationIndexStatus::default();
    for release in releases {
        if release
            .release
            .release_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("single"))
        {
            let tracker = release.release.tracker.clone();
            annotate_single_coverage(
                state,
                &tracker,
                release.release.group_id,
                &preferences.release,
                &mut release.release.album_coverage,
                &mut ignored,
            )
            .await?;
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
    page.deduplication = status;
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

async fn enrich_library_artist_credits(state: &Arc<AppState>) -> Result<()> {
    let mut groups = HashSet::new();
    for record in state.db.list_library_records().await? {
        if record
            .canonical
            .value
            .release
            .artists
            .iter()
            .any(|artist| artist.source == ArtistCreditSource::Structured)
        {
            continue;
        }
        let tracker = record.canonical.value.release.tracker.clone();
        let group_id = record.canonical.value.release.group_id;
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

async fn live_downloads_by_hash(
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
    let (detail, raw) = tracker.group(id).await?;
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

async fn refresh_artist_catalog(state: Arc<AppState>, tracker_name: String, id: i64) -> Result<()> {
    let tracker = state
        .trackers
        .get(&tracker_name)
        .ok_or_else(|| anyhow!("tracker disappeared from configuration"))?;
    let (catalog, raw) = tracker.artist_catalog(id).await?;
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
    Provenance {
        tracker: tracker.to_owned(),
        fetched_at,
        cache_age_seconds: (Utc::now() - fetched_at).num_seconds().max(0),
        stale,
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
    paths(
        live, ready, preferences, update_preferences, account, search, downloads, download, create_download,
        library_artists, library_artist, artist_catalog
    ),
    components(schemas(
        Health, Account, Provenance, RuntimePreferences, crate::model::ReleasePreferences,
        SearchPage, TorrentMetadata, DownloadProfile,
        LiveDownloadStatus, crate::model::DownloadDiagnostic, ClientDownloadState,
        CreateDownload, DownloadJob, DownloadState,
        PublicConfig, ArtistRole, ArtistCreditSource, ArtistCredit, ReleaseSummary,
        TorrentVariant, ReleaseDetail, CanonicalDownload,
        DownloadsPage, LibraryAvailability, LibraryCopy, LibraryVariantState, LibraryRelease,
        LibraryArtistSummary, LibraryIndexStatus, LibraryArtistsPage, LibraryArtistPage,
        ArtistCatalogRole, crate::model::ArtistCatalogArtist, ArtistCatalogRelease,
        ArtistCatalogPage,
        ErrorBody, ErrorDetail
    )),
    tags((name = "wotbox", description = "Wotbox API"))
)]
struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    fn credit(id: i64, name: &str, role: ArtistRole) -> ArtistCredit {
        ArtistCredit {
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
                tracker: "ops".into(),
                group_id,
                title: format!("Release {group_id}"),
                artist: None,
                artists,
                year: None,
                artwork: None,
                release_type: None,
                album_coverage: None,
            },
            variants: Vec::new(),
            availability: LibraryAvailability::Present,
            added_at: now,
            provenance: provenance("ops", now, false),
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
}
