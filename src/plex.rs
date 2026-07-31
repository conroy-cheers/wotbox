use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use crate::{
    config::{PlexConfig, read_secret},
    provider::{ProviderFailure, ProviderFailureKind, ProviderGovernor, RequestClass, retry_after},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexScanTarget {
    pub section_id: u32,
    pub root: String,
}

impl PlexScanTarget {
    pub fn key_for_bucket(&self, bucket: i64) -> String {
        let digest = Sha256::digest(format!("{}\0{}", self.section_id, self.root).as_bytes());
        format!(
            "plex-scan:{}:{}:{bucket}:v1",
            self.section_id,
            &hex::encode(digest)[..16]
        )
    }
}

#[derive(Clone)]
pub struct PlexIntegration {
    base_url: String,
    token: Arc<str>,
    section_id: u32,
    library_roots: Arc<Vec<String>>,
}

impl PlexIntegration {
    pub fn new(config: &PlexConfig) -> Result<Self> {
        let base_url = config.base_url.trim_end_matches('/').to_owned();
        let token = read_secret(&config.token_file)?;
        let mut library_roots = config
            .library_roots
            .iter()
            .map(|root| root.to_string_lossy().trim_end_matches('/').to_owned())
            .collect::<Vec<_>>();
        library_roots.sort_by_key(|root| std::cmp::Reverse(root.len()));
        library_roots.dedup();
        Ok(Self {
            base_url,
            token: Arc::from(token),
            section_id: config.section_id,
            library_roots: Arc::new(library_roots),
        })
    }

    pub fn section_id(&self) -> u32 {
        self.section_id
    }

    pub fn library_roots(&self) -> &[String] {
        &self.library_roots
    }

    pub fn targets(&self) -> Vec<PlexScanTarget> {
        self.library_roots
            .iter()
            .map(|root| PlexScanTarget {
                section_id: self.section_id,
                root: root.clone(),
            })
            .collect()
    }

    pub fn target_for_path(&self, value: &str) -> Option<PlexScanTarget> {
        let path = Path::new(value);
        self.library_roots
            .iter()
            .find(|root| path.starts_with(Path::new(root.as_str())))
            .map(|root| PlexScanTarget {
                section_id: self.section_id,
                root: root.clone(),
            })
    }

    pub fn allows(&self, target: &PlexScanTarget) -> bool {
        target.section_id == self.section_id && self.library_roots.contains(&target.root)
    }

    pub async fn scan(
        &self,
        client: &reqwest::Client,
        providers: &ProviderGovernor,
        target: &PlexScanTarget,
    ) -> Result<()> {
        if !self.allows(target) {
            bail!("Plex scan target is not configured");
        }
        let endpoint = format!(
            "{}/library/sections/{}/refresh",
            self.base_url, target.section_id
        );
        providers
            .execute("plex", RequestClass::Background, || async {
                let response = client
                    .get(&endpoint)
                    .header("X-Plex-Token", self.token.as_ref())
                    .query(&[("path", target.root.as_str())])
                    .send()
                    .await
                    .map_err(|error| ProviderFailure::new(ProviderFailureKind::Transient, error))?;
                let status = response.status();
                let retry = retry_after(&response);
                if status.is_success() {
                    return Ok(());
                }
                let kind = match status {
                    StatusCode::TOO_MANY_REQUESTS => ProviderFailureKind::RateLimited,
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                        ProviderFailureKind::Authentication
                    }
                    status if status.is_server_error() => ProviderFailureKind::Transient,
                    _ => ProviderFailureKind::Permanent,
                };
                let failure =
                    ProviderFailure::new(kind, format!("Plex scan returned HTTP {status}"));
                Err(if kind == ProviderFailureKind::RateLimited {
                    failure.retry_after(retry)
                } else {
                    failure
                })
            })
            .await
            .context("notify Plex of library changes")
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::{NamedTempFile, tempdir};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path, query_param},
    };

    use crate::{
        db::Database,
        model::ApiPreferences,
        provider::{ProviderDefinition, ProviderGovernor},
    };

    use super::*;

    fn integration() -> PlexIntegration {
        let token = NamedTempFile::new().expect("token file");
        std::fs::write(token.path(), "secret").expect("write token");
        PlexIntegration::new(&PlexConfig {
            base_url: "http://127.0.0.1:32400".into(),
            token_file: token.path().to_path_buf(),
            section_id: 4,
            library_roots: vec![
                PathBuf::from("/music/ops"),
                PathBuf::from("/music/ops/special"),
            ],
        })
        .expect("integration")
    }

    #[test]
    fn matches_the_longest_configured_library_root() {
        let plex = integration();
        assert_eq!(
            plex.target_for_path("/music/ops/special/album"),
            Some(PlexScanTarget {
                section_id: 4,
                root: "/music/ops/special".into(),
            })
        );
        assert!(plex.target_for_path("/music/red/album").is_none());
    }

    #[tokio::test]
    async fn partial_scan_sends_token_in_header_and_path_as_query_parameter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/library/sections/4/refresh"))
            .and(header("X-Plex-Token", "secret"))
            .and(query_param("path", "/music/ops"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let token = NamedTempFile::new().expect("token file");
        std::fs::write(token.path(), "secret").expect("write token");
        let plex = PlexIntegration::new(&PlexConfig {
            base_url: server.uri(),
            token_file: token.path().to_path_buf(),
            section_id: 4,
            library_roots: vec![PathBuf::from("/music/ops")],
        })
        .expect("integration");
        let directory = tempdir().expect("temporary directory");
        let db = Database::open(&directory.path().join("provider.sqlite"))
            .await
            .expect("database");
        let providers = ProviderGovernor::new(
            db,
            vec![ProviderDefinition::plex()],
            &ApiPreferences::default(),
        )
        .await
        .expect("governor");

        plex.scan(
            &reqwest::Client::new(),
            &providers,
            &PlexScanTarget {
                section_id: 4,
                root: "/music/ops".into(),
            },
        )
        .await
        .expect("scan");
    }
}
