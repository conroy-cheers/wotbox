use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde_json::Value;
use sha1::{Digest, Sha1};
use url::Url;

use crate::{
    config::{TrackerConfig, TrackerKind, read_secret},
    model::{
        Account, ArtistCatalogArtist, ArtistCatalogPage, ArtistCatalogRelease, ArtistCatalogRole,
        ArtistCredit, ArtistCreditSource, ArtistRole, CanonicalTorrent, LeechStatus, ReleaseDetail,
        ReleaseSource, ReleaseSummary, SearchGroup, SearchPage, SearchTorrent, TorrentMetadata,
        TorrentVariant, sanitized, value_bool, value_f64, value_i64, value_string,
    },
    provider::{ProviderFailure, ProviderFailureKind, ProviderGovernor, RequestClass, retry_after},
};

#[derive(Debug, Clone, Default)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub artist: Option<String>,
    pub release_type: Option<String>,
    pub year: Option<i64>,
    pub format: Option<String>,
    pub encoding: Option<String>,
    pub media: Option<String>,
    pub page: Option<i64>,
}

#[async_trait]
pub trait TrackerClient: Send + Sync {
    fn name(&self) -> &str;
    async fn account(&self) -> Result<(Account, Value)>;
    async fn search(&self, request: &SearchRequest) -> Result<(SearchPage, Value)>;
    async fn search_with_class(
        &self,
        request: &SearchRequest,
        _class: RequestClass,
    ) -> Result<(SearchPage, Value)> {
        self.search(request).await
    }
    async fn artist_catalog(&self, id: i64) -> Result<(ArtistCatalogPage, Value)>;
    async fn artist_catalog_with_class(
        &self,
        id: i64,
        _class: RequestClass,
    ) -> Result<(ArtistCatalogPage, Value)> {
        self.artist_catalog(id).await
    }
    async fn group(&self, id: i64) -> Result<(ReleaseDetail, Value)>;
    async fn group_with_class(
        &self,
        id: i64,
        _class: RequestClass,
    ) -> Result<(ReleaseDetail, Value)> {
        self.group(id).await
    }
    async fn torrent(&self, id: i64) -> Result<(TorrentMetadata, CanonicalTorrent, Value)>;
    async fn torrent_with_class(
        &self,
        id: i64,
        _class: RequestClass,
    ) -> Result<(TorrentMetadata, CanonicalTorrent, Value)> {
        self.torrent(id).await
    }
    async fn torrent_by_hash(&self, info_hash: &str) -> Result<(CanonicalTorrent, Value)>;
    async fn torrent_by_hash_with_class(
        &self,
        info_hash: &str,
        _class: RequestClass,
    ) -> Result<(CanonicalTorrent, Value)> {
        self.torrent_by_hash(info_hash).await
    }
    async fn download_torrent(&self, id: i64, use_token: bool) -> Result<Vec<u8>>;
    async fn download_torrent_with_class(
        &self,
        id: i64,
        use_token: bool,
        _class: RequestClass,
    ) -> Result<Vec<u8>> {
        self.download_torrent(id, use_token).await
    }
}

pub struct GazelleTrackerClient {
    name: String,
    base_url: Url,
    token: String,
    client: Client,
    governor: Option<ProviderGovernor>,
    provider_id: String,
    kind: TrackerKind,
}

impl GazelleTrackerClient {
    pub fn new(name: String, config: &TrackerConfig) -> Result<Self> {
        tracing::debug!(tracker = %name, kind = ?config.kind, "configuring Gazelle tracker");
        let base_url = Url::parse(&config.base_url).context("parse tracker base URL")?;
        let client = Client::builder()
            .user_agent(concat!("wotbox/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        let provider_id = format!("tracker:{name}");
        Ok(Self {
            name,
            base_url,
            token: read_secret(&config.token_file)?,
            client,
            governor: None,
            provider_id,
            kind: config.kind,
        })
    }

    pub fn governed(
        name: String,
        config: &TrackerConfig,
        governor: ProviderGovernor,
    ) -> Result<Self> {
        let mut client = Self::new(name, config)?;
        client.governor = Some(governor);
        Ok(client)
    }

    async fn request(
        &self,
        action: &str,
        params: &[(&str, String)],
        class: RequestClass,
    ) -> Result<Value> {
        let operation = || async {
            let endpoint = self
                .base_url
                .join("ajax.php")
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Permanent, error))?;
            let response = self
                .client
                .get(endpoint)
                .header("Authorization", &self.token)
                .query(&[("action", action)])
                .query(params)
                .send()
                .await
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Transient, error))?;
            let status = response.status();
            let retry = retry_after(&response);
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("unknown")
                .to_owned();
            let bytes = response
                .bytes()
                .await
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Transient, error))?;
            let parsed = serde_json::from_slice::<Value>(&bytes);
            let message = parsed
                .as_ref()
                .map(tracker_error)
                .unwrap_or_else(|_| tracker_response_summary(status, &bytes));
            if status == StatusCode::TOO_MANY_REQUESTS {
                return Err(
                    ProviderFailure::new(ProviderFailureKind::RateLimited, message)
                        .retry_after(retry),
                );
            }
            if !status.is_success() {
                let mut failure = ProviderFailure::from_message(format!(
                    "tracker returned HTTP {status}: {message}"
                ));
                if failure.kind == ProviderFailureKind::Permanent {
                    failure.kind =
                        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                            ProviderFailureKind::Authentication
                        } else if status.is_server_error() && !tracker_semantic_failure(&message) {
                            ProviderFailureKind::Transient
                        } else {
                            ProviderFailureKind::Permanent
                        };
                }
                return Err(failure);
            }
            let body = parsed.map_err(|error| {
                ProviderFailure::new(
                    ProviderFailureKind::Transient,
                    format!(
                        "tracker returned HTTP {status} with invalid JSON ({content_type}): {message}: {error}"
                    ),
                )
            })?;
            if body.get("status").and_then(Value::as_str) != Some("success") {
                return Err(ProviderFailure::from_message(format!(
                    "tracker rejected request: {message}"
                )));
            }
            Ok(body)
        };
        match &self.governor {
            Some(governor) => governor
                .execute(&self.provider_id, class, operation)
                .await
                .map_err(Into::into),
            None => operation()
                .await
                .map_err(|failure| anyhow!(failure.message)),
        }
    }
}

#[async_trait]
impl TrackerClient for GazelleTrackerClient {
    fn name(&self) -> &str {
        &self.name
    }

    async fn account(&self) -> Result<(Account, Value)> {
        let raw = self
            .request("index", &[], RequestClass::Interactive)
            .await?;
        let response = raw.get("response").cloned().unwrap_or(Value::Null);
        let stats = response.get("userstats").unwrap_or(&Value::Null);
        let account = Account {
            id: value_i64(&response, &["id", "userId"]),
            username: value_string(&response, &["username"]).unwrap_or_else(|| "Unknown".into()),
            uploaded: value_i64(stats, &["uploaded"]),
            downloaded: value_i64(stats, &["downloaded"]),
            ratio: value_f64(stats, &["ratio"]),
            required_ratio: value_f64(stats, &["requiredratio", "requiredRatio"]),
            user_class: value_string(stats, &["class", "userClass"]),
            bonus_points: value_f64(stats, &["bonusPoints", "bonuspoints"]),
            raw: sanitized(response),
        };
        Ok((account, sanitized(raw)))
    }

    async fn search(&self, request: &SearchRequest) -> Result<(SearchPage, Value)> {
        self.search_with_class(request, RequestClass::Interactive)
            .await
    }

    async fn search_with_class(
        &self,
        request: &SearchRequest,
        class: RequestClass,
    ) -> Result<(SearchPage, Value)> {
        let mut params = Vec::new();
        push(&mut params, "searchstr", request.query.as_ref());
        push(&mut params, "artistname", request.artist.as_ref());
        push(&mut params, "releasetype", request.release_type.as_ref());
        push(&mut params, "format", request.format.as_ref());
        push(&mut params, "encoding", request.encoding.as_ref());
        push(&mut params, "media", request.media.as_ref());
        if let Some(year) = request.year {
            params.push(("year", year.to_string()));
        }
        if let Some(page) = request.page {
            params.push(("page", page.to_string()));
        }
        let raw = self.request("browse", &params, class).await?;
        let response = raw.get("response").cloned().unwrap_or(Value::Null);
        let groups = response
            .get("results")
            .and_then(Value::as_array)
            .map(|groups| {
                groups
                    .iter()
                    .filter_map(|group| normalize_group(&self.name, self.kind, group))
                    .collect()
            })
            .unwrap_or_default();
        let page = SearchPage {
            current_page: value_i64(&response, &["currentPage", "currentpage"]).unwrap_or(1),
            total_pages: value_i64(&response, &["pages"]).unwrap_or(1),
            total_results: value_i64(&response, &["resultsCount", "totalResults"]),
            groups,
            deduplication: Default::default(),
            source_status: vec![crate::model::SourceLoadStatus {
                tracker: self.name.clone(),
                state: "ready".into(),
                error: None,
            }],
        };
        Ok((page, sanitized(raw)))
    }

    async fn artist_catalog(&self, id: i64) -> Result<(ArtistCatalogPage, Value)> {
        self.artist_catalog_with_class(id, RequestClass::Interactive)
            .await
    }

    async fn artist_catalog_with_class(
        &self,
        id: i64,
        class: RequestClass,
    ) -> Result<(ArtistCatalogPage, Value)> {
        let raw = self
            .request("artist", &[("id", id.to_string())], class)
            .await?;
        let response = raw.get("response").cloned().unwrap_or(Value::Null);
        Ok((
            normalize_artist_catalog(&self.name, self.kind, id, &response)?,
            sanitized(raw),
        ))
    }

    async fn group(&self, id: i64) -> Result<(ReleaseDetail, Value)> {
        self.group_with_class(id, RequestClass::Interactive).await
    }

    async fn group_with_class(
        &self,
        id: i64,
        class: RequestClass,
    ) -> Result<(ReleaseDetail, Value)> {
        let raw = self
            .request("torrentgroup", &[("id", id.to_string())], class)
            .await?;
        let response = raw.get("response").cloned().unwrap_or(Value::Null);
        Ok((
            normalize_release_detail(&self.name, self.kind, id, &response)?,
            sanitized(raw),
        ))
    }

    async fn torrent(&self, id: i64) -> Result<(TorrentMetadata, CanonicalTorrent, Value)> {
        self.torrent_with_class(id, RequestClass::Interactive).await
    }

    async fn torrent_with_class(
        &self,
        id: i64,
        class: RequestClass,
    ) -> Result<(TorrentMetadata, CanonicalTorrent, Value)> {
        let raw = self
            .request("torrent", &[("id", id.to_string())], class)
            .await?;
        let response = raw.get("response").cloned().unwrap_or(Value::Null);
        let torrent = response.get("torrent").unwrap_or(&response);
        let group = response.get("group").unwrap_or(&Value::Null);
        let info_hash =
            value_string(torrent, &["infoHash", "info_hash"]).map(|hash| hash.to_ascii_lowercase());
        let metadata = TorrentMetadata {
            torrent_id: value_i64(torrent, &["id", "torrentId"]).unwrap_or(id),
            group_id: value_i64(group, &["id", "groupId"])
                .or_else(|| value_i64(torrent, &["groupId"])),
            name: value_string(group, &["name", "groupName"])
                .or_else(|| value_string(torrent, &["filePath", "name"]))
                .unwrap_or_else(|| format!("Torrent {id}")),
            info_hash,
            can_use_token: value_bool(torrent, &["canUseToken", "can_use_token"]),
            token_eligibility_known: torrent.get("canUseToken").is_some()
                || torrent.get("can_use_token").is_some(),
            raw: sanitized(response.clone()),
        };
        let canonical =
            normalize_canonical_torrent(&self.name, self.kind, Some(id), None, &response)?;
        Ok((metadata, canonical, sanitized(raw)))
    }

    async fn torrent_by_hash(&self, info_hash: &str) -> Result<(CanonicalTorrent, Value)> {
        self.torrent_by_hash_with_class(info_hash, RequestClass::Interactive)
            .await
    }

    async fn torrent_by_hash_with_class(
        &self,
        info_hash: &str,
        class: RequestClass,
    ) -> Result<(CanonicalTorrent, Value)> {
        let requested_hash = info_hash.to_ascii_uppercase();
        let raw = self
            .request("torrent", &[("hash", requested_hash.clone())], class)
            .await?;
        let response = raw.get("response").cloned().unwrap_or(Value::Null);
        let canonical = normalize_canonical_torrent(
            &self.name,
            self.kind,
            None,
            Some(&requested_hash),
            &response,
        )?;
        Ok((canonical, sanitized(raw)))
    }

    async fn download_torrent(&self, id: i64, use_token: bool) -> Result<Vec<u8>> {
        self.download_torrent_with_class(id, use_token, RequestClass::Download)
            .await
    }

    async fn download_torrent_with_class(
        &self,
        id: i64,
        use_token: bool,
        class: RequestClass,
    ) -> Result<Vec<u8>> {
        let operation = || async {
            let endpoint = self
                .base_url
                .join("ajax.php")
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Permanent, error))?;
            let mut query = vec![("action", "download".to_owned()), ("id", id.to_string())];
            if use_token {
                query.push(("usetoken", "1".to_owned()));
            }
            let response = self
                .client
                .get(endpoint)
                .header("Authorization", &self.token)
                .query(&query)
                .send()
                .await
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Transient, error))?;
            let status = response.status();
            let retry = retry_after(&response);
            let bytes = response
                .bytes()
                .await
                .map_err(|error| ProviderFailure::new(ProviderFailureKind::Transient, error))?;
            if !status.is_success() {
                let message = serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .map(|body| tracker_error(&body))
                    .unwrap_or_else(|| tracker_response_summary(status, &bytes));
                let kind = if status == StatusCode::TOO_MANY_REQUESTS {
                    ProviderFailureKind::RateLimited
                } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                    ProviderFailureKind::Authentication
                } else if status.is_server_error() {
                    ProviderFailureKind::Transient
                } else {
                    ProviderFailure::from_message(&message).kind
                };
                return Err(ProviderFailure::new(kind, message).retry_after(retry));
            }
            if bytes.is_empty() || bytes.first() != Some(&b'd') {
                let message = if let Ok(body) = serde_json::from_slice::<Value>(&bytes) {
                    format!(
                        "tracker rejected torrent download: {}",
                        tracker_error(&body)
                    )
                } else {
                    format!(
                        "tracker returned an invalid torrent payload: {}",
                        tracker_response_summary(status, &bytes)
                    )
                };
                return Err(ProviderFailure::from_message(message));
            }
            Ok(bytes.to_vec())
        };
        match &self.governor {
            Some(governor) => governor
                .execute(&self.provider_id, class, operation)
                .await
                .map_err(Into::into),
            None => operation()
                .await
                .map_err(|failure| anyhow!(failure.message)),
        }
    }
}

pub fn torrent_info_hash(payload: &[u8]) -> Result<String> {
    if payload.first() != Some(&b'd') {
        bail!("torrent payload is not a bencoded dictionary");
    }
    let mut offset = 1;
    let mut info = None;
    while payload.get(offset) != Some(&b'e') {
        let key = parse_bencoded_bytes(payload, &mut offset)?;
        let value_start = offset;
        skip_bencoded_value(payload, &mut offset, 0)?;
        if key == b"info" {
            if info.is_some() {
                bail!("torrent payload contains more than one info dictionary");
            }
            info = Some(&payload[value_start..offset]);
        }
    }
    offset += 1;
    if offset != payload.len() {
        bail!("torrent payload contains trailing data");
    }
    let info = info.ok_or_else(|| anyhow!("torrent payload omitted its info dictionary"))?;
    Ok(hex::encode(Sha1::digest(info)))
}

fn parse_bencoded_bytes<'a>(payload: &'a [u8], offset: &mut usize) -> Result<&'a [u8]> {
    let length_start = *offset;
    while payload.get(*offset).is_some_and(u8::is_ascii_digit) {
        *offset += 1;
    }
    if *offset == length_start || payload.get(*offset) != Some(&b':') {
        bail!("invalid bencoded byte string");
    }
    if *offset - length_start > 1 && payload[length_start] == b'0' {
        bail!("invalid bencoded byte string length");
    }
    let length = std::str::from_utf8(&payload[length_start..*offset])?
        .parse::<usize>()
        .context("invalid bencoded byte string length")?;
    *offset += 1;
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= payload.len())
        .ok_or_else(|| anyhow!("truncated bencoded byte string"))?;
    let value = &payload[*offset..end];
    *offset = end;
    Ok(value)
}

fn skip_bencoded_value(payload: &[u8], offset: &mut usize, depth: usize) -> Result<()> {
    if depth > 128 {
        bail!("bencoded value exceeds maximum nesting depth");
    }
    match payload.get(*offset).copied() {
        Some(b'0'..=b'9') => {
            parse_bencoded_bytes(payload, offset)?;
        }
        Some(b'i') => {
            *offset += 1;
            let start = *offset;
            if payload.get(*offset) == Some(&b'-') {
                *offset += 1;
            }
            let digits = *offset;
            while payload.get(*offset).is_some_and(u8::is_ascii_digit) {
                *offset += 1;
            }
            if *offset == digits || payload.get(*offset) != Some(&b'e') {
                bail!("invalid bencoded integer");
            }
            if payload[start] == b'-' && *offset == start + 1 {
                bail!("invalid bencoded integer");
            }
            *offset += 1;
        }
        Some(b'l' | b'd') => {
            let dictionary = payload[*offset] == b'd';
            *offset += 1;
            while payload.get(*offset) != Some(&b'e') {
                if dictionary {
                    parse_bencoded_bytes(payload, offset)?;
                }
                skip_bencoded_value(payload, offset, depth + 1)?;
            }
            *offset += 1;
        }
        _ => bail!("invalid or truncated bencoded value"),
    }
    Ok(())
}

fn push<'a>(target: &mut Vec<(&'a str, String)>, key: &'a str, value: Option<&String>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        target.push((key, value.clone()));
    }
}

fn normalize_group(tracker: &str, kind: TrackerKind, value: &Value) -> Option<SearchGroup> {
    let torrents = value
        .get("torrents")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| normalize_search_torrent(tracker, item))
                .collect()
        })
        .unwrap_or_default();
    let tags = value
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    tag.as_str()
                        .map(ToOwned::to_owned)
                        .or_else(|| value_string(tag, &["name"]))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(SearchGroup {
        id: None,
        tracker: tracker.to_owned(),
        group_id: value_i64(value, &["groupId", "groupid", "id"])?,
        name: value_string(value, &["groupName", "groupname", "name"])
            .unwrap_or_else(|| "Untitled release".into()),
        artist: value_string(value, &["artist", "artistName"]),
        year: value_i64(value, &["groupYear", "year"]),
        release_type: normalize_release_type(kind, value),
        image: value_string(value, &["cover", "image"]),
        tags,
        torrents,
        sources: vec![ReleaseSource {
            tracker: tracker.to_owned(),
            group_id: value_i64(value, &["groupId", "groupid", "id"])?,
            match_score: 1.0,
        }],
        album_coverage: None,
    })
}

fn normalize_search_torrent(tracker: &str, value: &Value) -> Option<SearchTorrent> {
    let leech_status = normalize_leech_status(value);
    Some(SearchTorrent {
        tracker: tracker.to_owned(),
        torrent_id: value_i64(value, &["torrentId", "id"])?,
        edition_id: value_i64(value, &["editionId"]),
        format: value_string(value, &["format"]),
        encoding: value_string(value, &["encoding"]),
        media: value_string(value, &["media"]),
        size: value_i64(value, &["size"]),
        seeders: value_i64(value, &["seeders"]),
        leechers: value_i64(value, &["leechers"]),
        snatched: value_i64(value, &["snatched", "snatches"]),
        freeleech: leech_status.has_no_download_debit(),
        leech_status,
        can_use_token: value_bool(value, &["canUseToken", "can_use_token"]),
        eligibility: None,
        remaster_title: value_string(value, &["remasterTitle"]),
        info_hash: value_string(value, &["infoHash", "info_hash"])
            .map(|hash| hash.to_ascii_lowercase()),
        downloads: Vec::new(),
    })
}

fn normalize_artist_catalog(
    tracker: &str,
    kind: TrackerKind,
    requested_id: i64,
    value: &Value,
) -> Result<ArtistCatalogPage> {
    let artist_id = value_i64(value, &["id"]).unwrap_or(requested_id);
    let artist = ArtistCatalogArtist {
        id: None,
        tracker: tracker.to_owned(),
        artist_id,
        name: value_string(value, &["name"]).unwrap_or_else(|| format!("Artist {artist_id}")),
        artwork: value_string(value, &["image"]),
    };
    let mut groups = value
        .get("torrentgroup")
        .or_else(|| value.get("torrentGroup"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|group| normalize_artist_group(tracker, kind, artist_id, group))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    groups.sort_by(|left, right| {
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
    let primary_count = groups
        .iter()
        .filter(|group| group.roles.contains(&ArtistCatalogRole::Primary))
        .count();
    let appearance_count = groups.len().saturating_sub(primary_count);
    Ok(ArtistCatalogPage {
        artist,
        groups,
        primary_count,
        appearance_count,
        deduplication: Default::default(),
    })
}

fn normalize_artist_group(
    tracker: &str,
    kind: TrackerKind,
    artist_id: i64,
    value: &Value,
) -> Option<ArtistCatalogRelease> {
    let group_id = value_i64(value, &["groupId", "groupid", "id"])?;
    let mut release = normalize_release_summary(tracker, kind, group_id, value);
    release.release_type = normalize_release_type(kind, value);
    let variants = value
        .get("torrent")
        .or_else(|| value.get("torrents"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| normalize_variant(tracker, group_id, item, None, None))
                .collect()
        })
        .unwrap_or_default();
    let mut roles = artist_catalog_roles(value, artist_id);
    if roles.is_empty()
        && value
            .get("artists")
            .and_then(Value::as_array)
            .is_some_and(|artists| {
                artists
                    .iter()
                    .any(|artist| value_i64(artist, &["id"]) == Some(artist_id))
            })
    {
        roles.push(ArtistCatalogRole::Primary);
    }
    Some(ArtistCatalogRelease {
        release,
        tags: normalize_tags(value),
        variants,
        roles,
        listed_on_tracker: true,
        library_availability: None,
        library_added_at: None,
    })
}

fn artist_catalog_roles(value: &Value, artist_id: i64) -> Vec<ArtistCatalogRole> {
    let Some(roles) = value.get("extendedArtists") else {
        return Vec::new();
    };
    [
        ("1", ArtistCatalogRole::Primary),
        ("2", ArtistCatalogRole::Guest),
        ("3", ArtistCatalogRole::Remixer),
        ("4", ArtistCatalogRole::Composer),
        ("5", ArtistCatalogRole::Conductor),
        ("6", ArtistCatalogRole::Dj),
        ("7", ArtistCatalogRole::Producer),
        ("8", ArtistCatalogRole::Arranger),
    ]
    .into_iter()
    .filter_map(|(key, role)| {
        roles
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|artists| {
                artists
                    .iter()
                    .any(|artist| value_i64(artist, &["id"]) == Some(artist_id))
            })
            .then_some(role)
    })
    .collect()
}

fn release_type_name(kind: TrackerKind, id: i64) -> Option<String> {
    Some(
        match id {
            1 => "Album",
            3 => "Soundtrack",
            5 => "EP",
            6 => "Anthology",
            7 => "Compilation",
            8 if matches!(kind, TrackerKind::Ops) => "Sampler",
            9 => "Single",
            10 if matches!(kind, TrackerKind::Ops) => "Demo",
            11 => "Live album",
            12 if matches!(kind, TrackerKind::Ops) => "Split",
            13 => "Remix",
            14 => "Bootleg",
            15 => "Interview",
            16 => "Mixtape",
            17 if matches!(kind, TrackerKind::Red) => "Demo",
            17 => "DJ Mix",
            18 => "Concert Recording",
            19 if matches!(kind, TrackerKind::Red) => "DJ Mix",
            21 => "Unknown",
            _ => return None,
        }
        .to_owned(),
    )
}

fn normalize_release_type(kind: TrackerKind, value: &Value) -> Option<String> {
    value_string(value, &["releaseTypeName", "releaseType"])
        .or_else(|| value_i64(value, &["releaseType"]).and_then(|id| release_type_name(kind, id)))
}

fn normalize_release_detail(
    tracker: &str,
    kind: TrackerKind,
    requested_id: i64,
    value: &Value,
) -> Result<ReleaseDetail> {
    let group = value.get("group").unwrap_or(value);
    let group_id = value_i64(group, &["id", "groupId"]).unwrap_or(requested_id);
    let release = normalize_release_summary(tracker, kind, group_id, group);
    let torrents = value
        .get("torrents")
        .or_else(|| group.get("torrents"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| normalize_variant(tracker, group_id, item, None, None))
                .collect()
        })
        .unwrap_or_default();
    Ok(ReleaseDetail {
        release,
        field_provenance: serde_json::json!({}),
        tags: normalize_tags(group),
        description: value_string(group, &["wikiBody", "bbBody", "description"]),
        record_label: value_string(group, &["recordLabel", "label"]),
        variants: torrents,
    })
}

fn normalize_canonical_torrent(
    tracker: &str,
    kind: TrackerKind,
    requested_id: Option<i64>,
    requested_hash: Option<&str>,
    value: &Value,
) -> Result<CanonicalTorrent> {
    let group = value.get("group").unwrap_or(&Value::Null);
    let torrent = value.get("torrent").unwrap_or(value);
    let torrent_id = value_i64(torrent, &["id", "torrentId"])
        .or(requested_id)
        .ok_or_else(|| anyhow!("tracker response omitted torrent id"))?;
    let group_id = value_i64(group, &["id", "groupId"])
        .or_else(|| value_i64(torrent, &["groupId", "group_id"]))
        .ok_or_else(|| anyhow!("tracker response omitted group id"))?;
    let release = normalize_release_summary(tracker, kind, group_id, group);
    let variant = normalize_variant(tracker, group_id, torrent, Some(torrent_id), requested_hash)
        .ok_or_else(|| anyhow!("tracker response could not be normalized"))?;
    Ok(CanonicalTorrent {
        release,
        variant,
        tags: normalize_tags(group),
        description: value_string(group, &["wikiBody", "bbBody", "description"]),
        record_label: value_string(group, &["recordLabel", "label"]),
    })
}

fn normalize_release_summary(
    tracker: &str,
    kind: TrackerKind,
    group_id: i64,
    group: &Value,
) -> ReleaseSummary {
    let artists = normalize_artist_credits(tracker, group);
    let display_artist = value_string(group, &["artist", "artistName"]).or_else(|| {
        let primary = artists
            .iter()
            .filter(|artist| artist.role == ArtistRole::Primary)
            .map(|artist| artist.name.clone())
            .collect::<Vec<_>>();
        (!primary.is_empty()).then(|| primary.join(", "))
    });
    ReleaseSummary {
        id: None,
        tracker: tracker.to_owned(),
        group_id,
        title: value_string(group, &["name", "groupName", "groupname"])
            .unwrap_or_else(|| format!("Release {group_id}")),
        artist: display_artist,
        artists,
        year: value_i64(group, &["year", "groupYear"]),
        artwork: value_string(group, &["wikiImage", "cover", "image"]),
        release_type: normalize_release_type(kind, group),
        sources: vec![ReleaseSource {
            tracker: tracker.to_owned(),
            group_id,
            match_score: 1.0,
        }],
        album_coverage: None,
    }
}

fn normalize_artist_credits(tracker: &str, group: &Value) -> Vec<ArtistCredit> {
    let mut credits = Vec::new();
    let music_info = group.get("musicInfo");
    for (field, role) in [
        ("artists", ArtistRole::Primary),
        ("with", ArtistRole::Guest),
    ] {
        let Some(items) = music_info
            .and_then(|value| value.get(field))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for item in items {
            let Some(name) = value_string(item, &["name"]) else {
                continue;
            };
            let artist_id = value_i64(item, &["id"]).or_else(|| {
                item.get("artist")
                    .and_then(|artist| value_i64(artist, &["id"]))
            });
            let key = artist_id
                .map(|id| format!("id:{id}"))
                .unwrap_or_else(|| fallback_artist_key(&name));
            if !credits
                .iter()
                .any(|credit: &ArtistCredit| credit.key == key && credit.role == role)
            {
                credits.push(ArtistCredit {
                    canonical_id: None,
                    key,
                    tracker: tracker.to_owned(),
                    artist_id,
                    name,
                    role: role.clone(),
                    source: ArtistCreditSource::Structured,
                });
            }
        }
    }
    if credits.is_empty() {
        for (field, role) in [("1", ArtistRole::Primary), ("2", ArtistRole::Guest)] {
            let Some(items) = group
                .get("extendedArtists")
                .and_then(|value| value.get(field))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for item in items {
                let Some(name) = value_string(item, &["name"]) else {
                    continue;
                };
                let artist_id = value_i64(item, &["id"]);
                let key = artist_id
                    .map(|id| format!("id:{id}"))
                    .unwrap_or_else(|| fallback_artist_key(&name));
                if !credits
                    .iter()
                    .any(|credit: &ArtistCredit| credit.key == key && credit.role == role)
                {
                    credits.push(ArtistCredit {
                        canonical_id: None,
                        key,
                        tracker: tracker.to_owned(),
                        artist_id,
                        name,
                        role: role.clone(),
                        source: ArtistCreditSource::Structured,
                    });
                }
            }
        }
    }
    if credits.is_empty()
        && let Some(name) = value_string(group, &["artist", "artistName"])
    {
        credits.push(ArtistCredit {
            canonical_id: None,
            key: fallback_artist_key(&name),
            tracker: tracker.to_owned(),
            artist_id: None,
            name,
            role: ArtistRole::Primary,
            source: ArtistCreditSource::DisplayFallback,
        });
    }
    credits
}

pub fn fallback_artist_credit(tracker: &str, name: &str) -> ArtistCredit {
    ArtistCredit {
        canonical_id: None,
        key: fallback_artist_key(name),
        tracker: tracker.to_owned(),
        artist_id: None,
        name: name.to_owned(),
        role: ArtistRole::Primary,
        source: ArtistCreditSource::DisplayFallback,
    }
}

fn fallback_artist_key(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let normalized = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let digest = hex::encode(Sha256::digest(normalized.as_bytes()));
    format!("name:{}", &digest[..16])
}

fn normalize_tags(value: &Value) -> Vec<String> {
    value
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    tag.as_str()
                        .map(ToOwned::to_owned)
                        .or_else(|| value_string(tag, &["name"]))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_variant(
    tracker: &str,
    group_id: i64,
    value: &Value,
    requested_id: Option<i64>,
    requested_hash: Option<&str>,
) -> Option<TorrentVariant> {
    let leech_status = normalize_leech_status(value);
    Some(TorrentVariant {
        tracker: tracker.to_owned(),
        torrent_id: value_i64(value, &["id", "torrentId"]).or(requested_id)?,
        group_id,
        info_hash: value_string(value, &["infoHash", "info_hash"])
            .or_else(|| requested_hash.map(ToOwned::to_owned))
            .map(|hash| hash.to_ascii_lowercase()),
        format: value_string(value, &["format"]),
        encoding: value_string(value, &["encoding"]),
        media: value_string(value, &["media"]),
        size: value_i64(value, &["size"]),
        seeders: value_i64(value, &["seeders"]),
        leechers: value_i64(value, &["leechers"]),
        snatched: value_i64(value, &["snatched", "snatches"]),
        freeleech: leech_status.has_no_download_debit(),
        leech_status,
        can_use_token: value_bool(value, &["canUseToken", "can_use_token"]),
        token_eligibility_known: value.get("canUseToken").is_some()
            || value.get("can_use_token").is_some(),
        eligibility: None,
        remaster_title: value_string(value, &["remasterTitle"]),
        downloads: Vec::new(),
        library: None,
    })
}

fn normalize_leech_status(value: &Value) -> LeechStatus {
    if value_bool(value, &["isPersonalFreeleech", "personalFreeleech"]) {
        LeechStatus::PersonalFreeleech
    } else if value_bool(value, &["isNeutralLeech", "isNeutralleech", "neutralLeech"]) {
        LeechStatus::Neutral
    } else if value_bool(value, &["isFreeload", "freeload"]) {
        LeechStatus::Freeload
    } else if value_bool(value, &["isFreeleech", "freeTorrent", "freeleech"]) {
        LeechStatus::Freeleech
    } else {
        LeechStatus::Regular
    }
}

fn tracker_error(body: &Value) -> String {
    value_string(body, &["error", "message"])
        .or_else(|| {
            body.get("response")
                .and_then(|value| value_string(value, &["error", "message"]))
        })
        .unwrap_or_else(|| "unknown tracker error".into())
}

fn tracker_response_summary(status: StatusCode, bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut plain = String::with_capacity(text.len().min(512));
    let mut in_tag = false;
    let mut last_space = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            value if value.is_whitespace() => {
                if !last_space && !plain.is_empty() {
                    plain.push(' ');
                    last_space = true;
                }
            }
            value => {
                plain.push(value);
                last_space = false;
            }
        }
        if plain.chars().count() >= 240 {
            break;
        }
    }
    let plain = plain.trim();
    if plain.is_empty() {
        format!("tracker returned HTTP {status} with an empty non-JSON response")
    } else {
        format!("tracker returned HTTP {status}: {plain}")
    }
}

fn tracker_semantic_failure(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    ["bad parameters", "not found", "does not exist", "bad hash"]
        .iter()
        .any(|needle| message.contains(needle))
}

pub fn search_cache_key(request: &SearchRequest) -> String {
    let mut values = BTreeMap::new();
    values.insert("query", request.query.clone().unwrap_or_default());
    values.insert("artist", request.artist.clone().unwrap_or_default());
    values.insert(
        "release_type",
        request.release_type.clone().unwrap_or_default(),
    );
    values.insert(
        "year",
        request.year.map(|v| v.to_string()).unwrap_or_default(),
    );
    values.insert("format", request.format.clone().unwrap_or_default());
    values.insert("encoding", request.encoding.clone().unwrap_or_default());
    values.insert("media", request.media.clone().unwrap_or_default());
    values.insert("page", request.page.unwrap_or(1).to_string());
    let canonical = serde_json::to_vec(&values).unwrap_or_default();
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(canonical))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path, query_param, query_param_is_missing},
    };

    use crate::config::{TrackerConfig, TrackerKind};

    use super::*;

    #[test]
    fn search_cache_keys_are_deterministic() {
        let request = SearchRequest {
            query: Some("Rockabye".into()),
            artist: Some("Clean Bandit".into()),
            ..Default::default()
        };
        assert_eq!(search_cache_key(&request), search_cache_key(&request));
    }

    #[test]
    fn derives_v1_info_hash_from_exact_bencoded_info_dictionary() {
        let payload = b"d8:announce15:https://tracker4:infod4:name4:test6:lengthi42eee";
        assert_eq!(
            torrent_info_hash(payload).expect("valid torrent"),
            "07e73c3f168e838c9e99635915f82fabe76208d8"
        );
    }

    #[test]
    fn rejects_torrent_payloads_without_a_valid_info_dictionary() {
        assert!(torrent_info_hash(b"d8:announce15:https://trackere").is_err());
        assert!(torrent_info_hash(b"not a torrent").is_err());
        assert!(torrent_info_hash(b"d4:infod4:name4:testeejunk").is_err());
    }

    #[test]
    fn normalizes_browse_groups() {
        let group = normalize_group(
            "ops",
            TrackerKind::Ops,
            &serde_json::json!({
                "groupId": 42,
                "groupName": "Kind of Blue",
                "artist": "Miles Davis",
                "groupYear": 1959,
                "tags": ["jazz"],
                "torrents": [{
                    "torrentId": 99,
                    "format": "FLAC",
                    "encoding": "Lossless",
                    "size": 1000,
                    "seeders": 3,
                    "canUseToken": true
                }]
            }),
        )
        .unwrap();
        assert_eq!(group.group_id, 42);
        assert_eq!(group.torrents[0].torrent_id, 99);
        assert!(group.torrents[0].can_use_token);
    }

    #[test]
    fn uses_requested_hash_when_tracker_omits_it() {
        let canonical = normalize_canonical_torrent(
            "ops",
            TrackerKind::Ops,
            None,
            Some("ABCDEF0123456789ABCDEF0123456789ABCDEF01"),
            &serde_json::json!({
                "group": {
                    "id": 42,
                    "name": "Kind of Blue",
                    "artist": "Miles Davis"
                },
                "torrent": {
                    "id": 99,
                    "format": "FLAC",
                    "encoding": "Lossless"
                }
            }),
        )
        .expect("canonical torrent");
        assert_eq!(
            canonical.variant.info_hash.as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef01")
        );
        assert_eq!(canonical.release.title, "Kind of Blue");
    }

    #[test]
    fn normalizes_primary_and_guest_artist_credits_without_other_roles() {
        let detail = normalize_release_detail(
            "ops",
            TrackerKind::Ops,
            42,
            &serde_json::json!({
                "group": {
                    "id": 42,
                    "name": "Collaborative Record",
                    "musicInfo": {
                        "artists": [{
                            "name": "The Primary",
                            "artist": {"id": 10}
                        }],
                        "with": [{
                            "id": 11,
                            "name": "Guest Artist"
                        }],
                        "composers": [{
                            "id": 12,
                            "name": "Composer"
                        }],
                        "producer": [{
                            "id": 13,
                            "name": "Producer"
                        }]
                    }
                },
                "torrents": []
            }),
        )
        .expect("release detail");
        assert_eq!(detail.release.artists.len(), 2);
        assert_eq!(detail.release.artists[0].key, "id:10");
        assert_eq!(detail.release.artists[0].role, ArtistRole::Primary);
        assert_eq!(detail.release.artists[1].key, "id:11");
        assert_eq!(detail.release.artists[1].role, ArtistRole::Guest);
        assert!(
            detail
                .release
                .artists
                .iter()
                .all(|artist| artist.source == ArtistCreditSource::Structured)
        );
        assert_eq!(detail.release.artist.as_deref(), Some("The Primary"));
    }

    #[test]
    fn keeps_tracker_display_artist_as_an_unparsed_fallback() {
        let summary = normalize_release_summary(
            "ops",
            TrackerKind::Ops,
            42,
            &serde_json::json!({
                "name": "Compilation",
                "artist": "Artist One, Artist Two & Friends"
            }),
        );
        assert_eq!(summary.artists.len(), 1);
        assert_eq!(summary.artists[0].name, "Artist One, Artist Two & Friends");
        assert_eq!(
            summary.artists[0].source,
            ArtistCreditSource::DisplayFallback
        );
    }

    #[test]
    fn normalizes_artist_catalog_roles_and_variants() {
        let catalog = normalize_artist_catalog(
            "ops",
            TrackerKind::Ops,
            10,
            &serde_json::json!({
                "id": 10,
                "name": "The Artist",
                "image": "https://images.example/artist.jpg",
                "torrentgroup": [{
                    "groupId": 42,
                    "groupName": "Primary Record",
                    "groupYear": 2024,
                    "releaseType": 1,
                    "wikiImage": "https://images.example/cover.jpg",
                    "tags": ["ambient"],
                    "artists": [{"id": 10, "name": "The Artist"}],
                    "extendedArtists": {
                        "1": [{"id": 10, "name": "The Artist"}],
                        "2": null,
                        "3": null,
                        "4": null,
                        "5": null,
                        "6": null,
                        "7": null,
                        "8": null
                    },
                    "torrent": [{
                        "id": 99,
                        "format": "FLAC",
                        "encoding": "Lossless",
                        "media": "WEB",
                        "freeTorrent": false,
                        "seeders": 5
                    }]
                }, {
                    "groupId": 43,
                    "groupName": "Produced Record",
                    "groupYear": 2023,
                    "releaseType": 5,
                    "artists": [{"id": 20, "name": "Someone Else"}],
                    "extendedArtists": {
                        "1": [{"id": 20, "name": "Someone Else"}],
                        "2": null,
                        "3": null,
                        "4": null,
                        "5": null,
                        "6": null,
                        "7": [{"id": 10, "name": "The Artist"}],
                        "8": null
                    },
                    "torrent": [{"id": 100, "format": "MP3", "freeTorrent": true}]
                }]
            }),
        )
        .expect("artist catalog");
        assert_eq!(catalog.artist.artist_id, 10);
        assert_eq!(catalog.primary_count, 1);
        assert_eq!(catalog.appearance_count, 1);
        assert_eq!(
            catalog.groups[0].release.release_type.as_deref(),
            Some("Album")
        );
        assert_eq!(catalog.groups[0].roles, vec![ArtistCatalogRole::Primary]);
        assert_eq!(catalog.groups[0].variants[0].torrent_id, 99);
        assert!(!catalog.groups[0].variants[0].token_eligibility_known);
        assert_eq!(catalog.groups[1].roles, vec![ArtistCatalogRole::Producer]);
        assert!(catalog.groups[1].variants[0].freeleech);
    }

    #[tokio::test]
    async fn requests_artist_catalog_by_stable_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ajax.php"))
            .and(query_param("action", "artist"))
            .and(query_param("id", "10"))
            .and(header("authorization", "tracker-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "success",
                "response": {
                    "id": 10,
                    "name": "The Artist",
                    "torrentgroup": []
                }
            })))
            .mount(&server)
            .await;
        let directory = tempdir().expect("temporary directory");
        let token_path = directory.path().join("token");
        std::fs::write(&token_path, "tracker-token").expect("write token");
        let client = GazelleTrackerClient::new(
            "ops".into(),
            &TrackerConfig {
                kind: TrackerKind::Ops,
                base_url: server.uri(),
                token_file: token_path,
                announce_hosts: vec!["home.opsfet.ch".into()],
            },
        )
        .expect("tracker client");

        let (catalog, _) = client.artist_catalog(10).await.expect("artist lookup");
        assert_eq!(catalog.artist.name, "The Artist");
    }

    #[tokio::test]
    async fn requests_hash_in_uppercase_and_normalizes_missing_hash() {
        let server = MockServer::start().await;
        let lowercase = "abcdef0123456789abcdef0123456789abcdef01";
        Mock::given(method("GET"))
            .and(path("/ajax.php"))
            .and(query_param("action", "torrent"))
            .and(query_param("hash", lowercase.to_ascii_uppercase()))
            .and(header("authorization", "tracker-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "success",
                "response": {
                    "group": {"id": 42, "name": "Kind of Blue"},
                    "torrent": {"id": 99, "format": "FLAC"}
                }
            })))
            .mount(&server)
            .await;
        let directory = tempdir().expect("temporary directory");
        let token_path = directory.path().join("token");
        std::fs::write(&token_path, "tracker-token").expect("write token");
        let client = GazelleTrackerClient::new(
            "ops".into(),
            &TrackerConfig {
                kind: TrackerKind::Ops,
                base_url: server.uri(),
                token_file: token_path,
                announce_hosts: vec!["home.opsfet.ch".into()],
            },
        )
        .expect("tracker client");

        let (canonical, _) = client
            .torrent_by_hash(lowercase)
            .await
            .expect("hash lookup");
        assert_eq!(canonical.variant.info_hash.as_deref(), Some(lowercase));
    }

    #[tokio::test]
    async fn omits_token_parameter_for_normal_torrent_downloads() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ajax.php"))
            .and(query_param("action", "download"))
            .and(query_param("id", "99"))
            .and(query_param_is_missing("usetoken"))
            .and(header("authorization", "tracker-token"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"d4:infode".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_tracker_client(&server);

        let payload = client
            .download_torrent(99, false)
            .await
            .expect("torrent download");
        assert_eq!(payload, b"d4:infode");
    }

    #[tokio::test]
    async fn sends_token_parameter_only_when_requested() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ajax.php"))
            .and(query_param("action", "download"))
            .and(query_param("id", "99"))
            .and(query_param("usetoken", "1"))
            .and(header("authorization", "tracker-token"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"d4:infode".to_vec()))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_tracker_client(&server);

        client
            .download_torrent(99, true)
            .await
            .expect("token torrent download");
    }

    #[tokio::test]
    async fn surfaces_json_errors_from_torrent_downloads() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ajax.php"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "failure",
                "error": "already freeleech"
            })))
            .mount(&server)
            .await;
        let client = test_tracker_client(&server);

        let error = client
            .download_torrent(99, false)
            .await
            .expect_err("tracker failure");
        assert_eq!(
            error.to_string(),
            "tracker rejected torrent download: already freeleech"
        );
    }

    #[tokio::test]
    async fn surfaces_bounded_html_errors_before_attempting_json_decode() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ajax.php"))
            .and(query_param("action", "torrent"))
            .respond_with(
                ResponseTemplate::new(500)
                    .insert_header("content-type", "text/html")
                    .set_body_string(
                        "<html><head><title>Orpheus error</title></head><body>Bad parameters</body></html>",
                    ),
            )
            .mount(&server)
            .await;
        let client = test_tracker_client(&server);

        let error = client
            .torrent_by_hash("abcdef0123456789abcdef0123456789abcdef01")
            .await
            .expect_err("tracker failure");
        assert!(error.to_string().contains("Orpheus errorBad parameters"));
        assert!(!error.to_string().contains("expected value"));
    }

    fn test_tracker_client(server: &MockServer) -> GazelleTrackerClient {
        let directory = tempdir().expect("temporary directory");
        let token_path = directory.path().join("token");
        std::fs::write(&token_path, "tracker-token").expect("write token");
        GazelleTrackerClient::new(
            "ops".into(),
            &TrackerConfig {
                kind: TrackerKind::Ops,
                base_url: server.uri(),
                token_file: token_path,
                announce_hosts: vec!["home.opsfet.ch".into()],
            },
        )
        .expect("tracker client")
    }
}
