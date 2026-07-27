use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use gazelle_api::RateLimiter;
use reqwest::{Client, StatusCode};
use serde_json::Value;
use url::Url;

use crate::{
    config::{TrackerConfig, read_secret},
    model::{
        Account, SearchGroup, SearchPage, SearchTorrent, TorrentMetadata, sanitized, value_bool,
        value_f64, value_i64, value_string,
    },
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
    async fn group(&self, id: i64) -> Result<Value>;
    async fn torrent(&self, id: i64) -> Result<(TorrentMetadata, Value)>;
    async fn download_torrent(&self, id: i64, use_token: bool) -> Result<Vec<u8>>;
}

pub struct GazelleTrackerClient {
    name: String,
    base_url: Url,
    token: String,
    client: Client,
    limiter: Arc<RateLimiter>,
}

impl GazelleTrackerClient {
    pub fn new(name: String, config: &TrackerConfig) -> Result<Self> {
        tracing::debug!(tracker = %name, kind = ?config.kind, "configuring Gazelle tracker");
        let base_url = Url::parse(&config.base_url).context("parse tracker base URL")?;
        let client = Client::builder()
            .user_agent(concat!("wotbox/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            name,
            base_url,
            token: read_secret(&config.token_file)?,
            client,
            limiter: Arc::new(RateLimiter::new(5, Duration::from_secs(10))),
        })
    }

    async fn request(&self, action: &str, params: &[(&str, String)]) -> Result<Value> {
        self.limiter.execute().await;
        let endpoint = self.base_url.join("ajax.php")?;
        let response = self
            .client
            .get(endpoint)
            .header("Authorization", format!("token {}", self.token))
            .query(&[("action", action)])
            .query(params)
            .send()
            .await
            .context("request tracker")?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            bail!("tracker_rate_limited");
        }
        let status = response.status();
        let body: Value = response.json().await.context("decode tracker response")?;
        if !status.is_success() {
            bail!("tracker returned HTTP {status}: {}", tracker_error(&body));
        }
        if body.get("status").and_then(Value::as_str) != Some("success") {
            bail!("tracker rejected request: {}", tracker_error(&body));
        }
        Ok(body)
    }
}

#[async_trait]
impl TrackerClient for GazelleTrackerClient {
    fn name(&self) -> &str {
        &self.name
    }

    async fn account(&self) -> Result<(Account, Value)> {
        let raw = self.request("index", &[]).await?;
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
        let raw = self.request("browse", &params).await?;
        let response = raw.get("response").cloned().unwrap_or(Value::Null);
        let groups = response
            .get("results")
            .and_then(Value::as_array)
            .map(|groups| groups.iter().filter_map(normalize_group).collect())
            .unwrap_or_default();
        let page = SearchPage {
            current_page: value_i64(&response, &["currentPage", "currentpage"]).unwrap_or(1),
            total_pages: value_i64(&response, &["pages"]).unwrap_or(1),
            total_results: value_i64(&response, &["resultsCount", "totalResults"]),
            groups,
        };
        Ok((page, sanitized(raw)))
    }

    async fn group(&self, id: i64) -> Result<Value> {
        let raw = self
            .request("torrentgroup", &[("id", id.to_string())])
            .await?;
        Ok(sanitized(
            raw.get("response").cloned().unwrap_or(Value::Null),
        ))
    }

    async fn torrent(&self, id: i64) -> Result<(TorrentMetadata, Value)> {
        let raw = self.request("torrent", &[("id", id.to_string())]).await?;
        let response = raw.get("response").cloned().unwrap_or(Value::Null);
        let torrent = response.get("torrent").unwrap_or(&response);
        let group = response.get("group").unwrap_or(&Value::Null);
        let info_hash = value_string(torrent, &["infoHash", "info_hash"])
            .ok_or_else(|| anyhow!("tracker response omitted torrent info hash"))?
            .to_ascii_lowercase();
        let metadata = TorrentMetadata {
            torrent_id: value_i64(torrent, &["id", "torrentId"]).unwrap_or(id),
            group_id: value_i64(group, &["id", "groupId"])
                .or_else(|| value_i64(torrent, &["groupId"])),
            name: value_string(group, &["name", "groupName"])
                .or_else(|| value_string(torrent, &["filePath", "name"]))
                .unwrap_or_else(|| format!("Torrent {id}")),
            info_hash,
            can_use_token: value_bool(torrent, &["canUseToken", "can_use_token"]),
            raw: sanitized(response.clone()),
        };
        Ok((metadata, sanitized(raw)))
    }

    async fn download_torrent(&self, id: i64, use_token: bool) -> Result<Vec<u8>> {
        self.limiter.execute().await;
        let endpoint = self.base_url.join("ajax.php")?;
        let response = self
            .client
            .get(endpoint)
            .header("Authorization", format!("token {}", self.token))
            .query(&[
                ("action", "download".to_owned()),
                ("id", id.to_string()),
                ("usetoken", if use_token { "1" } else { "0" }.to_owned()),
            ])
            .send()
            .await?;
        if !response.status().is_success() {
            bail!(
                "tracker torrent download returned HTTP {}",
                response.status()
            );
        }
        let bytes = response.bytes().await?;
        if bytes.is_empty() || bytes.first() != Some(&b'd') {
            bail!("tracker returned an invalid torrent payload");
        }
        Ok(bytes.to_vec())
    }
}

fn push<'a>(target: &mut Vec<(&'a str, String)>, key: &'a str, value: Option<&String>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        target.push((key, value.clone()));
    }
}

fn normalize_group(value: &Value) -> Option<SearchGroup> {
    let torrents = value
        .get("torrents")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(normalize_search_torrent).collect())
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
        group_id: value_i64(value, &["groupId", "groupid", "id"])?,
        name: value_string(value, &["groupName", "groupname", "name"])
            .unwrap_or_else(|| "Untitled release".into()),
        artist: value_string(value, &["artist", "artistName"]),
        year: value_i64(value, &["groupYear", "year"]),
        release_type: value_string(value, &["releaseType", "releaseTypeName"]),
        image: value_string(value, &["cover", "image"]),
        tags,
        torrents,
    })
}

fn normalize_search_torrent(value: &Value) -> Option<SearchTorrent> {
    Some(SearchTorrent {
        torrent_id: value_i64(value, &["torrentId", "id"])?,
        edition_id: value_i64(value, &["editionId"]),
        format: value_string(value, &["format"]),
        encoding: value_string(value, &["encoding"]),
        media: value_string(value, &["media"]),
        size: value_i64(value, &["size"]),
        seeders: value_i64(value, &["seeders"]),
        leechers: value_i64(value, &["leechers"]),
        snatched: value_i64(value, &["snatched"]),
        freeleech: value_bool(value, &["isFreeleech", "freeTorrent", "freeleech"]),
        can_use_token: value_bool(value, &["canUseToken", "can_use_token"]),
        remaster_title: value_string(value, &["remasterTitle"]),
    })
}

fn tracker_error(body: &Value) -> String {
    value_string(body, &["error", "message"])
        .or_else(|| {
            body.get("response")
                .and_then(|value| value_string(value, &["error", "message"]))
        })
        .unwrap_or_else(|| "unknown tracker error".into())
}

pub fn search_cache_key(request: &SearchRequest) -> String {
    let mut values = HashMap::new();
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
    use super::*;

    #[test]
    fn normalizes_browse_groups() {
        let group = normalize_group(&serde_json::json!({
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
        }))
        .unwrap();
        assert_eq!(group.group_id, 42);
        assert_eq!(group.torrents[0].torrent_id, 99);
        assert!(group.torrents[0].can_use_token);
    }
}
