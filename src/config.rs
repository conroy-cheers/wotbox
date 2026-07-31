use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// TOML configuration file.
    #[arg(long, env = "WOTBOX_CONFIG")]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub listen_address: String,
    pub port: u16,
    pub base_path: String,
    pub database_path: PathBuf,
    pub trackers: BTreeMap<String, TrackerConfig>,
    pub download_clients: BTreeMap<String, DownloadClientConfig>,
    pub download_profiles: BTreeMap<String, DownloadProfileConfig>,
    pub lastfm_api_key_file: Option<PathBuf>,
    pub plex: Option<PlexConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackerConfig {
    pub kind: TrackerKind,
    pub base_url: String,
    pub token_file: PathBuf,
    #[serde(default, alias = "announceHosts")]
    pub announce_hosts: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind {
    Ops,
    Red,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadClientConfig {
    pub kind: DownloadClientKind,
    pub base_url: String,
    pub api_key_file: PathBuf,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadClientKind {
    Qbittorrent,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DownloadProfileConfig {
    pub client: String,
    pub save_path: String,
    pub tag: String,
    #[serde(default)]
    pub start_paused: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlexConfig {
    pub base_url: String,
    pub token_file: PathBuf,
    pub section_id: u32,
    pub library_roots: Vec<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1".into(),
            port: 8780,
            base_path: "/".into(),
            database_path: "wotbox.sqlite".into(),
            trackers: BTreeMap::new(),
            download_clients: BTreeMap::new(),
            download_profiles: BTreeMap::new(),
            lastfm_api_key_file: None,
            plex: None,
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        dotenvy::dotenv().ok();
        let mut config = if let Some(path) = path {
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("read configuration {}", path.display()))?;
            toml::from_str(&contents).context("parse configuration")?
        } else {
            Self::from_environment()?
        };

        config.base_path = normalize_base_path(&config.base_path)?;
        if config.trackers.is_empty() {
            bail!("at least one tracker must be configured");
        }
        if config.download_clients.is_empty() {
            bail!("at least one download client must be configured");
        }
        for (name, profile) in &config.download_profiles {
            if !config.download_clients.contains_key(&profile.client) {
                bail!(
                    "download profile {name} references unknown client {}",
                    profile.client
                );
            }
        }
        if let Some(plex) = &config.plex {
            let url = url::Url::parse(&plex.base_url).context("parse Plex base_url")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                bail!("Plex base_url must be an HTTP(S) origin without a query or fragment");
            }
            if plex.section_id == 0 {
                bail!("Plex section_id must be greater than zero");
            }
            if plex.library_roots.is_empty() {
                bail!("Plex library_roots must contain at least one path");
            }
            if plex.library_roots.iter().any(|root| !root.is_absolute()) {
                bail!("Plex library_roots must be absolute paths");
            }
            if plex.library_roots.iter().any(|root| root == Path::new("/")) {
                bail!("Plex library_roots must not scan the filesystem root");
            }
        }
        Ok(config)
    }

    fn from_environment() -> Result<Self> {
        let mut config = Self::default();
        if let Ok(value) = std::env::var("WOTBOX_LISTEN_ADDRESS") {
            config.listen_address = value;
        }
        if let Ok(value) = std::env::var("WOTBOX_PORT") {
            config.port = value.parse().context("parse WOTBOX_PORT")?;
        }
        if let Ok(value) = std::env::var("WOTBOX_BASE_PATH") {
            config.base_path = value;
        }
        if let Ok(value) = std::env::var("WOTBOX_DATABASE_PATH") {
            config.database_path = value.into();
        }
        let ops_token = std::env::var("OPS_TOKEN").context(
            "WOTBOX_CONFIG is unset and OPS_TOKEN is unavailable; provide a config file",
        )?;
        let qbit_key = std::env::var("QBITTORRENT_API_KEY")
            .context("WOTBOX_CONFIG is unset and QBITTORRENT_API_KEY is unavailable")?;
        let secret_dir = tempfile::Builder::new()
            .prefix("wotbox-secrets")
            .tempdir()?;
        let secret_dir = secret_dir.keep();
        let ops_path = secret_dir.join("ops-token");
        let red_path = secret_dir.join("red-token");
        let qbit_path = secret_dir.join("qbit-api-key");
        write_secret(&ops_path, &ops_token)?;
        let red_token = std::env::var("RED_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        if let Some(token) = &red_token {
            write_secret(&red_path, token)?;
        }
        write_secret(&qbit_path, &qbit_key)?;
        if let Ok(lastfm_key) = std::env::var("LASTFM_API_KEY")
            && !lastfm_key.trim().is_empty()
        {
            let lastfm_path = secret_dir.join("lastfm-api-key");
            write_secret(&lastfm_path, &lastfm_key)?;
            config.lastfm_api_key_file = Some(lastfm_path);
        }
        let plex_token = std::env::var("PLEX_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        if let Some(token) = plex_token {
            let section_id = std::env::var("PLEX_SECTION_ID")
                .context("PLEX_SECTION_ID is required when PLEX_TOKEN is set")?
                .parse()
                .context("parse PLEX_SECTION_ID")?;
            let library_roots = std::env::var("PLEX_LIBRARY_ROOTS")
                .context("PLEX_LIBRARY_ROOTS is required when PLEX_TOKEN is set")?
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .collect();
            let token_file = secret_dir.join("plex-token");
            write_secret(&token_file, &token)?;
            config.plex = Some(PlexConfig {
                base_url: std::env::var("PLEX_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:32400".into()),
                token_file,
                section_id,
                library_roots,
            });
        }

        config.trackers.insert(
            "ops".into(),
            TrackerConfig {
                kind: TrackerKind::Ops,
                base_url: "https://orpheus.network".into(),
                token_file: ops_path,
                announce_hosts: vec!["home.opsfet.ch".into()],
            },
        );
        if red_token.is_some() {
            config.trackers.insert(
                "red".into(),
                TrackerConfig {
                    kind: TrackerKind::Red,
                    base_url: "https://redacted.sh".into(),
                    token_file: red_path,
                    announce_hosts: vec!["flacsfor.me".into()],
                },
            );
        }
        config.download_clients.insert(
            "music".into(),
            DownloadClientConfig {
                kind: DownloadClientKind::Qbittorrent,
                base_url: std::env::var("QBITTORRENT_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8001".into()),
                api_key_file: qbit_path,
            },
        );
        config.download_profiles.insert(
            "ops".into(),
            DownloadProfileConfig {
                client: "music".into(),
                save_path: "/mnt/media/Downloads/torrent/complete/ops".into(),
                tag: "ops".into(),
                start_paused: false,
            },
        );
        if red_token.is_some() {
            config.download_profiles.insert(
                "red".into(),
                DownloadProfileConfig {
                    client: "music".into(),
                    save_path: "/mnt/media/Downloads/torrent/complete/red".into(),
                    tag: "red".into(),
                    start_paused: false,
                },
            );
        }
        Ok(config)
    }
}

pub fn read_secret(path: &Path) -> Result<String> {
    let value =
        std::fs::read_to_string(path).with_context(|| format!("read secret {}", path.display()))?;
    let value = value.trim();
    if value.is_empty() {
        bail!("secret {} is empty", path.display());
    }
    Ok(value.to_owned())
}

fn normalize_base_path(value: &str) -> Result<String> {
    if value == "/" || value.is_empty() {
        return Ok("/".into());
    }
    if !value.starts_with('/') || value.ends_with('/') {
        bail!("base_path must begin with / and must not end with /");
    }
    Ok(value.to_owned())
}

#[cfg(unix)]
fn write_secret(path: &Path, value: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(value.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret(path: &Path, value: &str) -> Result<()> {
    std::fs::write(path, value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn parses_snake_case_download_profile_fields() {
        let config: Config = toml::from_str(
            r#"
            [download_profiles.ops]
            client = "music"
            save_path = "/downloads/ops"
            tag = "ops"
            start_paused = true
            "#,
        )
        .expect("parse configuration");

        let profile = config.download_profiles.get("ops").expect("OPS profile");
        assert_eq!(profile.save_path, "/downloads/ops");
        assert!(profile.start_paused);
    }
}
