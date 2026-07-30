use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, LocalResult, NaiveTime, TimeZone, Utc, Weekday,
};
use chrono_tz::Tz;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    api::{AppState, assign_search_ids, cache_search_canonical},
    model::{
        ChannelConfig, ChannelKind, ChannelPackItem, ChannelRunPhase, ChannelRunStatus,
        LastfmChannelSettings, PackItemPlanState, PlannedDownload, RecommendationMatchState,
        RecommendationSource, RecommendationSubstitution, ReleasePreferences, RuntimePreferences,
        TorrentVariant,
    },
    provider::{
        ProviderFailure, ProviderFailureKind, RequestClass, is_provider_unavailable, retry_after,
    },
    release_matcher::{AUTO_MERGE_THRESHOLD, external_score, normalized},
    tracker::SearchRequest,
};

const APPLE_FEED_ROOT: &str = "https://rss.applemarketingtools.com/api/v2";
const APPLE_SEARCH_ROOT: &str = "https://itunes.apple.com";
const LASTFM_ROOT: &str = "https://ws.audioscrobbler.com/2.0/";

pub fn validate_channel(channel: &ChannelConfig, lastfm_configured: bool) -> Result<()> {
    if !(1..=7).contains(&channel.schedule.weekday) {
        bail!("schedule weekday must be between 1 and 7");
    }
    NaiveTime::parse_from_str(&channel.schedule.time, "%H:%M")
        .context("schedule time must use HH:MM")?;
    channel
        .schedule
        .timezone
        .parse::<Tz>()
        .context("schedule timezone must be an IANA timezone")?;
    match channel.kind {
        ChannelKind::CountryChart => {
            if channel.id != "country_chart" {
                bail!("country chart channel id must be country_chart");
            }
            let country = channel
                .country_chart
                .as_ref()
                .context("country chart settings are required")?
                .country
                .trim();
            if country.len() != 2 || !country.chars().all(|value| value.is_ascii_alphabetic()) {
                bail!("country must be a two-letter code");
            }
        }
        ChannelKind::Lastfm => {
            if channel.id != "lastfm" {
                bail!("Last.fm channel id must be lastfm");
            }
            let settings = channel
                .lastfm
                .as_ref()
                .context("Last.fm settings are required")?;
            if channel.enabled && (!lastfm_configured || settings.username.trim().is_empty()) {
                bail!(
                    "Last.fm username and API key must be configured before enabling the channel"
                );
            }
            if !matches!(
                settings.period.as_str(),
                "7day" | "1month" | "3month" | "6month" | "12month" | "overall"
            ) {
                bail!("unsupported Last.fm seed period");
            }
            if !(1..=100).contains(&settings.pack_size) {
                bail!("Last.fm pack size must be between 1 and 100");
            }
            if settings.suppression_packs > 52 {
                bail!("Last.fm suppression window cannot exceed 52 packs");
            }
            let country = settings.catalog_country.trim();
            if country.len() != 2 || !country.chars().all(|value| value.is_ascii_alphabetic()) {
                bail!("Last.fm catalog country must be a two-letter code");
            }
        }
    }
    Ok(())
}

pub fn validate_channel_refresh(channel: &ChannelConfig, lastfm_configured: bool) -> Result<()> {
    validate_channel(channel, lastfm_configured)?;
    if matches!(channel.kind, ChannelKind::Lastfm) {
        let settings = channel
            .lastfm
            .as_ref()
            .context("Last.fm settings are required")?;
        if !lastfm_configured {
            bail!("Last.fm API key must be configured before refreshing");
        }
        if settings.username.trim().is_empty() {
            bail!("Last.fm username is required before refreshing");
        }
    }
    Ok(())
}

pub fn next_occurrence(channel: &ChannelConfig, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
    scheduled_occurrence(channel, after, false)
}

pub fn channel_is_due(channel: &ChannelConfig, now: DateTime<Utc>) -> Result<bool> {
    if !channel.enabled {
        return Ok(false);
    }
    if channel.last_error.is_some()
        && let Some(last_attempt) = channel.last_attempt_at
    {
        return Ok(last_attempt + retry_delay(channel.failure_count) <= now);
    }
    let since = channel
        .last_attempt_at
        .or(channel.last_successful_at)
        .unwrap_or(now - ChronoDuration::days(8));
    Ok(next_occurrence(channel, since)? <= now)
}

pub fn next_refresh_at(channel: &ChannelConfig, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    if channel.last_error.is_some()
        && let Some(last_attempt) = channel.last_attempt_at
    {
        return Ok(last_attempt + retry_delay(channel.failure_count));
    }
    next_occurrence(channel, now)
}

fn retry_delay(failure_count: u32) -> ChronoDuration {
    let exponent = failure_count.saturating_sub(1).min(6);
    ChronoDuration::minutes(15 * (1_i64 << exponent))
}

fn scheduled_occurrence(
    channel: &ChannelConfig,
    after: DateTime<Utc>,
    inclusive: bool,
) -> Result<DateTime<Utc>> {
    let timezone = channel.schedule.timezone.parse::<Tz>()?;
    let time = NaiveTime::parse_from_str(&channel.schedule.time, "%H:%M")?;
    let target = weekday(channel.schedule.weekday)?;
    let local_after = after.with_timezone(&timezone);
    for offset in 0..=8 {
        let date = local_after.date_naive() + ChronoDuration::days(offset);
        if date.weekday() != target {
            continue;
        }
        let naive = date.and_time(time);
        let local = resolve_local_time(timezone, naive)?;
        let utc = local.with_timezone(&Utc);
        if utc > after || (inclusive && utc == after) {
            return Ok(utc);
        }
    }
    bail!("could not calculate the next channel schedule")
}

fn resolve_local_time(timezone: Tz, mut naive: chrono::NaiveDateTime) -> Result<DateTime<Tz>> {
    for _ in 0..=120 {
        match timezone.from_local_datetime(&naive) {
            LocalResult::Single(value) => return Ok(value),
            LocalResult::Ambiguous(first, _) => return Ok(first),
            LocalResult::None => naive += ChronoDuration::minutes(1),
        }
    }
    bail!("scheduled local time does not exist")
}

fn weekday(value: u8) -> Result<Weekday> {
    Ok(match value {
        1 => Weekday::Mon,
        2 => Weekday::Tue,
        3 => Weekday::Wed,
        4 => Weekday::Thu,
        5 => Weekday::Fri,
        6 => Weekday::Sat,
        7 => Weekday::Sun,
        _ => bail!("invalid weekday"),
    })
}

pub async fn refresh_channel(
    state: Arc<AppState>,
    channel: ChannelConfig,
    run_id: uuid::Uuid,
) -> Result<(uuid::Uuid, ChannelRunStatus)> {
    let (sources, mut partial, title) = match channel.kind {
        ChannelKind::CountryChart => {
            let settings = channel
                .country_chart
                .as_ref()
                .context("country chart settings disappeared")?;
            (
                fetch_apple_chart(&state, &settings.country).await?,
                false,
                format!("{} Top 100 Albums", settings.country.to_ascii_uppercase()),
            )
        }
        ChannelKind::Lastfm => {
            let settings = channel
                .lastfm
                .as_ref()
                .context("Last.fm settings disappeared")?;
            let (items, was_partial) =
                fetch_lastfm_recommendations(&state, settings, run_id).await?;
            (
                items,
                was_partial,
                format!("Last.fm discovery for {}", settings.username),
            )
        }
    };
    if sources.is_empty() {
        bail!("recommendation source returned no usable albums");
    }

    let preferences = state.db.get_runtime_preferences().await?;
    let fingerprint = preference_fingerprint(&state, &preferences)?;
    let mut items = Vec::with_capacity(sources.len());
    let source_total = sources.len() as u32;
    state
        .db
        .update_channel_run_progress(
            run_id,
            ChannelRunPhase::Matching,
            0,
            Some(source_total),
            Some("Matching recommendations on configured trackers"),
        )
        .await?;
    for (index, source) in sources.into_iter().enumerate() {
        state
            .db
            .update_channel_run_progress(
                run_id,
                ChannelRunPhase::Matching,
                index as u32,
                Some(source_total),
                Some(&format!("Matching {} — {}", source.artist, source.title)),
            )
            .await?;
        let item = match resolve_source(&state, source.clone(), &preferences).await {
            Ok(item) => item,
            Err(error) => {
                partial = true;
                ChannelPackItem {
                    ordinal: source.rank,
                    source,
                    match_state: RecommendationMatchState::Error,
                    release: None,
                    variants: Vec::new(),
                    plan_state: PackItemPlanState::SourceError,
                    plan: None,
                    reason: Some(format!("Tracker lookup failed: {error}")),
                    job_id: None,
                    job: None,
                }
            }
        };
        items.push(item);
    }
    state
        .db
        .update_channel_run_progress(
            run_id,
            ChannelRunPhase::Planning,
            source_total,
            Some(source_total),
            Some("Applying download rules and pack constraints"),
        )
        .await?;
    coordinate_pack_plan(&state, &mut items, &preferences).await;
    state
        .db
        .update_channel_run_progress(
            run_id,
            ChannelRunPhase::Saving,
            source_total,
            Some(source_total),
            Some("Saving recommendation pack"),
        )
        .await?;
    let pack_id = state
        .db
        .create_channel_pack(&channel.id, &title, partial, &fingerprint, &items)
        .await?;
    Ok((
        pack_id,
        if partial {
            ChannelRunStatus::Partial
        } else {
            ChannelRunStatus::Successful
        },
    ))
}

pub async fn replan_items(
    state: &Arc<AppState>,
    items: Vec<ChannelPackItem>,
) -> Result<Vec<ChannelPackItem>> {
    let preferences = state.db.get_runtime_preferences().await?;
    let mut replanned = Vec::with_capacity(items.len());
    for item in items {
        replanned.push(resolve_source(state, item.source, &preferences).await?);
    }
    coordinate_pack_plan(state, &mut replanned, &preferences).await;
    Ok(replanned)
}

pub fn preference_fingerprint(
    state: &AppState,
    preferences: &RuntimePreferences,
) -> Result<String> {
    const PLAN_COST_MODEL: &str = "token-cost-v2-ops-320-mib";
    let mut profiles = state.profiles.values().cloned().collect::<Vec<_>>();
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    let value = serde_json::to_vec(&(PLAN_COST_MODEL, preferences, profiles))?;
    Ok(hex::encode(Sha256::digest(value)))
}

async fn fetch_apple_chart(state: &AppState, country: &str) -> Result<Vec<RecommendationSource>> {
    let country = country.trim().to_ascii_lowercase();
    let url = format!("{APPLE_FEED_ROOT}/{country}/music/most-played/100/albums.json");
    let body = provider_json(
        state,
        "apple",
        RequestClass::Scheduled,
        state.source_client.get(url),
    )
    .await?;
    parse_apple_chart(&body)
}

fn parse_apple_chart(body: &Value) -> Result<Vec<RecommendationSource>> {
    let results = body
        .pointer("/feed/results")
        .and_then(Value::as_array)
        .context("Apple chart omitted feed results")?;
    Ok(results
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            Some(RecommendationSource {
                id: format!("apple:{}", item.get("id")?.as_str()?),
                rank: (index + 1) as u32,
                artist: item.get("artistName")?.as_str()?.to_owned(),
                title: item.get("name")?.as_str()?.to_owned(),
                year: item
                    .get("releaseDate")
                    .and_then(Value::as_str)
                    .and_then(|value| value.get(..4))
                    .and_then(|value| value.parse().ok()),
                artwork: item
                    .get("artworkUrl100")
                    .and_then(Value::as_str)
                    .map(|value| value.replace("100x100bb", "600x600bb")),
                url: item.get("url").and_then(Value::as_str).map(str::to_owned),
                mbid: None,
                score: None,
                catalog_country: None,
                substituted_from: None,
            })
        })
        .collect())
}

async fn fetch_lastfm_recommendations(
    state: &AppState,
    settings: &LastfmChannelSettings,
    run_id: uuid::Uuid,
) -> Result<(Vec<RecommendationSource>, bool)> {
    let api_key = state
        .lastfm_api_key
        .as_deref()
        .context("Last.fm API key is not configured")?;
    let top_artists = lastfm_call(
        state,
        api_key,
        "user.getTopArtists",
        &[
            ("user", settings.username.as_str()),
            ("period", settings.period.as_str()),
            ("limit", "20"),
        ],
    )
    .await?;
    let seeds = json_array(&top_artists, "/topartists/artist");
    if seeds.is_empty() {
        bail!("Last.fm returned no top artists for this user");
    }

    let mut partial = false;
    let known = match lastfm_call(
        state,
        api_key,
        "user.getTopAlbums",
        &[
            ("user", settings.username.as_str()),
            ("period", "overall"),
            ("limit", "500"),
        ],
    )
    .await
    {
        Ok(value) => json_array(&value, "/topalbums/album")
            .into_iter()
            .filter_map(|album| {
                Some(normalized_pair(
                    album.pointer("/artist/name")?.as_str()?,
                    album.get("name")?.as_str()?,
                ))
            })
            .collect::<HashSet<_>>(),
        Err(error) => {
            tracing::warn!(%error, "Last.fm listened-album exclusion unavailable");
            partial = true;
            HashSet::new()
        }
    };
    let recent = state
        .db
        .recent_channel_sources("lastfm", settings.suppression_packs as u64)
        .await?
        .into_iter()
        .map(|source| normalized_pair(&source.artist, &source.title))
        .collect::<HashSet<_>>();
    let seed_names = seeds
        .iter()
        .filter_map(|seed| seed.get("name").and_then(Value::as_str))
        .map(normalized)
        .collect::<HashSet<_>>();

    let mut artists: HashMap<String, (String, f64)> = HashMap::new();
    for (index, seed) in seeds.iter().enumerate() {
        let Some(seed_name) = seed.get("name").and_then(Value::as_str) else {
            continue;
        };
        let response = match lastfm_call(
            state,
            api_key,
            "artist.getSimilar",
            &[("artist", seed_name), ("limit", "10"), ("autocorrect", "1")],
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                if is_provider_unavailable(&error) {
                    return Err(error);
                }
                tracing::warn!(artist = seed_name, %error, "Last.fm similar artists unavailable");
                partial = true;
                continue;
            }
        };
        let weight = 1.0 / ((index + 1) as f64).sqrt();
        for artist in json_array(&response, "/similarartists/artist") {
            let Some(name) = artist.get("name").and_then(Value::as_str) else {
                continue;
            };
            let key = normalized(name);
            if seed_names.contains(&key) {
                continue;
            }
            let similarity = artist
                .get("match")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<f64>().ok())
                .or_else(|| artist.get("match").and_then(Value::as_f64))
                .unwrap_or_default();
            let entry = artists.entry(key).or_insert_with(|| (name.to_owned(), 0.0));
            entry.1 += similarity * weight;
        }
    }
    let mut artists = artists.into_values().collect::<Vec<_>>();
    artists.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal));

    let mut recommendations = Vec::new();
    let mut seen = HashSet::new();
    for (artist, artist_score) in artists.into_iter().take(100) {
        if recommendations.len() >= settings.pack_size as usize {
            break;
        }
        state
            .db
            .update_channel_run_progress(
                run_id,
                ChannelRunPhase::Discovering,
                recommendations.len() as u32,
                Some(settings.pack_size as u32),
                Some(&format!("Checking recommendations from {artist}")),
            )
            .await?;
        let response = match lastfm_call(
            state,
            api_key,
            "artist.getTopAlbums",
            &[("artist", &artist), ("limit", "5"), ("autocorrect", "1")],
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                if is_provider_unavailable(&error) {
                    return Err(error);
                }
                tracing::warn!(%artist, %error, "Last.fm artist albums unavailable");
                partial = true;
                continue;
            }
        };
        let album = json_array(&response, "/topalbums/album")
            .into_iter()
            .find(|album| {
                let Some(title) = album.get("name").and_then(Value::as_str) else {
                    return false;
                };
                let pair = normalized_pair(&artist, title);
                !known.contains(&pair) && !recent.contains(&pair) && seen.insert(pair)
            });
        let Some(album) = album else {
            continue;
        };
        let title = album
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mbid = nonempty_string(album.get("mbid"));
        let id = mbid
            .as_ref()
            .map(|value| format!("lastfm:{value}"))
            .unwrap_or_else(|| {
                let digest = Sha256::digest(format!("{artist}\0{title}").as_bytes());
                format!("lastfm:{}", &hex::encode(digest)[..20])
            });
        recommendations.push(RecommendationSource {
            id,
            rank: (recommendations.len() + 1) as u32,
            artist,
            title,
            year: None,
            artwork: largest_image(album),
            url: album.get("url").and_then(Value::as_str).map(str::to_owned),
            mbid,
            score: Some(artist_score),
            catalog_country: Some(settings.catalog_country.to_ascii_uppercase()),
            substituted_from: None,
        });
    }
    if recommendations.len() < settings.pack_size as usize {
        partial = true;
    }
    Ok((recommendations, partial))
}

async fn lastfm_call(
    state: &AppState,
    api_key: &str,
    method: &str,
    params: &[(&str, &str)],
) -> Result<Value> {
    let mut query = vec![("method", method), ("api_key", api_key), ("format", "json")];
    query.extend_from_slice(params);
    state
        .providers
        .execute("lastfm", RequestClass::Scheduled, || async {
            let response = state
                .source_client
                .get(LASTFM_ROOT)
                .query(&query)
                .send()
                .await
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Transient, error))?;
            let status = response.status();
            let retry = retry_after(&response);
            let value: Value = response
                .json()
                .await
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Transient, error))?;
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(ProviderFailure::new(
                    ProviderFailureKind::RateLimited,
                    "Last.fm rate limit exceeded",
                )
                .retry_after(retry));
            }
            if !status.is_success() {
                let kind = if status.is_server_error() {
                    ProviderFailureKind::Transient
                } else if matches!(
                    status,
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
                ) {
                    ProviderFailureKind::Authentication
                } else {
                    ProviderFailureKind::Permanent
                };
                return Err(ProviderFailure::new(
                    kind,
                    format!("Last.fm returned HTTP {status}"),
                ));
            }
            if let Some(error) = value.get("error") {
                let code = error.as_i64().unwrap_or_default();
                let message = value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("request failed");
                let kind = match code {
                    29 => ProviderFailureKind::RateLimited,
                    10 | 26 => ProviderFailureKind::Authentication,
                    11 | 16 => ProviderFailureKind::Transient,
                    _ => ProviderFailureKind::Permanent,
                };
                return Err(ProviderFailure::new(
                    kind,
                    format!("Last.fm error {code}: {message}"),
                ));
            }
            Ok(value)
        })
        .await
        .map_err(Into::into)
}

#[cfg(test)]
async fn get_json_with_retry(request: reqwest::RequestBuilder) -> Result<Value> {
    let mut last_error = None;
    for attempt in 0..3 {
        let Some(next) = request.try_clone() else {
            bail!("recommendation request could not be retried");
        };
        match next.send().await {
            Ok(response) if response.status().is_success() => {
                return response
                    .json()
                    .await
                    .context("decode recommendation response");
            }
            Ok(response)
                if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || response.status().is_server_error() =>
            {
                last_error = Some(anyhow::anyhow!(
                    "recommendation source returned HTTP {}",
                    response.status()
                ));
            }
            Ok(response) => bail!("recommendation source returned HTTP {}", response.status()),
            Err(error) => last_error = Some(error.into()),
        }
        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(250 * (1 << attempt))).await;
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("recommendation request failed")))
}

async fn resolve_source(
    state: &Arc<AppState>,
    source: RecommendationSource,
    preferences: &RuntimePreferences,
) -> Result<ChannelPackItem> {
    if let Some(item) = resolve_tracker_source(state, source.clone(), preferences).await? {
        return Ok(item);
    }
    if source.id.starts_with("lastfm:")
        && source.substituted_from.is_none()
        && let Some(candidates) = apple_containing_releases(state, &source).await?
    {
        for candidate in candidates {
            if let Some(item) = resolve_tracker_source(state, candidate, preferences).await? {
                return Ok(item);
            }
        }
    }
    Ok(unresolved_item(
        source,
        RecommendationMatchState::Unmatched,
        PackItemPlanState::Unmatched,
        "No matching Album or EP is currently available on a configured tracker",
    ))
}

async fn resolve_tracker_source(
    state: &Arc<AppState>,
    source: RecommendationSource,
    preferences: &RuntimePreferences,
) -> Result<Option<ChannelPackItem>> {
    let mut groups = Vec::new();
    let mut queries = vec![source.title.clone()];
    if let Some(base) = base_edition_title(&source.title)
        && !queries
            .iter()
            .any(|value| value.eq_ignore_ascii_case(&base))
    {
        queries.push(base);
    }
    let mut tracker_errors = Vec::new();
    let mut successful_lookups = 0usize;
    for (name, tracker) in &state.trackers {
        for query in &queries {
            let request = SearchRequest {
                query: Some(query.clone()),
                artist: Some(source.artist.clone()),
                release_type: None,
                year: source.year,
                format: None,
                encoding: None,
                media: None,
                page: Some(1),
            };
            match tracker
                .search_with_class(&request, RequestClass::Scheduled)
                .await
            {
                Ok((mut page, _)) => {
                    successful_lookups += 1;
                    cache_search_canonical(&state.db, name, &page).await?;
                    assign_search_ids(&state.db, &mut page).await?;
                    groups.extend(page.groups.into_iter().filter(|group| {
                        group.release_type.as_deref().is_some_and(|value| {
                            value.eq_ignore_ascii_case("album") || value.eq_ignore_ascii_case("ep")
                        })
                    }));
                }
                Err(error) => {
                    tracing::warn!(tracker = name, %error, "channel tracker lookup failed");
                    tracker_errors.push(format!("{name}: {error}"));
                }
            }
        }
    }
    groups.sort_by_key(|group| group.id);
    groups.dedup_by_key(|group| group.id);
    let mut matches = groups
        .into_iter()
        .filter_map(|group| {
            let score = external_score(
                &source.title,
                &source.artist,
                source.year,
                &group.name,
                group.artist.as_deref(),
                group.year,
                group.release_type.as_deref(),
            );
            (score >= AUTO_MERGE_THRESHOLD).then_some((score, group))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(Ordering::Equal));
    let Some((best_score, best)) = matches.first().cloned() else {
        if successful_lookups == 0 && !tracker_errors.is_empty() {
            bail!("tracker lookup incomplete: {}", tracker_errors.join("; "));
        }
        return Ok(None);
    };
    if let Some((second_score, second)) = matches.get(1)
        && best.id != second.id
        && best_score - second_score < 0.03
    {
        return Ok(Some(unresolved_item(
            source,
            RecommendationMatchState::Ambiguous,
            PackItemPlanState::Ambiguous,
            "Multiple tracker releases matched with similar confidence",
        )));
    }
    let release_id = best
        .id
        .context("matched tracker release has no canonical id")?;
    resolve_release(state, source, release_id, preferences)
        .await
        .map(Some)
}

fn base_edition_title(title: &str) -> Option<String> {
    const SUFFIXES: [&str; 22] = [
        " (super deluxe edition)",
        " (super deluxe)",
        " (deluxe edition)",
        " (deluxe)",
        " (expanded edition)",
        " (expanded)",
        " (extended edition)",
        " (extended)",
        " (anniversary edition)",
        " (bonus track version)",
        " [super deluxe]",
        " [deluxe edition]",
        " [deluxe]",
        " [expanded edition]",
        " [expanded]",
        " - super deluxe edition",
        " - deluxe edition",
        " - expanded edition",
        ": super deluxe edition",
        ": deluxe edition",
        ": expanded edition",
        " deluxe",
    ];
    let trimmed = title.trim();
    let lower = trimmed.to_ascii_lowercase();
    SUFFIXES.iter().find_map(|suffix| {
        lower
            .strip_suffix(suffix)
            .map(|base| trimmed[..base.len()].trim().to_owned())
            .filter(|base| !base.is_empty())
    })
}

#[derive(Debug, Clone)]
struct AppleCollection {
    id: i64,
    artist: String,
    title: String,
    year: Option<i64>,
    artwork: Option<String>,
    url: Option<String>,
    is_ep: bool,
}

async fn apple_containing_releases(
    state: &Arc<AppState>,
    source: &RecommendationSource,
) -> Result<Option<Vec<RecommendationSource>>> {
    let country = source.catalog_country.as_deref().unwrap_or("AU");
    let cache_key = hex::encode(Sha256::digest(
        format!(
            "{}\0{}\0{}",
            country.to_ascii_uppercase(),
            normalized(&source.artist),
            normalized(&source.title)
        )
        .as_bytes(),
    ));
    if let Some(cached) = state
        .db
        .get_snapshot::<Vec<Value>>("apple", "song-collections", &cache_key)
        .await?
        && cached.expires_at > Utc::now()
    {
        return apple_sources_from_cache(source, cached.value).map(Some);
    }

    let search = apple_json(
        state,
        state
            .source_client
            .get(format!("{APPLE_SEARCH_ROOT}/search"))
            .query(&[
                ("term", format!("{} {}", source.artist, source.title)),
                ("country", country.to_ascii_uppercase()),
                ("media", "music".to_owned()),
                ("entity", "song".to_owned()),
                ("attribute", "songTerm".to_owned()),
                ("limit", "50".to_owned()),
            ]),
    )
    .await?;
    let mut ids = search
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("artistName")
                .and_then(Value::as_str)
                .is_some_and(|artist| normalized(artist) == normalized(&source.artist))
                && item
                    .get("trackName")
                    .and_then(Value::as_str)
                    .is_some_and(|title| normalized(title) == normalized(&source.title))
        })
        .filter_map(|item| item.get("collectionId").and_then(Value::as_i64))
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids.truncate(20);

    let mut values = Vec::new();
    if !ids.is_empty() {
        let ids = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let lookup = apple_json(
            state,
            state
                .source_client
                .get(format!("{APPLE_SEARCH_ROOT}/lookup"))
                .query(&[("id", ids), ("country", country.to_ascii_uppercase())]),
        )
        .await?;
        values = lookup
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
    }
    let now = Utc::now();
    let ttl = if values.is_empty() {
        ChronoDuration::hours(24)
    } else {
        ChronoDuration::days(7)
    };
    state
        .db
        .put_snapshot(
            "apple",
            "song-collections",
            &cache_key,
            &values,
            &Value::Array(values.clone()),
            now,
            now + ttl,
        )
        .await?;
    apple_sources_from_cache(source, values).map(Some)
}

async fn apple_json(state: &AppState, request: reqwest::RequestBuilder) -> Result<Value> {
    provider_json(state, "apple", RequestClass::Scheduled, request).await
}

async fn provider_json(
    state: &AppState,
    provider: &str,
    class: RequestClass,
    request: reqwest::RequestBuilder,
) -> Result<Value> {
    state
        .providers
        .execute(provider, class, || async {
            let response = request
                .send()
                .await
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Transient, error))?;
            let status = response.status();
            let retry = retry_after(&response);
            if !status.is_success() {
                let kind = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    ProviderFailureKind::RateLimited
                } else if matches!(
                    status,
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
                ) {
                    ProviderFailureKind::Authentication
                } else if status.is_server_error() {
                    ProviderFailureKind::Transient
                } else {
                    ProviderFailureKind::Permanent
                };
                return Err(ProviderFailure::new(
                    kind,
                    format!("recommendation source returned HTTP {status}"),
                )
                .retry_after(retry));
            }
            response
                .json()
                .await
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Transient, error))
        })
        .await
        .map_err(Into::into)
}

fn apple_sources_from_cache(
    source: &RecommendationSource,
    values: Vec<Value>,
) -> Result<Vec<RecommendationSource>> {
    let mut collections = values
        .into_iter()
        .filter_map(|item| {
            let artist = item.get("artistName")?.as_str()?.to_owned();
            if normalized(&artist) != normalized(&source.artist) {
                return None;
            }
            let title = item.get("collectionName")?.as_str()?.to_owned();
            let normalized_title = normalized(&title);
            let track_count = item.get("trackCount").and_then(Value::as_i64).unwrap_or(0);
            if track_count <= 1 || normalized_title.ends_with(" single") {
                return None;
            }
            let is_ep = normalized_title.ends_with(" ep") || (2..=8).contains(&track_count);
            Some(AppleCollection {
                id: item.get("collectionId")?.as_i64()?,
                artist,
                title,
                year: item
                    .get("releaseDate")
                    .and_then(Value::as_str)
                    .and_then(|date| date.get(..4))
                    .and_then(|year| year.parse().ok()),
                artwork: item
                    .get("artworkUrl100")
                    .and_then(Value::as_str)
                    .map(|url| url.replace("100x100bb", "600x600bb")),
                url: item
                    .get("collectionViewUrl")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                is_ep,
            })
        })
        .collect::<Vec<_>>();
    collections.sort_by(|left, right| {
        left.is_ep
            .cmp(&right.is_ep)
            .then_with(|| right.year.cmp(&left.year))
    });
    collections.dedup_by_key(|item| item.id);
    Ok(collections
        .into_iter()
        .map(|collection| RecommendationSource {
            id: source.id.clone(),
            rank: source.rank,
            artist: collection.artist,
            title: collection.title,
            year: collection.year,
            artwork: collection.artwork.or_else(|| source.artwork.clone()),
            url: collection.url,
            mbid: None,
            score: source.score,
            catalog_country: source.catalog_country.clone(),
            substituted_from: Some(RecommendationSubstitution {
                title: source.title.clone(),
                url: source.url.clone(),
                mbid: source.mbid.clone(),
                release_type: "single".into(),
            }),
        })
        .collect())
}

pub async fn resolve_release(
    state: &Arc<AppState>,
    source: RecommendationSource,
    release_id: uuid::Uuid,
    preferences: &RuntimePreferences,
) -> Result<ChannelPackItem> {
    let detail = state
        .db
        .get_release_detail(release_id)
        .await?
        .context("canonical release detail is unavailable")?;
    if !detail.release.release_type.as_deref().is_some_and(|value| {
        value.eq_ignore_ascii_case("album") || value.eq_ignore_ascii_case("ep")
    }) {
        bail!("only Album and EP releases can be attached to a channel pack");
    }
    let mut variants = detail.variants;
    for variant in &mut variants {
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
    let (owned, downloading) = state.db.release_download_flags(release_id).await?;
    let (plan_state, plan, reason) = if owned {
        (
            PackItemPlanState::AlreadyOwned,
            None,
            Some("Already present in the Library".into()),
        )
    } else if downloading {
        (
            PackItemPlanState::AlreadyDownloading,
            None,
            Some("A download for this release is already active".into()),
        )
    } else {
        plan_variant(
            &state.profiles,
            &variants,
            &preferences.release,
            u32::MAX,
            &source.title,
        )
    };
    Ok(ChannelPackItem {
        ordinal: source.rank,
        source,
        match_state: RecommendationMatchState::Matched,
        release: Some(detail.release),
        variants,
        plan_state,
        plan,
        reason,
        job_id: None,
        job: None,
    })
}

fn plan_variant(
    profiles: &HashMap<String, crate::model::DownloadProfile>,
    variants: &[TorrentVariant],
    preferences: &ReleasePreferences,
    token_budget: u32,
    edition_intent: &str,
) -> (PackItemPlanState, Option<PlannedDownload>, Option<String>) {
    let mut candidates = variants
        .iter()
        .filter_map(|variant| {
            let eligibility = variant.eligibility.as_ref()?;
            let policy = preferences.tracker_policy(&variant.tracker);
            let token_cost = eligibility.token_cost.unwrap_or_default();
            if !eligibility.eligible
                || (eligibility.requires_token
                    && (!policy.auto_use_tokens || token_cost > token_budget))
            {
                return None;
            }
            Some((variant, eligibility.requires_token, token_cost))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left, _, _), (right, _, _)| {
        compare_variants(left, right, preferences, edition_intent)
    });
    let Some((variant, use_token, token_cost)) = candidates.first() else {
        return (
            PackItemPlanState::PolicyBlocked,
            None,
            Some("No torrent variant satisfies the configured tracker and quality rules".into()),
        );
    };
    let profile = preferences
        .tracker_policy(&variant.tracker)
        .download_profile
        .filter(|profile| profiles.contains_key(profile));
    let Some(profile) = profile else {
        return (
            PackItemPlanState::NoProfile,
            None,
            Some("No download profile is configured".into()),
        );
    };
    (
        PackItemPlanState::Executable,
        Some(PlannedDownload {
            tracker: variant.tracker.clone(),
            torrent_id: variant.torrent_id,
            profile,
            use_token: *use_token,
            token_cost: if *use_token { *token_cost } else { 0 },
            size: variant.size,
            format: variant.format.clone(),
            encoding: variant.encoding.clone(),
            media: variant.media.clone(),
        }),
        None,
    )
}

pub async fn coordinate_pack_plan(
    state: &AppState,
    items: &mut [ChannelPackItem],
    preferences: &RuntimePreferences,
) {
    let mut remaining_by_client = HashMap::new();
    for (name, client) in &state.download_clients {
        let capacity = match client.free_space().await {
            Ok(value) if value >= 0 => Some(value),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(client = name, %error, "channel planner could not read free space");
                None
            }
        };
        remaining_by_client.insert(name.clone(), capacity);
    }

    apply_pack_constraints(
        &state.profiles,
        items,
        preferences,
        &mut remaining_by_client,
    );
}

fn apply_pack_constraints(
    profiles: &HashMap<String, crate::model::DownloadProfile>,
    items: &mut [ChannelPackItem],
    preferences: &RuntimePreferences,
    remaining_by_client: &mut HashMap<String, Option<i64>>,
) {
    let mut seen_releases = HashSet::new();
    let mut seen_torrents = HashSet::new();
    let mut token_uses: HashMap<String, u32> = HashMap::new();
    for item in items {
        if item.plan_state != PackItemPlanState::Executable {
            continue;
        }
        if let Some(release_id) = item.release.as_ref().and_then(|release| release.id)
            && !seen_releases.insert(release_id)
        {
            item.plan_state = PackItemPlanState::Duplicate;
            item.plan = None;
            item.reason =
                Some("Another recommendation in this pack resolves to the same release".into());
            continue;
        }

        let Some(mut plan) = item.plan.clone() else {
            continue;
        };
        let policy = preferences.release.tracker_policy(&plan.tracker);
        let used = token_uses.get(&plan.tracker).copied().unwrap_or_default();
        if plan.use_token && used.saturating_add(plan.token_cost) > policy.auto_token_limit {
            let remaining = policy.auto_token_limit.saturating_sub(used);
            let (state_value, replacement, reason) = plan_variant(
                profiles,
                &item.variants,
                &preferences.release,
                remaining,
                &item.source.title,
            );
            if let Some(replacement) = replacement {
                plan = replacement;
                item.plan_state = state_value;
                item.reason = reason;
            } else {
                item.plan_state = PackItemPlanState::TokenBudgetExceeded;
                item.plan = None;
                item.reason = Some(format!(
                    "{} requires {} token{}; {} of {} already allocated for this pack",
                    plan.tracker.to_ascii_uppercase(),
                    plan.token_cost,
                    if plan.token_cost == 1 { "" } else { "s" },
                    used,
                    policy.auto_token_limit,
                ));
                continue;
            }
        }

        if !seen_torrents.insert((plan.tracker.clone(), plan.torrent_id, plan.profile.clone())) {
            item.plan_state = PackItemPlanState::Duplicate;
            item.plan = None;
            item.reason = Some("Another recommendation already selected this torrent".into());
            continue;
        }

        let Some(profile) = profiles.get(&plan.profile) else {
            item.plan_state = PackItemPlanState::NoProfile;
            item.plan = None;
            item.reason = Some("The selected download profile is no longer configured".into());
            continue;
        };
        if let (Some(size), Some(Some(remaining))) = (
            plan.size.filter(|size| *size > 0),
            remaining_by_client.get_mut(&profile.client),
        ) {
            if size > *remaining {
                item.plan_state = PackItemPlanState::CapacityBlocked;
                item.plan = None;
                item.reason = Some(format!(
                    "Not enough free space remains on download client {}",
                    profile.client
                ));
                continue;
            }
            *remaining -= size;
        }
        if plan.use_token {
            *token_uses.entry(plan.tracker.clone()).or_default() += plan.token_cost;
        }
        item.plan = Some(plan);
        item.reason = None;
    }
}

fn compare_variants(
    left: &TorrentVariant,
    right: &TorrentVariant,
    preferences: &ReleasePreferences,
    edition_intent: &str,
) -> Ordering {
    let mut ordering = Ordering::Equal;
    for criterion in &preferences.variant_sort_order {
        let next = match criterion {
            crate::model::VariantSortCriterion::Quality => {
                quality_rank(left, preferences).cmp(&quality_rank(right, preferences))
            }
            crate::model::VariantSortCriterion::Tracker => tracker_rank(&left.tracker, preferences)
                .cmp(&tracker_rank(&right.tracker, preferences)),
            crate::model::VariantSortCriterion::Media => {
                media_rank(left.media.as_deref(), preferences)
                    .cmp(&media_rank(right.media.as_deref(), preferences))
            }
            crate::model::VariantSortCriterion::Edition => {
                edition_rank(left.remaster_title.as_deref(), edition_intent).cmp(&edition_rank(
                    right.remaster_title.as_deref(),
                    edition_intent,
                ))
            }
        };
        ordering = ordering.then(next);
    }
    ordering
        .then_with(|| {
            right
                .seeders
                .unwrap_or_default()
                .cmp(&left.seeders.unwrap_or_default())
        })
        .then_with(|| left.torrent_id.cmp(&right.torrent_id))
}

fn tracker_rank(tracker: &str, preferences: &ReleasePreferences) -> usize {
    preferences
        .tracker_order
        .iter()
        .position(|value| value.eq_ignore_ascii_case(tracker))
        .unwrap_or(preferences.tracker_order.len())
}

fn quality_rank(variant: &TorrentVariant, preferences: &ReleasePreferences) -> usize {
    preferences.quality_rank(variant.format.as_deref(), variant.encoding.as_deref())
}

fn media_rank(media: Option<&str>, preferences: &ReleasePreferences) -> usize {
    preferences.media_rank(media)
}

fn edition_rank(remaster_title: Option<&str>, intent: &str) -> usize {
    let title = remaster_title.unwrap_or_default().to_ascii_lowercase();
    let intent = intent.to_ascii_lowercase();
    let enhanced = [
        "super deluxe",
        "deluxe",
        "expanded",
        "extended",
        "anniversary",
        "bonus track",
    ];
    let alternates = ["instrumental", "remix", "live", "karaoke"];
    let requested = enhanced
        .iter()
        .find(|label| intent.contains(**label))
        .copied();
    if requested.is_some_and(|label| title.contains(label)) {
        0
    } else if enhanced.iter().any(|label| title.contains(label)) {
        if requested.is_some() { 1 } else { 0 }
    } else if alternates.iter().any(|label| title.contains(label)) {
        3
    } else if requested.is_some() {
        2
    } else {
        1
    }
}

fn unresolved_item(
    source: RecommendationSource,
    match_state: RecommendationMatchState,
    plan_state: PackItemPlanState,
    reason: &str,
) -> ChannelPackItem {
    ChannelPackItem {
        ordinal: source.rank,
        source,
        match_state,
        release: None,
        variants: Vec::new(),
        plan_state,
        plan: None,
        reason: Some(reason.into()),
        job_id: None,
        job: None,
    }
}

fn json_array<'a>(value: &'a Value, pointer: &str) -> Vec<&'a Value> {
    match value.pointer(pointer) {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(Value::Object(_)) => value.pointer(pointer).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn normalized_pair(artist: &str, title: &str) -> String {
    format!("{}\0{}", normalized(artist), normalized(title))
}

fn nonempty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn largest_image(album: &Value) -> Option<String> {
    album
        .get("image")
        .and_then(Value::as_array)
        .and_then(|images| {
            images
                .iter()
                .rev()
                .find_map(|image| nonempty_string(image.get("#text")))
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use crate::model::{
        ChannelConfig, ChannelPackItem, DownloadEligibility, DownloadEligibilityReason,
        DownloadProfile, LeechStatus, PackItemPlanState, PlannedDownload, RecommendationMatchState,
        RecommendationSource, ReleasePreferences, RuntimePreferences, TorrentVariant,
    };

    use super::{
        apple_sources_from_cache, apply_pack_constraints, base_edition_title, channel_is_due,
        compare_variants, get_json_with_retry, next_occurrence, next_refresh_at, parse_apple_chart,
        validate_channel, validate_channel_refresh,
    };

    #[test]
    fn weekly_schedule_is_dst_aware() {
        let channel = ChannelConfig::country_chart_default(Utc::now());
        let before = Utc.with_ymd_and_hms(2026, 10, 3, 20, 0, 0).unwrap();
        let next = next_occurrence(&channel, before).expect("next occurrence");
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 10, 4, 19, 0, 0).unwrap());
    }

    #[test]
    fn failed_channels_retry_with_exponential_backoff() {
        let mut channel = ChannelConfig::country_chart_default(Utc::now());
        channel.enabled = true;
        let attempted = Utc.with_ymd_and_hms(2026, 7, 30, 0, 0, 0).unwrap();
        channel.last_attempt_at = Some(attempted);
        channel.last_error = Some("temporary source failure".into());
        channel.failure_count = 2;
        assert_eq!(
            next_refresh_at(&channel, attempted).expect("retry"),
            attempted + chrono::Duration::minutes(30)
        );
        assert!(
            !channel_is_due(&channel, attempted + chrono::Duration::minutes(29)).expect("not due")
        );
        assert!(channel_is_due(&channel, attempted + chrono::Duration::minutes(30)).expect("due"));
    }

    #[test]
    fn validates_lastfm_credentials_before_enabling() {
        let mut channel = ChannelConfig::lastfm_default(Utc::now());
        channel.enabled = true;
        channel.lastfm.as_mut().expect("settings").username = "listener".into();
        let error = validate_channel(&channel, false).expect_err("missing credential");
        assert!(error.to_string().contains("API key"));
        validate_channel(&channel, true).expect("valid channel");
    }

    #[test]
    fn requires_lastfm_username_for_manual_refresh() {
        let channel = ChannelConfig::lastfm_default(Utc::now());
        validate_channel(&channel, true).expect("disabled channel may be saved before setup");
        let error =
            validate_channel_refresh(&channel, true).expect_err("refresh requires username");
        assert!(error.to_string().contains("username"));
    }

    #[tokio::test]
    async fn recommendation_http_errors_do_not_expose_query_secrets() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/source"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let error = get_json_with_retry(
            client
                .get(format!("{}/source", server.uri()))
                .query(&[("api_key", "do-not-log-this")]),
        )
        .await
        .expect_err("source should fail");
        assert!(error.to_string().contains("HTTP 400"));
        assert!(!error.to_string().contains("do-not-log-this"));
    }

    #[test]
    fn parses_ranked_apple_album_feed() {
        let items = parse_apple_chart(&json!({
            "feed": {
                "results": [{
                    "artistName": "Artist",
                    "id": "42",
                    "name": "Album",
                    "releaseDate": "2026-07-01",
                    "artworkUrl100": "https://example/100x100bb.jpg",
                    "url": "https://music.apple.com/au/album/42"
                }]
            }
        }))
        .expect("parse chart");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].rank, 1);
        assert_eq!(items[0].year, Some(2026));
        assert_eq!(
            items[0].artwork.as_deref(),
            Some("https://example/600x600bb.jpg")
        );
    }

    #[test]
    fn strips_only_terminal_enhanced_edition_qualifiers() {
        assert_eq!(
            base_edition_title("Short n' Sweet (Deluxe)").as_deref(),
            Some("Short n' Sweet")
        );
        assert_eq!(
            base_edition_title("Album - Expanded Edition").as_deref(),
            Some("Album")
        );
        assert_eq!(base_edition_title("Deluxe Music"), None);
    }

    #[test]
    fn maps_exact_apple_song_collections_to_album_then_ep_candidates() {
        let source = RecommendationSource {
            id: "lastfm:static".into(),
            rank: 1,
            artist: "Sleep Theory".into(),
            title: "Static".into(),
            year: None,
            artwork: None,
            url: Some("https://last.fm/static".into()),
            mbid: Some("single-id".into()),
            score: Some(1.0),
            catalog_country: Some("AU".into()),
            substituted_from: None,
        };
        let candidates = apple_sources_from_cache(
            &source,
            vec![
                json!({
                    "wrapperType": "collection",
                    "collectionType": "Album",
                    "artistName": "Sleep Theory",
                    "collectionName": "Paper Hearts - EP",
                    "collectionId": 2,
                    "trackCount": 6,
                    "releaseDate": "2023-09-29T00:00:00Z"
                }),
                json!({
                    "wrapperType": "collection",
                    "collectionType": "Album",
                    "artistName": "Sleep Theory",
                    "collectionName": "Afterglow",
                    "collectionId": 1,
                    "trackCount": 12,
                    "releaseDate": "2025-05-16T00:00:00Z"
                }),
                json!({
                    "wrapperType": "collection",
                    "collectionType": "Album",
                    "artistName": "Sleep Theory",
                    "collectionName": "Static - Single",
                    "collectionId": 3,
                    "trackCount": 1,
                    "releaseDate": "2025-02-05T00:00:00Z"
                }),
            ],
        )
        .expect("Apple candidates");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].title, "Afterglow");
        assert_eq!(candidates[1].title, "Paper Hearts - EP");
        assert_eq!(
            candidates[0]
                .substituted_from
                .as_ref()
                .map(|value| value.title.as_str()),
            Some("Static")
        );
    }

    #[test]
    fn variant_order_matches_normal_tracker_quality_media_policy() {
        fn variant(tracker: &str, torrent_id: i64, encoding: &str, media: &str) -> TorrentVariant {
            TorrentVariant {
                tracker: tracker.into(),
                torrent_id,
                group_id: 1,
                info_hash: None,
                format: Some("FLAC".into()),
                encoding: Some(encoding.into()),
                media: Some(media.into()),
                size: Some(1),
                seeders: Some(10),
                leechers: None,
                snatched: None,
                freeleech: true,
                leech_status: LeechStatus::Freeleech,
                can_use_token: false,
                token_eligibility_known: true,
                eligibility: Some(DownloadEligibility {
                    eligible: true,
                    reason: DownloadEligibilityReason::Eligible,
                    requires_token: false,
                    token_available: false,
                    token_cost: Some(0),
                }),
                remaster_title: None,
                downloads: Vec::new(),
                library: None,
            }
        }
        let preferences = ReleasePreferences::default();
        let ops = variant("ops", 2, "Lossless", "CD");
        let red = variant("red", 1, "24bit Lossless", "WEB");
        assert!(compare_variants(&ops, &red, &preferences, "Album").is_gt());
    }

    #[test]
    fn pack_constraints_enforce_token_capacity_and_duplicate_limits() {
        fn item(rank: u32, torrent_id: i64, size: i64, token_cost: u32) -> ChannelPackItem {
            let use_token = token_cost > 0;
            let eligibility = DownloadEligibility {
                eligible: true,
                reason: DownloadEligibilityReason::Eligible,
                requires_token: use_token,
                token_available: use_token,
                token_cost: Some(token_cost),
            };
            let variant = TorrentVariant {
                tracker: "ops".into(),
                torrent_id,
                group_id: rank as i64,
                info_hash: None,
                format: Some("FLAC".into()),
                encoding: Some("Lossless".into()),
                media: Some("WEB".into()),
                size: Some(size),
                seeders: Some(10),
                leechers: None,
                snatched: None,
                freeleech: !use_token,
                leech_status: if use_token {
                    LeechStatus::Regular
                } else {
                    LeechStatus::Freeleech
                },
                can_use_token: use_token,
                token_eligibility_known: true,
                eligibility: Some(eligibility),
                remaster_title: None,
                downloads: Vec::new(),
                library: None,
            };
            ChannelPackItem {
                ordinal: rank,
                source: RecommendationSource {
                    id: format!("source:{rank}"),
                    rank,
                    artist: "Artist".into(),
                    title: format!("Album {rank}"),
                    year: None,
                    artwork: None,
                    url: None,
                    mbid: None,
                    score: None,
                    catalog_country: None,
                    substituted_from: None,
                },
                match_state: RecommendationMatchState::Matched,
                release: None,
                variants: vec![variant],
                plan_state: PackItemPlanState::Executable,
                plan: Some(PlannedDownload {
                    tracker: "ops".into(),
                    torrent_id,
                    profile: "ops".into(),
                    use_token,
                    token_cost,
                    size: Some(size),
                    format: Some("FLAC".into()),
                    encoding: Some("Lossless".into()),
                    media: Some("WEB".into()),
                }),
                reason: None,
                job_id: None,
                job: None,
            }
        }

        let profiles = HashMap::from([(
            "ops".into(),
            DownloadProfile {
                name: "ops".into(),
                client: "music".into(),
                save_path: "/music".into(),
                tag: "ops".into(),
                start_paused: false,
            },
        )]);
        let mut preferences = RuntimePreferences::default();
        preferences
            .release
            .tracker_policies
            .iter_mut()
            .find(|policy| policy.tracker == "ops")
            .expect("OPS policy")
            .auto_token_limit = 3;
        let mut capacities = HashMap::from([("music".into(), Some(150))]);
        let mut fallback = item(2, 2, 10, 2);
        let mut cheaper = fallback.variants[0].clone();
        cheaper.torrent_id = 22;
        cheaper
            .eligibility
            .as_mut()
            .expect("eligibility")
            .token_cost = Some(1);
        fallback.variants.push(cheaper);
        let mut items = vec![
            item(1, 1, 60, 2),
            fallback,
            item(5, 5, 10, 1),
            item(3, 3, 100, 0),
            item(4, 1, 20, 0),
        ];

        apply_pack_constraints(&profiles, &mut items, &preferences, &mut capacities);

        assert_eq!(items[0].plan_state, PackItemPlanState::Executable);
        assert_eq!(
            items[1].plan.as_ref().expect("fallback plan").torrent_id,
            22
        );
        assert_eq!(items[1].plan.as_ref().expect("fallback plan").token_cost, 1);
        assert_eq!(items[2].plan_state, PackItemPlanState::TokenBudgetExceeded);
        assert_eq!(items[3].plan_state, PackItemPlanState::CapacityBlocked);
        assert_eq!(items[4].plan_state, PackItemPlanState::Duplicate);
    }
}
