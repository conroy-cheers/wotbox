use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrackerDownloadMode {
    Disabled,
    FreeleechOnly,
    FreeleechOrToken,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrackerPreference {
    pub tracker: String,
    pub mode: TrackerDownloadMode,
    pub auto_use_tokens: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_profile: Option<String>,
    #[serde(default = "default_auto_token_limit")]
    pub auto_token_limit: u32,
}

fn default_auto_token_limit() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LeechStatus {
    #[default]
    Regular,
    Freeleech,
    PersonalFreeleech,
    Neutral,
    Freeload,
}

impl LeechStatus {
    pub fn has_no_download_debit(self) -> bool {
        self != Self::Regular
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadEligibilityReason {
    Eligible,
    TrackerDisabled,
    FreeleechRequired,
    TokenUnavailable,
    TokenCostUnknown,
    BelowQualityCutoff,
    BelowMediaCutoff,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadEligibility {
    pub eligible: bool,
    pub reason: DownloadEligibilityReason,
    pub requires_token: bool,
    pub token_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_cost: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub tracker: String,
    pub fetched_at: DateTime<Utc>,
    pub cache_age_seconds: i64,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiEnvelope<T> {
    pub data: T,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: Option<i64>,
    pub username: String,
    pub uploaded: Option<i64>,
    pub downloaded: Option<i64>,
    pub ratio: Option<f64>,
    pub required_ratio: Option<f64>,
    pub user_class: Option<String>,
    pub bonus_points: Option<f64>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackerAccount {
    pub tracker: String,
    pub account: Account,
    pub provenance: Provenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub current_page: i64,
    pub total_pages: i64,
    pub total_results: Option<i64>,
    pub groups: Vec<SearchGroup>,
    #[serde(default)]
    pub deduplication: DeduplicationIndexStatus,
    #[serde(default)]
    pub source_status: Vec<SourceLoadStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceLoadStatus {
    pub tracker: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchGroup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub tracker: String,
    pub group_id: i64,
    pub name: String,
    pub artist: Option<String>,
    pub year: Option<i64>,
    pub release_type: Option<String>,
    pub image: Option<String>,
    pub tags: Vec<String>,
    pub torrents: Vec<SearchTorrent>,
    #[serde(default)]
    pub sources: Vec<ReleaseSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_coverage: Option<AlbumCoverage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumReference {
    pub tracker: String,
    pub group_id: i64,
    pub title: String,
    pub year: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoverageConfidence {
    Exact,
    Fuzzy,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumCoverage {
    pub albums: Vec<AlbumReference>,
    pub confidence: CoverageConfidence,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct DeduplicationIndexStatus {
    pub checked: usize,
    pub total: usize,
    pub pending: usize,
    pub resolving: usize,
    pub failed: usize,
    pub hidden: usize,
    pub tracklists_indexed: usize,
    pub tracklists_total: usize,
    pub tracklists_pending: usize,
    pub tracklists_resolving: usize,
    pub tracklists_failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchTorrent {
    #[serde(default)]
    pub tracker: String,
    pub torrent_id: i64,
    pub edition_id: Option<i64>,
    pub format: Option<String>,
    pub encoding: Option<String>,
    pub media: Option<String>,
    pub size: Option<i64>,
    pub seeders: Option<i64>,
    pub leechers: Option<i64>,
    pub snatched: Option<i64>,
    pub freeleech: bool,
    #[serde(default)]
    pub leech_status: LeechStatus,
    pub can_use_token: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility: Option<DownloadEligibility>,
    pub remaster_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
    #[serde(default)]
    pub downloads: Vec<LiveDownloadStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TorrentMetadata {
    pub torrent_id: i64,
    pub group_id: Option<i64>,
    pub name: String,
    pub info_hash: Option<String>,
    pub can_use_token: bool,
    #[serde(default)]
    pub token_eligibility_known: bool,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProfile {
    pub name: String,
    pub client: String,
    pub save_path: String,
    pub tag: String,
    pub start_paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateDownload {
    pub tracker: String,
    pub torrent_id: i64,
    pub profile: String,
    #[serde(default)]
    pub use_token: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VariantSortCriterion {
    Quality,
    Tracker,
    Media,
    Edition,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReleasePreferences {
    #[serde(default = "default_quality_tiers")]
    pub quality_tiers: Vec<Vec<String>>,
    #[serde(default = "default_quality_cutoff")]
    pub quality_cutoff_index: usize,
    pub media_tiers: Vec<Vec<String>>,
    #[serde(default = "default_media_cutoff")]
    pub media_cutoff_index: usize,
    #[serde(default = "default_variant_sort_order")]
    pub variant_sort_order: Vec<VariantSortCriterion>,
    #[serde(default = "default_tracker_order")]
    pub tracker_order: Vec<String>,
    #[serde(default = "default_tracker_preferences")]
    pub tracker_policies: Vec<TrackerPreference>,
    #[serde(default, rename = "qualityOrder", skip_serializing)]
    pub legacy_quality_order: Vec<String>,
    #[serde(default, rename = "minimumQuality", skip_serializing)]
    pub legacy_minimum_quality: String,
}

fn default_quality_tiers() -> Vec<Vec<String>> {
    vec![
        vec!["hi_res".into()],
        vec!["lossless".into()],
        vec!["320".into()],
        vec!["v0".into()],
        vec!["other".into()],
    ]
}

fn default_quality_cutoff() -> usize {
    2
}

fn default_media_tiers() -> Vec<Vec<String>> {
    vec![
        vec!["WEB".into(), "CD".into()],
        vec!["SACD".into(), "DVD".into(), "Blu-ray".into()],
        vec!["Vinyl".into()],
        vec!["Cassette".into()],
        vec!["Other".into()],
    ]
}

fn default_media_cutoff() -> usize {
    2
}

fn default_variant_sort_order() -> Vec<VariantSortCriterion> {
    vec![
        VariantSortCriterion::Quality,
        VariantSortCriterion::Tracker,
        VariantSortCriterion::Media,
        VariantSortCriterion::Edition,
    ]
}

fn default_tracker_order() -> Vec<String> {
    vec!["ops".into(), "red".into()]
}

fn default_tracker_preferences() -> Vec<TrackerPreference> {
    vec![
        TrackerPreference {
            tracker: "ops".into(),
            mode: TrackerDownloadMode::FreeleechOrToken,
            auto_use_tokens: true,
            download_profile: Some("ops".into()),
            auto_token_limit: 1,
        },
        TrackerPreference {
            tracker: "red".into(),
            mode: TrackerDownloadMode::FreeleechOnly,
            auto_use_tokens: false,
            download_profile: Some("red".into()),
            auto_token_limit: 0,
        },
    ]
}

impl Default for ReleasePreferences {
    fn default() -> Self {
        Self {
            quality_tiers: default_quality_tiers(),
            quality_cutoff_index: default_quality_cutoff(),
            media_tiers: default_media_tiers(),
            media_cutoff_index: default_media_cutoff(),
            variant_sort_order: default_variant_sort_order(),
            tracker_order: default_tracker_order(),
            tracker_policies: default_tracker_preferences(),
            legacy_quality_order: Vec::new(),
            legacy_minimum_quality: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePreferences {
    pub release: ReleasePreferences,
    #[serde(default)]
    pub api: ApiPreferences,
    #[serde(default)]
    pub imports: ImportPreferences,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreferences {
    /// What to do with an explicitly superseded torrent after its replacement is complete.
    #[serde(default)]
    pub trumped_cleanup: ImportCleanupMode,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportCleanupMode {
    /// Keep both the old torrent and its payload. This is the safe application default.
    #[default]
    Keep,
    /// Remove only the old torrent from the client, retaining its payload on disk.
    RemoveTorrent,
    /// Remove the old torrent and ask the client to delete its payload.
    DeleteFiles,
}

impl ImportCleanupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::RemoveTorrent => "remove_torrent",
            Self::DeleteFiles => "delete_files",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiPreferences {
    #[serde(default)]
    pub providers: std::collections::BTreeMap<String, ProviderPolicyOverride>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPolicyOverride {
    pub minimum_interval_ms: Option<u64>,
    pub background_minimum_interval_ms: Option<u64>,
    pub max_concurrency: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCircuitState {
    Available,
    Cooldown,
    HalfOpen,
    Blocked,
    Paused,
}

impl ProviderCircuitState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Cooldown => "cooldown",
            Self::HalfOpen => "half_open",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
        }
    }
}

impl std::str::FromStr for ProviderCircuitState {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "available" => Self::Available,
            "cooldown" => Self::Cooldown,
            "half_open" => Self::HalfOpen,
            "blocked" => Self::Blocked,
            "paused" => Self::Paused,
            _ => anyhow::bail!("unknown provider circuit state {value}"),
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderQueueCounts {
    pub interactive: usize,
    pub download: usize,
    pub manual: usize,
    pub scheduled: usize,
    pub background: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub id: String,
    pub display_name: String,
    pub kind: String,
    pub state: ProviderCircuitState,
    pub reason_code: Option<String>,
    pub message: Option<String>,
    pub last_request_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
    pub retry_at: Option<DateTime<Utc>>,
    pub last_background_request_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub minimum_interval_ms: u64,
    pub safe_minimum_interval_ms: u64,
    pub background_minimum_interval_ms: u64,
    pub safe_background_minimum_interval_ms: u64,
    pub max_concurrency: u32,
    pub safe_max_concurrency: u32,
    pub queued: ProviderQueueCounts,
    pub can_pause: bool,
    pub can_resume: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobState {
    Pending,
    Running,
    Retrying,
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

impl std::str::FromStr for BackgroundJobState {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "retrying" => Self::Retrying,
            "waiting" => Self::Waiting,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => anyhow::bail!("unknown background job state {value}"),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundJobStatus {
    pub id: Uuid,
    pub deduplication_key: String,
    pub kind: String,
    pub state: BackgroundJobState,
    pub provider_id: Option<String>,
    pub lane: String,
    pub priority: i64,
    pub attempts: u32,
    pub deferrals: u64,
    pub max_attempts: u32,
    pub next_run_at: Option<DateTime<Utc>>,
    pub lease_until: Option<DateTime<Utc>>,
    pub progress_completed: u64,
    pub progress_total: Option<u64>,
    pub progress_message: Option<String>,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
    pub parent_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub can_cancel: bool,
    pub can_retry: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundJobCounts {
    pub pending: u64,
    pub running: u64,
    pub retrying: u64,
    pub waiting: u64,
    pub completed: u64,
    pub failed: u64,
    pub cancelled: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundJobsOverview {
    pub counts: BackgroundJobCounts,
    pub jobs: Vec<BackgroundJobStatus>,
}

impl ReleasePreferences {
    pub fn migrate_legacy(mut self) -> Self {
        let was_legacy = !self.legacy_quality_order.is_empty();
        if !self.legacy_quality_order.is_empty() {
            let cutoff = self
                .legacy_quality_order
                .iter()
                .position(|value| value == &self.legacy_minimum_quality)
                .map(|index| index + 1)
                .unwrap_or_else(default_quality_cutoff);
            self.quality_tiers = self
                .legacy_quality_order
                .drain(..)
                .map(|value| vec![value])
                .collect();
            self.quality_cutoff_index = cutoff;
            self.legacy_minimum_quality.clear();
        }
        let legacy_default = vec![
            vec!["WEB".to_owned(), "CD".to_owned()],
            vec!["Vinyl".to_owned()],
            vec!["SACD".to_owned(), "DVD".to_owned(), "Blu-ray".to_owned()],
            vec!["Cassette".to_owned()],
            vec!["Other".to_owned()],
        ];
        if self.media_tiers == legacy_default {
            self.media_tiers = default_media_tiers();
            self.media_cutoff_index = default_media_cutoff();
        } else if was_legacy {
            self.media_cutoff_index = self.media_tiers.len();
        }
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        const QUALITIES: [&str; 5] = ["hi_res", "lossless", "320", "v0", "other"];
        if self.quality_tiers.is_empty()
            || self.quality_tiers.iter().any(Vec::is_empty)
            || self.quality_tiers.iter().flatten().count() != QUALITIES.len()
            || QUALITIES.iter().any(|quality| {
                self.quality_tiers
                    .iter()
                    .flatten()
                    .filter(|item| item == quality)
                    .count()
                    != 1
            })
        {
            return Err(
                "qualityTiers must contain hi_res, lossless, 320, v0, and other exactly once"
                    .into(),
            );
        }
        if self.quality_cutoff_index > self.quality_tiers.len() {
            return Err("qualityCutoffIndex must be between zero and the tier count".into());
        }
        if self.media_tiers.is_empty() || self.media_tiers.iter().any(Vec::is_empty) {
            return Err("mediaTiers must contain at least one non-empty tier".into());
        }
        if self.media_cutoff_index > self.media_tiers.len() {
            return Err("mediaCutoffIndex must be between zero and the tier count".into());
        }
        let mut media = std::collections::HashSet::new();
        for value in self.media_tiers.iter().flatten() {
            let normalized = value.trim().to_ascii_lowercase();
            if normalized.is_empty() || !media.insert(normalized) {
                return Err("media values must be non-empty and unique across tiers".into());
            }
        }
        const CRITERIA: [VariantSortCriterion; 4] = [
            VariantSortCriterion::Quality,
            VariantSortCriterion::Tracker,
            VariantSortCriterion::Media,
            VariantSortCriterion::Edition,
        ];
        if self.variant_sort_order.len() != CRITERIA.len()
            || CRITERIA.iter().any(|criterion| {
                self.variant_sort_order
                    .iter()
                    .filter(|value| *value == criterion)
                    .count()
                    != 1
            })
        {
            return Err(
                "variantSortOrder must contain quality, tracker, media, and edition exactly once"
                    .into(),
            );
        }
        let normalized_order = self
            .tracker_order
            .iter()
            .map(|tracker| tracker.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        if normalized_order.iter().any(String::is_empty)
            || normalized_order
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != normalized_order.len()
        {
            return Err("trackerOrder must contain unique, non-empty tracker names".into());
        }
        let mut policies = std::collections::HashSet::new();
        for policy in &self.tracker_policies {
            let tracker = policy.tracker.trim().to_ascii_lowercase();
            if tracker.is_empty() || !policies.insert(tracker.clone()) {
                return Err("trackerPolicies must contain unique, non-empty tracker names".into());
            }
            if !normalized_order.contains(&tracker) {
                return Err("every tracker policy must be present in trackerOrder".into());
            }
            if policy
                .download_profile
                .as_deref()
                .is_some_and(|profile| profile.trim().is_empty())
            {
                return Err("downloadProfile must be non-empty when configured".into());
            }
            if policy.auto_token_limit > 100 {
                return Err("autoTokenLimit cannot exceed 100".into());
            }
        }
        Ok(())
    }

    pub fn quality_class(format: Option<&str>, encoding: Option<&str>) -> &'static str {
        let format = format.unwrap_or_default().to_ascii_lowercase();
        let encoding = encoding.unwrap_or_default().to_ascii_lowercase();
        if encoding.contains("24bit") || encoding.contains("24-bit") || encoding.contains("24 bit")
        {
            "hi_res"
        } else if encoding.contains("lossless") || format.contains("flac") {
            "lossless"
        } else if encoding.contains("320") {
            "320"
        } else if encoding.contains("v0") {
            "v0"
        } else {
            "other"
        }
    }

    pub fn quality_rank(&self, format: Option<&str>, encoding: Option<&str>) -> usize {
        self.quality_tiers
            .iter()
            .position(|tier| {
                tier.iter()
                    .any(|item| item == Self::quality_class(format, encoding))
            })
            .unwrap_or(self.quality_tiers.len())
    }

    pub fn media_rank(&self, media: Option<&str>) -> usize {
        let media = media.unwrap_or("other");
        self.media_tiers
            .iter()
            .position(|tier| tier.iter().any(|item| item.eq_ignore_ascii_case(media)))
            .or_else(|| {
                self.media_tiers
                    .iter()
                    .position(|tier| tier.iter().any(|item| item.eq_ignore_ascii_case("other")))
            })
            .unwrap_or(self.media_tiers.len())
    }

    pub fn allows_quality(&self, format: Option<&str>, encoding: Option<&str>) -> bool {
        self.quality_rank(format, encoding) < self.quality_cutoff_index
    }

    pub fn allows_media(&self, media: Option<&str>) -> bool {
        self.media_rank(media) < self.media_cutoff_index
    }

    pub fn tracker_policy(&self, tracker: &str) -> TrackerPreference {
        self.tracker_policies
            .iter()
            .find(|policy| policy.tracker.eq_ignore_ascii_case(tracker))
            .cloned()
            .unwrap_or_else(|| TrackerPreference {
                tracker: tracker.to_ascii_lowercase(),
                mode: TrackerDownloadMode::FreeleechOnly,
                auto_use_tokens: false,
                download_profile: None,
                auto_token_limit: default_auto_token_limit(),
            })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn eligibility(
        &self,
        tracker: &str,
        format: Option<&str>,
        encoding: Option<&str>,
        media: Option<&str>,
        size: Option<i64>,
        leech_status: LeechStatus,
        can_use_token: bool,
    ) -> DownloadEligibility {
        let free = leech_status.has_no_download_debit();
        let token_cost = if free {
            Some(0)
        } else if can_use_token {
            Self::token_cost(tracker, size)
        } else {
            None
        };
        if !self.allows_quality(format, encoding) {
            return DownloadEligibility {
                eligible: false,
                reason: DownloadEligibilityReason::BelowQualityCutoff,
                requires_token: false,
                token_available: can_use_token,
                token_cost,
            };
        }
        if !self.allows_media(media) {
            return DownloadEligibility {
                eligible: false,
                reason: DownloadEligibilityReason::BelowMediaCutoff,
                requires_token: false,
                token_available: can_use_token,
                token_cost,
            };
        }
        let policy = self.tracker_policy(tracker);
        let (eligible, reason, requires_token) = match policy.mode {
            TrackerDownloadMode::Disabled => {
                (false, DownloadEligibilityReason::TrackerDisabled, false)
            }
            TrackerDownloadMode::FreeleechOnly if !free => {
                (false, DownloadEligibilityReason::FreeleechRequired, false)
            }
            TrackerDownloadMode::FreeleechOrToken if !free && !can_use_token => {
                (false, DownloadEligibilityReason::TokenUnavailable, true)
            }
            TrackerDownloadMode::FreeleechOrToken if !free && token_cost.is_none() => {
                (false, DownloadEligibilityReason::TokenCostUnknown, true)
            }
            TrackerDownloadMode::FreeleechOrToken if !free => {
                (true, DownloadEligibilityReason::Eligible, true)
            }
            _ => (true, DownloadEligibilityReason::Eligible, false),
        };
        DownloadEligibility {
            eligible,
            reason,
            requires_token,
            token_available: can_use_token,
            token_cost,
        }
    }

    pub fn token_cost(tracker: &str, size: Option<i64>) -> Option<u32> {
        const OPS_TOKEN_BYTES: u64 = 320 * 1024 * 1024;
        if tracker.eq_ignore_ascii_case("red") {
            return Some(1);
        }
        if !tracker.eq_ignore_ascii_case("ops") {
            return None;
        }
        let size = u64::try_from(size?).ok().filter(|size| *size > 0)?;
        u32::try_from(size.div_ceil(OPS_TOKEN_BYTES)).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ClientDownloadState {
    Downloading,
    Seeding,
    Paused,
    Queued,
    Checking,
    Stalled,
    Complete,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadDiagnostic {
    pub code: String,
    pub summary: String,
    pub message: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LiveDownloadStatus {
    pub client: String,
    pub info_hash: String,
    pub state: ClientDownloadState,
    pub client_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<DownloadDiagnostic>,
    pub progress: f64,
    pub size: i64,
    pub downloaded: i64,
    pub uploaded: i64,
    pub download_speed: i64,
    pub upload_speed: i64,
    pub eta: Option<i64>,
    pub ratio: f64,
    pub save_path: String,
    #[serde(default, skip_serializing)]
    #[schema(ignore)]
    pub content_path: Option<String>,
    pub added_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseDownload {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracker: Option<String>,
    pub in_library: bool,
    pub live: LiveDownloadStatus,
}

#[derive(Debug, Clone)]
pub struct ObservedDownload {
    pub name: String,
    pub live: LiveDownloadStatus,
    pub announce_host: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadTrackerStatus {
    pub announce_host: Option<String>,
    pub status: i64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadFile {
    pub name: String,
    pub size: i64,
    pub progress: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CrossSeedPlan {
    pub source_tracker: String,
    pub source_torrent_id: i64,
    pub source_client: String,
    pub source_info_hash: String,
    pub source_path: String,
    pub target_tracker: String,
    pub target_torrent_id: i64,
    pub compatible: bool,
    pub matched_files: usize,
    pub target_files: usize,
    pub missing_files: Vec<String>,
    pub policy_eligible: bool,
    pub summary: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub tracker: String,
    pub group_id: i64,
    pub title: String,
    pub artist: Option<String>,
    #[serde(default)]
    pub artists: Vec<ArtistCredit>,
    pub year: Option<i64>,
    pub artwork: Option<String>,
    pub release_type: Option<String>,
    #[serde(default)]
    pub sources: Vec<ReleaseSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_coverage: Option<AlbumCoverage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSource {
    pub tracker: String,
    pub group_id: i64,
    #[serde(default)]
    pub match_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseMatchRecord {
    pub matcher_version: i32,
    pub sources: Vec<ReleaseSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ArtistMatchRecord {
    pub matcher_version: i32,
    pub sources: Vec<ArtistSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ArtistSource {
    pub tracker: String,
    pub artist_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ArtistRole {
    Primary,
    Guest,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ArtistCreditSource {
    Structured,
    DisplayFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtistCredit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_id: Option<Uuid>,
    pub key: String,
    pub tracker: String,
    pub artist_id: Option<i64>,
    pub name: String,
    pub role: ArtistRole,
    pub source: ArtistCreditSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TorrentVariant {
    pub tracker: String,
    pub torrent_id: i64,
    pub group_id: i64,
    pub info_hash: Option<String>,
    pub format: Option<String>,
    pub encoding: Option<String>,
    pub media: Option<String>,
    pub size: Option<i64>,
    pub seeders: Option<i64>,
    pub leechers: Option<i64>,
    pub snatched: Option<i64>,
    pub freeleech: bool,
    #[serde(default)]
    pub leech_status: LeechStatus,
    pub can_use_token: bool,
    #[serde(default)]
    pub token_eligibility_known: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligibility: Option<DownloadEligibility>,
    pub remaster_title: Option<String>,
    #[serde(default)]
    pub downloads: Vec<LiveDownloadStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<LibraryVariantState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseDetail {
    pub release: ReleaseSummary,
    #[serde(default)]
    pub field_provenance: Value,
    pub tags: Vec<String>,
    pub description: Option<String>,
    pub record_label: Option<String>,
    pub variants: Vec<TorrentVariant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalTorrent {
    pub release: ReleaseSummary,
    pub variant: TorrentVariant,
    pub tags: Vec<String>,
    pub description: Option<String>,
    pub record_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalDownload {
    pub release: ReleaseSummary,
    pub variant: TorrentVariant,
    pub download: LiveDownloadStatus,
    pub provenance: Provenance,
    pub live_observed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub live_stale: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadIndexCounts {
    pub linked: i64,
    pub pending: i64,
    pub resolving: i64,
    pub failed: i64,
    pub unconfigured: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadsPage {
    pub items: Vec<CanonicalDownload>,
    pub total: i64,
    pub index: DownloadIndexCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportTaskState {
    Downloading,
    Resolving,
    NeedsReview,
    Ready,
    Processing,
    Complete,
    Blocked,
    Failed,
    Dismissed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportSupersession {
    pub source_client: String,
    pub source_info_hash: String,
    pub tracker: String,
    pub source_name: String,
    pub cleanup_mode: ImportCleanupMode,
    pub cleanup_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<LiveDownloadStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportTask {
    pub id: Uuid,
    pub state: ImportTaskState,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_job_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub baseline: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<LiveDownloadStatus>,
    #[serde(default)]
    pub supersessions: Vec<ImportSupersession>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportTaskCounts {
    pub active: i64,
    pub review: i64,
    pub complete: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportsPage {
    pub items: Vec<ImportTask>,
    pub total: i64,
    pub counts: ImportTaskCounts,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LibraryAvailability {
    Present,
    Partial,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCopy {
    pub client: String,
    pub info_hash: String,
    pub present: bool,
    pub completed_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub missing_since: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryVariantState {
    pub availability: LibraryAvailability,
    pub copies: Vec<LibraryCopy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryRelease {
    pub release: ReleaseSummary,
    pub variants: Vec<TorrentVariant>,
    pub availability: LibraryAvailability,
    pub added_at: DateTime<Utc>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryArtistSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub key: String,
    pub tracker: String,
    pub artist_id: Option<i64>,
    pub credit_source: ArtistCreditSource,
    pub name: String,
    pub release_count: usize,
    pub missing_count: usize,
    pub artworks: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryIndexStatus {
    pub last_successful_scan_at: Option<DateTime<Utc>>,
    pub unresolved_credits: usize,
    #[serde(default)]
    pub deduplication: DeduplicationIndexStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryArtistsPage {
    pub artists: Vec<LibraryArtistSummary>,
    pub releases: Vec<LibraryRelease>,
    pub artist_total: usize,
    pub release_total: usize,
    pub index: LibraryIndexStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryArtistPage {
    pub artist: LibraryArtistSummary,
    pub items: Vec<LibraryRelease>,
    pub total: usize,
    pub index: LibraryIndexStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtistCatalogRole {
    Primary,
    Guest,
    Remixer,
    Composer,
    Conductor,
    Dj,
    Producer,
    Arranger,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtistCatalogArtist {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    pub tracker: String,
    pub artist_id: i64,
    pub name: String,
    pub artwork: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtistCatalogRelease {
    pub release: ReleaseSummary,
    pub tags: Vec<String>,
    pub variants: Vec<TorrentVariant>,
    pub roles: Vec<ArtistCatalogRole>,
    pub listed_on_tracker: bool,
    pub library_availability: Option<LibraryAvailability>,
    pub library_added_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtistCatalogPage {
    pub artist: ArtistCatalogArtist,
    pub groups: Vec<ArtistCatalogRelease>,
    pub primary_count: usize,
    pub appearance_count: usize,
    #[serde(default)]
    pub deduplication: DeduplicationIndexStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Queued,
    FetchingMetadata,
    Submitting,
    Active,
    Complete,
    Failed,
    Unknown,
}

impl DownloadState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::FetchingMetadata => "fetching_metadata",
            Self::Submitting => "submitting",
            Self::Active => "active",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

impl std::str::FromStr for DownloadState {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "queued" => Self::Queued,
            "fetching_metadata" => Self::FetchingMetadata,
            "submitting" => Self::Submitting,
            "active" => Self::Active,
            "complete" => Self::Complete,
            "failed" => Self::Failed,
            _ => Self::Unknown,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadJob {
    pub id: Uuid,
    pub tracker: String,
    pub torrent_id: i64,
    pub group_id: Option<i64>,
    pub profile: String,
    pub use_token: bool,
    pub info_hash: Option<String>,
    pub name: Option<String>,
    pub state: DownloadState,
    pub progress: f64,
    pub download_speed: i64,
    pub upload_speed: i64,
    pub eta: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicConfig {
    pub base_path: String,
    pub trackers: Vec<String>,
    #[serde(default)]
    pub tracker_sites: std::collections::BTreeMap<String, String>,
    pub download_profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlexIntegrationStatus {
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_id: Option<u32>,
    #[serde(default)]
    pub library_roots: Vec<String>,
    pub pending_scans: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlexScanQueued {
    pub job_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    CountryChart,
    Lastfm,
    TrumpedDownloads,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSchedule {
    /// Monday is 1 and Sunday is 7.
    pub weekday: u8,
    /// Local wall-clock time in HH:MM form.
    pub time: String,
    /// IANA timezone name.
    pub timezone: String,
}

impl Default for ChannelSchedule {
    fn default() -> Self {
        Self {
            weekday: 1,
            time: "06:00".into(),
            timezone: "Australia/Melbourne".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CountryChartChannelSettings {
    pub country: String,
    #[serde(default = "default_country_chart_album_count")]
    pub album_count: u16,
}

fn default_country_chart_album_count() -> u16 {
    100
}

impl Default for CountryChartChannelSettings {
    fn default() -> Self {
        Self {
            country: "AU".into(),
            album_count: default_country_chart_album_count(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LastfmChannelSettings {
    pub username: String,
    pub period: String,
    pub pack_size: u16,
    pub suppression_packs: u16,
    #[serde(default = "default_catalog_country")]
    pub catalog_country: String,
}

fn default_catalog_country() -> String {
    "AU".into()
}

impl Default for LastfmChannelSettings {
    fn default() -> Self {
        Self {
            username: String::new(),
            period: "3month".into(),
            pack_size: 25,
            suppression_packs: 8,
            catalog_country: default_catalog_country(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConfig {
    pub id: String,
    pub kind: ChannelKind,
    pub enabled: bool,
    pub schedule: ChannelSchedule,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country_chart: Option<CountryChartChannelSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lastfm: Option<LastfmChannelSettings>,
    #[serde(default)]
    pub credential_configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_refresh_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_successful_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub failure_count: u32,
    pub updated_at: DateTime<Utc>,
}

impl ChannelConfig {
    pub fn country_chart_default(now: DateTime<Utc>) -> Self {
        Self {
            id: "country_chart".into(),
            kind: ChannelKind::CountryChart,
            enabled: false,
            schedule: ChannelSchedule::default(),
            country_chart: Some(CountryChartChannelSettings::default()),
            lastfm: None,
            credential_configured: true,
            next_refresh_at: None,
            last_successful_at: None,
            last_attempt_at: None,
            last_error: None,
            failure_count: 0,
            updated_at: now,
        }
    }

    pub fn lastfm_default(now: DateTime<Utc>) -> Self {
        Self {
            id: "lastfm".into(),
            kind: ChannelKind::Lastfm,
            enabled: false,
            schedule: ChannelSchedule::default(),
            country_chart: None,
            lastfm: Some(LastfmChannelSettings::default()),
            credential_configured: false,
            next_refresh_at: None,
            last_successful_at: None,
            last_attempt_at: None,
            last_error: None,
            failure_count: 0,
            updated_at: now,
        }
    }

    pub fn trumped_downloads_default(now: DateTime<Utc>) -> Self {
        Self {
            id: "trumped_downloads".into(),
            kind: ChannelKind::TrumpedDownloads,
            enabled: true,
            schedule: ChannelSchedule::default(),
            country_chart: None,
            lastfm: None,
            credential_configured: true,
            next_refresh_at: None,
            last_successful_at: None,
            last_attempt_at: None,
            last_error: None,
            failure_count: 0,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRunStatus {
    Running,
    Successful,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRunTrigger {
    Scheduled,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRunPhase {
    Discovering,
    Matching,
    Planning,
    Saving,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRun {
    pub id: Uuid,
    pub channel_id: String,
    pub trigger: ChannelRunTrigger,
    pub status: ChannelRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<ChannelRunPhase>,
    #[serde(default)]
    pub progress_completed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelPackDecision {
    Open,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationMatchState {
    Matched,
    Unmatched,
    Ambiguous,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackItemPlanState {
    Executable,
    CleanupReady,
    AlreadyOwned,
    AlreadyDownloading,
    Duplicate,
    TokenBudgetExceeded,
    CapacityBlocked,
    Excluded,
    Unmatched,
    Ambiguous,
    PolicyBlocked,
    NoProfile,
    SourceError,
    Submitted,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecommendationSource {
    pub id: String,
    pub rank: u32,
    pub artist: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub substituted_from: Option<RecommendationSubstitution>,
    /// Exact client torrents represented by a grouped trumped-download recommendation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trumped_downloads: Vec<TrumpedDownloadRef>,
    /// Local qBittorrent file metadata used only while resolving a trumped download.
    #[serde(skip)]
    #[schema(ignore)]
    pub lookup_files: Vec<DownloadFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct TrumpedDownloadRef {
    pub client: String,
    pub info_hash: String,
    pub name: String,
    pub tracker: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecommendationSubstitution {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    pub release_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlannedDownload {
    pub tracker: String,
    pub torrent_id: i64,
    pub profile: String,
    pub use_token: bool,
    #[serde(default)]
    pub token_cost: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplacementTargetState {
    Missing,
    Downloading,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementTarget {
    pub tracker: String,
    pub torrent_id: i64,
    pub state: ReplacementTargetState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default)]
    pub downloads: Vec<ReleaseDownload>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPackItem {
    pub ordinal: u32,
    pub source: RecommendationSource,
    pub match_state: RecommendationMatchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseSummary>,
    #[serde(default)]
    pub variants: Vec<TorrentVariant>,
    #[serde(default)]
    pub candidates: Vec<ReleaseSummary>,
    #[serde(default)]
    pub downloads: Vec<ReleaseDownload>,
    pub plan_state: PackItemPlanState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlannedDownload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<ReplacementTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<DownloadJob>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPlanSummary {
    pub executable: usize,
    pub skipped: usize,
    pub total_size: i64,
    pub token_uses: usize,
    pub by_tracker: std::collections::BTreeMap<String, usize>,
    pub by_reason: std::collections::BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPack {
    pub id: Uuid,
    pub channel_id: String,
    pub decision: ChannelPackDecision,
    pub partial: bool,
    pub source_title: String,
    pub plan_version: i32,
    pub plan_stale: bool,
    pub summary: ChannelPlanSummary,
    #[serde(default)]
    pub items: Vec<ChannelPackItem>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPackSummary {
    pub id: Uuid,
    pub channel_id: String,
    pub decision: ChannelPackDecision,
    pub partial: bool,
    pub source_title: String,
    pub plan_version: i32,
    pub summary: ChannelPlanSummary,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelOverview {
    pub channel: ChannelConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run: Option<ChannelRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_pack: Option<ChannelPackSummary>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecideChannelPack {
    pub plan_version: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinals: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttachChannelPackItem {
    pub plan_version: i32,
    pub release_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelBatchResult {
    pub pack_id: Uuid,
    pub submitted: usize,
    pub skipped: usize,
    pub jobs: Vec<DownloadJob>,
}

pub fn sanitized(mut value: Value) -> Value {
    redact(&mut value);
    value
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let normalized = key.to_ascii_lowercase().replace(['_', '-'], "");
                if matches!(
                    normalized.as_str(),
                    "authkey" | "passkey" | "token" | "apikey" | "torrentpass"
                ) {
                    *child = Value::String("[redacted]".into());
                } else {
                    redact(child);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact),
        _ => {}
    }
}

pub fn value_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value.as_i64().or_else(|| value.as_str()?.parse().ok())
    })
}

pub fn value_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value.as_f64().or_else(|| value.as_str()?.parse().ok())
    })
}

pub fn value_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key)?.as_str().map(ToOwned::to_owned))
}

pub fn value_bool(value: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .find_map(|key| {
            let value = value.get(*key)?;
            value.as_bool().or_else(|| value.as_i64().map(|v| v != 0))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod preference_tests {
    use super::{
        DeduplicationIndexStatus, DownloadEligibilityReason, LeechStatus, ReleasePreferences,
    };

    #[test]
    fn default_cutoff_accepts_lossless_and_hi_res_only() {
        let preferences = ReleasePreferences::default();
        assert!(preferences.allows_quality(Some("FLAC"), Some("Lossless")));
        assert!(preferences.allows_quality(Some("FLAC"), Some("24bit Lossless")));
        assert!(!preferences.allows_quality(Some("MP3"), Some("320")));
        assert!(!preferences.allows_quality(Some("MP3"), Some("V0 (VBR)")));
        assert!(preferences.allows_media(Some("WEB")));
        assert!(!preferences.allows_media(Some("Vinyl")));
    }

    #[test]
    fn rejects_incomplete_or_duplicate_preference_orders() {
        let mut preferences = ReleasePreferences::default();
        preferences.quality_tiers[4] = vec!["lossless".into()];
        assert!(preferences.validate().is_err());

        let mut preferences = ReleasePreferences::default();
        preferences.media_tiers[1].push("web".into());
        assert!(preferences.validate().is_err());
    }

    #[test]
    fn old_tracker_preferences_receive_safe_pack_defaults() {
        let policy: super::TrackerPreference = serde_json::from_value(serde_json::json!({
            "tracker": "ops",
            "mode": "freeleech_or_token",
            "autoUseTokens": true
        }))
        .expect("legacy tracker preference");
        assert_eq!(policy.download_profile, None);
        assert_eq!(policy.auto_token_limit, 1);
    }

    #[test]
    fn migrates_legacy_quality_and_default_media_preferences() {
        let preferences: super::RuntimePreferences = serde_json::from_value(serde_json::json!({
            "release": {
                "qualityOrder": ["hi_res", "lossless", "320", "v0", "other"],
                "minimumQuality": "lossless",
                "mediaTiers": [["WEB", "CD"], ["Vinyl"], ["SACD", "DVD", "Blu-ray"], ["Cassette"], ["Other"]],
                "trackerOrder": ["ops", "red"],
                "trackerPolicies": []
            }
        }))
        .expect("legacy preferences");
        let migrated = preferences.release.migrate_legacy();
        assert_eq!(migrated.quality_cutoff_index, 2);
        assert_eq!(migrated.quality_tiers[1], vec!["lossless"]);
        assert_eq!(migrated.media_tiers[1], vec!["SACD", "DVD", "Blu-ray"]);
        assert_eq!(migrated.media_cutoff_index, 2);
    }

    #[test]
    fn defaults_to_no_debit_downloads_on_both_trackers() {
        let preferences = ReleasePreferences::default();
        let ops = preferences.eligibility(
            "ops",
            Some("FLAC"),
            Some("Lossless"),
            Some("WEB"),
            Some(320 * 1024 * 1024),
            LeechStatus::Regular,
            true,
        );
        assert!(ops.eligible);
        assert!(ops.requires_token);
        assert_eq!(ops.token_cost, Some(1));

        let red = preferences.eligibility(
            "red",
            Some("FLAC"),
            Some("Lossless"),
            Some("WEB"),
            Some(1),
            LeechStatus::Regular,
            true,
        );
        assert!(!red.eligible);
        assert_eq!(red.reason, DownloadEligibilityReason::FreeleechRequired);

        let red_free = preferences.eligibility(
            "red",
            Some("FLAC"),
            Some("Lossless"),
            Some("WEB"),
            Some(1),
            LeechStatus::PersonalFreeleech,
            false,
        );
        assert!(red_free.eligible);
        assert!(!red_free.requires_token);
        assert_eq!(red_free.token_cost, Some(0));
    }

    #[test]
    fn calculates_tracker_specific_token_costs_and_blocks_unknown_ops_sizes() {
        assert_eq!(ReleasePreferences::token_cost("ops", Some(1)), Some(1));
        assert_eq!(
            ReleasePreferences::token_cost("ops", Some(320 * 1024 * 1024)),
            Some(1)
        );
        assert_eq!(
            ReleasePreferences::token_cost("ops", Some(320 * 1024 * 1024 + 1)),
            Some(2)
        );
        assert_eq!(
            ReleasePreferences::token_cost("ops", Some(535_494_014)),
            Some(2)
        );
        assert_eq!(ReleasePreferences::token_cost("ops", None), None);
        assert_eq!(ReleasePreferences::token_cost("red", None), Some(1));

        let eligibility = ReleasePreferences::default().eligibility(
            "ops",
            Some("FLAC"),
            Some("Lossless"),
            Some("WEB"),
            None,
            LeechStatus::Regular,
            true,
        );
        assert!(!eligibility.eligible);
        assert_eq!(
            eligibility.reason,
            DownloadEligibilityReason::TokenCostUnknown
        );
    }

    #[test]
    fn old_deduplication_statuses_default_new_progress_fields() {
        let status: DeduplicationIndexStatus =
            serde_json::from_str(r#"{"pending":3,"resolving":1,"failed":0,"hidden":2}"#)
                .expect("old cached status should remain readable");

        assert_eq!(status.checked, 0);
        assert_eq!(status.total, 0);
        assert_eq!(status.pending, 3);
        assert_eq!(status.hidden, 2);
        assert_eq!(status.tracklists_indexed, 0);
    }
}
