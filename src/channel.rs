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
        ChannelConfig, ChannelKind, ChannelPackItem, ChannelRunStatus, LastfmChannelSettings,
        PackItemPlanState, PlannedDownload, RecommendationMatchState, RecommendationSource,
        ReleasePreferences, RuntimePreferences, TorrentVariant,
    },
    release_matcher::{AUTO_MERGE_THRESHOLD, external_score, normalized},
    tracker::SearchRequest,
};

const APPLE_FEED_ROOT: &str = "https://rss.applemarketingtools.com/api/v2";
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
    let since = channel
        .last_attempt_at
        .or(channel.last_successful_at)
        .unwrap_or(now - ChronoDuration::days(8));
    Ok(next_occurrence(channel, since)? <= now)
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
            let (items, was_partial) = fetch_lastfm_recommendations(&state, settings).await?;
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
    for source in sources {
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
    Ok(replanned)
}

pub fn preference_fingerprint(
    state: &AppState,
    preferences: &RuntimePreferences,
) -> Result<String> {
    let mut profiles = state.profiles.values().cloned().collect::<Vec<_>>();
    profiles.sort_by(|left, right| left.name.cmp(&right.name));
    let value = serde_json::to_vec(&(preferences, profiles))?;
    Ok(hex::encode(Sha256::digest(value)))
}

async fn fetch_apple_chart(state: &AppState, country: &str) -> Result<Vec<RecommendationSource>> {
    let country = country.trim().to_ascii_lowercase();
    let url = format!("{APPLE_FEED_ROOT}/{country}/music/most-played/100/albums.json");
    let body: Value = state
        .source_client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
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
            })
        })
        .collect())
}

async fn fetch_lastfm_recommendations(
    state: &AppState,
    settings: &LastfmChannelSettings,
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
    let value: Value = state
        .source_client
        .get(LASTFM_ROOT)
        .query(&query)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if let Some(error) = value.get("error") {
        bail!(
            "Last.fm error {}: {}",
            error,
            value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("request failed")
        );
    }
    Ok(value)
}

async fn resolve_source(
    state: &Arc<AppState>,
    source: RecommendationSource,
    preferences: &RuntimePreferences,
) -> Result<ChannelPackItem> {
    let mut groups = Vec::new();
    let request = SearchRequest {
        query: Some(source.title.clone()),
        artist: Some(source.artist.clone()),
        release_type: None,
        year: source.year,
        format: None,
        encoding: None,
        media: None,
        page: Some(1),
    };
    let mut tracker_errors = Vec::new();
    for (name, tracker) in &state.trackers {
        match tracker.search(&request).await {
            Ok((mut page, _)) => {
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
        if !tracker_errors.is_empty() {
            bail!("tracker lookup incomplete: {}", tracker_errors.join("; "));
        }
        return Ok(unresolved_item(
            source,
            RecommendationMatchState::Unmatched,
            PackItemPlanState::Unmatched,
            "No matching Album or EP is currently available on a configured tracker",
        ));
    };
    if let Some((second_score, second)) = matches.get(1)
        && best.id != second.id
        && best_score - second_score < 0.03
    {
        return Ok(unresolved_item(
            source,
            RecommendationMatchState::Ambiguous,
            PackItemPlanState::Ambiguous,
            "Multiple tracker releases matched with similar confidence",
        ));
    }
    let release_id = best
        .id
        .context("matched tracker release has no canonical id")?;
    let detail = state
        .db
        .get_release_detail(release_id)
        .await?
        .context("canonical release detail is unavailable")?;
    let mut variants = detail.variants;
    for variant in &mut variants {
        variant.eligibility = Some(preferences.release.eligibility(
            &variant.tracker,
            variant.format.as_deref(),
            variant.encoding.as_deref(),
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
        plan_variant(state, &variants, &preferences.release)
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
    state: &AppState,
    variants: &[TorrentVariant],
    preferences: &ReleasePreferences,
) -> (PackItemPlanState, Option<PlannedDownload>, Option<String>) {
    let mut candidates = variants
        .iter()
        .filter_map(|variant| {
            let eligibility = variant.eligibility.as_ref()?;
            let policy = preferences.tracker_policy(&variant.tracker);
            if !eligibility.eligible || (eligibility.requires_token && !policy.auto_use_tokens) {
                return None;
            }
            Some((variant, eligibility.requires_token))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left, _), (right, _)| compare_variants(left, right, preferences));
    let Some((variant, use_token)) = candidates.first() else {
        return (
            PackItemPlanState::PolicyBlocked,
            None,
            Some("No torrent variant satisfies the configured tracker and quality rules".into()),
        );
    };
    let mut profiles = state.profiles.keys().cloned().collect::<Vec<_>>();
    profiles.sort();
    let profile = state
        .profiles
        .get(&variant.tracker)
        .map(|profile| profile.name.clone())
        .or_else(|| profiles.first().cloned());
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
            size: variant.size,
            format: variant.format.clone(),
            encoding: variant.encoding.clone(),
            media: variant.media.clone(),
        }),
        None,
    )
}

fn compare_variants(
    left: &TorrentVariant,
    right: &TorrentVariant,
    preferences: &ReleasePreferences,
) -> Ordering {
    tracker_rank(&left.tracker, preferences)
        .cmp(&tracker_rank(&right.tracker, preferences))
        .then_with(|| quality_rank(left, preferences).cmp(&quality_rank(right, preferences)))
        .then_with(|| {
            media_rank(left.media.as_deref(), preferences)
                .cmp(&media_rank(right.media.as_deref(), preferences))
        })
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
    preferences
        .quality_order
        .iter()
        .position(|value| {
            value
                == ReleasePreferences::quality_class(
                    variant.format.as_deref(),
                    variant.encoding.as_deref(),
                )
        })
        .unwrap_or(preferences.quality_order.len())
}

fn media_rank(media: Option<&str>, preferences: &ReleasePreferences) -> usize {
    let media = media.unwrap_or("other");
    preferences
        .media_tiers
        .iter()
        .position(|tier| tier.iter().any(|value| value.eq_ignore_ascii_case(media)))
        .or_else(|| {
            preferences
                .media_tiers
                .iter()
                .position(|tier| tier.iter().any(|value| value.eq_ignore_ascii_case("other")))
        })
        .unwrap_or(preferences.media_tiers.len())
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
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use crate::model::{
        ChannelConfig, DownloadEligibility, DownloadEligibilityReason, LeechStatus,
        ReleasePreferences, TorrentVariant,
    };

    use super::{compare_variants, next_occurrence, parse_apple_chart, validate_channel};

    #[test]
    fn weekly_schedule_is_dst_aware() {
        let channel = ChannelConfig::country_chart_default(Utc::now());
        let before = Utc.with_ymd_and_hms(2026, 10, 3, 20, 0, 0).unwrap();
        let next = next_occurrence(&channel, before).expect("next occurrence");
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 10, 4, 19, 0, 0).unwrap());
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
                }),
                remaster_title: None,
                downloads: Vec::new(),
                library: None,
            }
        }
        let preferences = ReleasePreferences::default();
        let ops = variant("ops", 2, "Lossless", "CD");
        let red = variant("red", 1, "24bit Lossless", "WEB");
        assert!(compare_variants(&ops, &red, &preferences).is_lt());
    }
}
