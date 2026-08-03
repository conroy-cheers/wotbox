use std::{cmp::Ordering, collections::HashSet};

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::model::{ReleaseDetail, ReleaseSource, ReleaseSummary, SearchGroup};

pub const MATCHER_VERSION: i32 = 2;
pub const AUTO_MERGE_THRESHOLD: f64 = 0.88;
pub const DOWNLOAD_MATCH_THRESHOLD: f64 = 0.90;
pub const DOWNLOAD_MATCH_MARGIN: f64 = 0.035;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadReleaseIdentity {
    pub artist: String,
    pub title: String,
    pub year: Option<i64>,
}

impl PartialEq<(String, String, Option<i64>)> for DownloadReleaseIdentity {
    fn eq(&self, other: &(String, String, Option<i64>)) -> bool {
        self.artist == other.0 && self.title == other.1 && self.year == other.2
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DownloadReleaseMatch {
    pub release_id: uuid::Uuid,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DownloadMatchResult {
    Matched(DownloadReleaseMatch),
    Ambiguous {
        best: DownloadReleaseMatch,
        runner_up: DownloadReleaseMatch,
    },
    NotFound,
}

pub fn normalized(value: &str) -> String {
    value
        .replace(['&', '+'], " and ")
        .replace(['æ', 'Æ'], "ae")
        .replace(['ø', 'Ø'], "o")
        .replace(['ł', 'Ł'], "l")
        .replace('ß', "ss")
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn token_similarity(left: &str, right: &str) -> f64 {
    let left = normalized(left);
    let right = normalized(right);
    if left == right
        || (left.replace(' ', "") == right.replace(' ', "")
            && left.replace(' ', "").chars().count() >= 3)
    {
        return 1.0;
    }
    let left = left.split_whitespace().collect::<HashSet<_>>();
    let right = right.split_whitespace().collect::<HashSet<_>>();
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(&right).count() as f64;
    2.0 * intersection / (left.len() + right.len()) as f64
}

fn artist_similarity(left: &str, right: &str) -> f64 {
    let ordinary = token_similarity(left, right);
    let left_normalized = normalized(left);
    let right_normalized = normalized(right);
    let left_tokens = left_normalized
        .split_whitespace()
        .filter(|token| *token != "and")
        .collect::<HashSet<_>>();
    let right_tokens = right_normalized
        .split_whitespace()
        .filter(|token| *token != "and")
        .collect::<HashSet<_>>();
    let (smaller, larger) = if left_tokens.len() <= right_tokens.len() {
        (&left_tokens, &right_tokens)
    } else {
        (&right_tokens, &left_tokens)
    };
    let distinctive_solo = smaller.iter().next().is_some_and(|token| {
        smaller.len() == 1
            && token.chars().count() >= 5
            && !matches!(*token, "artist" | "unknown" | "various")
    });
    if (smaller.len() >= 2 || distinctive_solo)
        && smaller.is_subset(larger)
        && larger.len() - smaller.len() <= 2
    {
        ordinary.max(0.94)
    } else {
        ordinary
    }
}

fn title_similarity(left: &str, right: &str) -> f64 {
    let ordinary = token_similarity(left, right);
    let left_normalized = normalized(left);
    let right_normalized = normalized(right);
    let left_tokens = left_normalized.split_whitespace().collect::<HashSet<_>>();
    let right_tokens = right_normalized.split_whitespace().collect::<HashSet<_>>();
    let (smaller, larger) = if left_tokens.len() <= right_tokens.len() {
        (&left_tokens, &right_tokens)
    } else {
        (&right_tokens, &left_tokens)
    };
    let protected = [
        "live",
        "remix",
        "remixes",
        "version",
        "rerecorded",
        "re-recorded",
    ];
    let extras = larger.difference(smaller).copied().collect::<Vec<_>>();
    if !smaller.is_empty()
        && smaller.is_subset(larger)
        && extras.len() <= 2
        && !extras.iter().any(|word| protected.contains(word))
    {
        ordinary.max(0.96)
    } else {
        ordinary
    }
}

pub fn group_score(left: &SearchGroup, right: &SearchGroup) -> f64 {
    identity_score(
        &left.name,
        left.artist.as_deref(),
        left.year,
        left.release_type.as_deref(),
        &right.name,
        right.artist.as_deref(),
        right.year,
        right.release_type.as_deref(),
    )
}

#[allow(dead_code)]
pub fn detail_score(left: &ReleaseDetail, right: &ReleaseDetail) -> f64 {
    identity_score(
        &left.release.title,
        left.release.artist.as_deref(),
        left.release.year,
        left.release.release_type.as_deref(),
        &right.release.title,
        right.release.artist.as_deref(),
        right.release.year,
        right.release.release_type.as_deref(),
    )
}

pub fn summary_score(left: &ReleaseSummary, right: &ReleaseSummary) -> f64 {
    identity_score(
        &left.title,
        left.artist.as_deref(),
        left.year,
        left.release_type.as_deref(),
        &right.title,
        right.artist.as_deref(),
        right.year,
        right.release_type.as_deref(),
    )
}

pub fn external_score(
    source_title: &str,
    source_artist: &str,
    source_year: Option<i64>,
    tracker_title: &str,
    tracker_artist: Option<&str>,
    tracker_year: Option<i64>,
    tracker_type: Option<&str>,
) -> f64 {
    identity_score(
        source_title,
        Some(source_artist),
        source_year,
        None,
        tracker_title,
        tracker_artist,
        tracker_year,
        tracker_type,
    )
}

#[allow(clippy::too_many_arguments)]
fn identity_score(
    left_title: &str,
    left_artist: Option<&str>,
    left_year: Option<i64>,
    left_type: Option<&str>,
    right_title: &str,
    right_artist: Option<&str>,
    right_year: Option<i64>,
    right_type: Option<&str>,
) -> f64 {
    let title = title_similarity(left_title, right_title);
    let artist = match (left_artist, right_artist) {
        (Some(left), Some(right)) => artist_similarity(left, right),
        _ => 0.5,
    };
    let release_type = match (left_type, right_type) {
        (Some(left), Some(right)) if normalized(left) == normalized(right) => 1.0,
        (None, _) | (_, None) => 0.5,
        _ => 0.0,
    };
    let year = match (left_year, right_year) {
        (Some(left), Some(right)) if left == right => 1.0,
        (Some(left), Some(right)) if (left - right).abs() <= 1 => 0.7,
        (None, _) | (_, None) => 0.5,
        _ => 0.0,
    };
    if matches!((left_type, right_type), (Some(left), Some(right)) if normalized(left) != normalized(right))
    {
        return 0.0;
    }
    let generic_artist = [left_artist, right_artist]
        .into_iter()
        .flatten()
        .any(|artist| {
            matches!(
                normalized(artist).as_str(),
                "various artists" | "unknown artist"
            )
        });
    let generic_title = matches!(
        normalized(left_title).as_str(),
        "untitled" | "greatest hits" | "best of" | "the best of"
    );
    if (generic_artist || generic_title) && left_year != right_year {
        return 0.0;
    }
    if matches!((left_year, right_year), (Some(left), Some(right)) if (left - right).abs() > 2) {
        return 0.0;
    }
    title * 0.52 + artist * 0.30 + release_type * 0.10 + year * 0.08
}

pub fn parse_download_release_name(value: &str) -> DownloadReleaseIdentity {
    let value = value.replace(['—', '–'], " - ");
    let value = value.as_str();
    if let Some((artist, title, year)) = parse_scene_release_name(value) {
        return DownloadReleaseIdentity {
            artist,
            title,
            year,
        };
    }
    let structured = value.contains(" - ");
    let display = value
        .chars()
        .map(|character| match character {
            '_' => ' ',
            '.' if !structured => ' ',
            character => character,
        })
        .collect::<String>();
    let display = display.split_whitespace().collect::<Vec<_>>().join(" ");
    let year = release_name_year(&display);
    if let Some((title, artist)) = display.split_once(" ~ ") {
        return DownloadReleaseIdentity {
            artist: strip_download_metadata(artist),
            title: strip_download_metadata(strip_leading_catalog_code(title)),
            year,
        };
    }
    let Some((artist, remainder)) = display.split_once(" - ") else {
        let parts = display.split(" · ").map(str::trim).collect::<Vec<_>>();
        if parts.len() >= 3 && parts[1].parse::<i64>().ok() == year {
            return DownloadReleaseIdentity {
                artist: parts[0].to_owned(),
                title: strip_download_metadata(&parts[2..].join(" · ")),
                year,
            };
        }
        if let Some(year) = year
            && let Some((artist, title)) = split_dated_release_name(&display, year)
        {
            return DownloadReleaseIdentity {
                artist: strip_download_metadata(strip_leading_catalog_code(&artist)),
                title: strip_download_metadata(&title),
                year: Some(year),
            };
        }
        return DownloadReleaseIdentity {
            artist: String::new(),
            title: strip_unstructured_download_metadata(&display, year),
            year,
        };
    };
    let artist = strip_leading_bracket_metadata(artist);
    let artist = strip_leading_catalog_code(&strip_leading_year(artist.trim())).to_owned();
    let remainder = remainder.trim();
    let title = if remainder
        .get(..4)
        .and_then(|candidate| candidate.parse::<i64>().ok())
        .is_some_and(|candidate| (1900..=2100).contains(&candidate))
    {
        remainder
            .get(4..)
            .and_then(|value| value.trim().strip_prefix('-'))
            .map(str::trim)
            .unwrap_or(remainder)
    } else {
        remainder
    };
    DownloadReleaseIdentity {
        artist,
        title: strip_download_metadata(title),
        year,
    }
}

fn parse_scene_release_name(value: &str) -> Option<(String, String, Option<i64>)> {
    let catalog = value.find("-(")?;
    let catalog_end = value[catalog + 2..].find(')')? + catalog + 2;
    let catalog_value = &value[catalog + 2..catalog_end];
    if catalog_value.is_empty()
        || !catalog_value
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return None;
    }
    let (artist, title) = value[..catalog].rsplit_once('-')?;
    let clean = |part: &str| {
        part.replace('_', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    Some((
        clean(artist),
        strip_download_metadata(&clean(title)),
        release_name_year(value),
    ))
}

fn split_dated_release_name(value: &str, year: i64) -> Option<(String, String)> {
    let year = year.to_string();
    let offset = value.find(&year)?;
    let artist = value[..offset]
        .trim()
        .trim_end_matches(['-', '.', ' '])
        .trim();
    if artist.is_empty() {
        return None;
    }
    let mut remainder = value[offset + year.len()..].trim_start();
    let date_end = remainder
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit() || matches!(character, '.' | ' '))
        .map(|(offset, character)| offset + character.len_utf8())
        .last()
        .unwrap_or_default();
    if date_end > 0 && remainder[date_end..].starts_with('-') {
        remainder = remainder[date_end + 1..].trim_start();
    }
    while let Some(next) = remainder.strip_prefix('.') {
        remainder = next.trim_start_matches(|character: char| character.is_ascii_digit());
    }
    remainder = remainder
        .trim_start()
        .strip_prefix('-')
        .unwrap_or(remainder)
        .trim();
    if remainder.split_whitespace().all(is_download_metadata_token) {
        return None;
    }
    let title = strip_download_metadata(remainder);
    (!title.is_empty()).then(|| (artist.to_owned(), title))
}

fn strip_unstructured_download_metadata(value: &str, year: Option<i64>) -> String {
    let mut value = strip_leading_year(value);
    if let Some(year) = year {
        let marker = year.to_string();
        if let Some(offset) = value.find(&marker)
            && value[offset + marker.len()..]
                .split_whitespace()
                .all(is_download_metadata_token)
        {
            value.truncate(offset);
        }
    }
    strip_download_metadata(&value)
}

pub(crate) fn is_download_metadata_token(token: &str) -> bool {
    let token = normalized(token);
    token.is_empty()
        || matches!(
            token.as_str(),
            "web" | "cd" | "cdm" | "flac" | "mp3" | "v0" | "v2" | "320" | "single"
        )
}

fn release_name_year(value: &str) -> Option<i64> {
    value
        .split(|character: char| !character.is_ascii_digit())
        .filter(|candidate| candidate.len() == 4)
        .filter_map(|candidate| candidate.parse::<i64>().ok())
        .find(|year| (1900..=2100).contains(year))
}

fn strip_download_metadata(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let markers = [
        " - 19",
        " - 20",
        " (19",
        " (20",
        " [19",
        " [20",
        " - single",
        " [single",
        " (single",
        " [flac",
        " [cd",
        " [web",
        " (flac",
        " (web",
        " (320",
        " [320",
        " (v0",
        " (v2",
        " [v0",
        " [v2",
        " [16",
        " [24",
        " [",
        " v0",
        " v2",
        " 320",
        " {",
    ];
    let end = markers
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
        .unwrap_or(value.len());
    let value = value[..end].trim().trim_end_matches('-').trim();
    value
        .strip_suffix(" Single")
        .or_else(|| value.strip_suffix(" single"))
        .unwrap_or(value)
        .trim()
        .to_owned()
}

fn strip_leading_year(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with('(')
        && trimmed.get(5..6) == Some(")")
        && trimmed
            .get(1..5)
            .and_then(|year| year.parse::<i64>().ok())
            .is_some_and(|year| (1900..=2100).contains(&year))
    {
        return trimmed[6..].trim().to_owned();
    }
    if trimmed.len() > 5
        && trimmed
            .get(..4)
            .and_then(|year| year.parse::<i64>().ok())
            .is_some_and(|year| (1900..=2100).contains(&year))
        && trimmed
            .as_bytes()
            .get(4)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return trimmed[5..].trim().to_owned();
    }
    trimmed.to_owned()
}

fn strip_leading_bracket_metadata(value: &str) -> &str {
    let mut value = value.trim();
    while let Some(remainder) = value.strip_prefix('[')
        && let Some(end) = remainder.find(']')
    {
        value = remainder[end + 1..].trim_start();
    }
    value
}

pub(crate) fn strip_leading_catalog_code(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.starts_with('[')
        && let Some(end) = trimmed.find("] ")
        && !trimmed[1..end].is_empty()
        && trimmed[1..end].chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return trimmed[end + 2..].trim();
    }
    trimmed
}

pub fn download_match_score(identity: &DownloadReleaseIdentity, release: &ReleaseSummary) -> f64 {
    if identity.title.trim().is_empty() {
        return 0.0;
    }
    let title = title_similarity(&identity.title, &release.title);
    let artist = match release.artist.as_deref() {
        Some(candidate) if !identity.artist.trim().is_empty() => {
            artist_similarity(&identity.artist, candidate)
        }
        Some(_) => 0.5,
        None => 0.5,
    };
    let year = match (identity.year, release.year) {
        (Some(left), Some(right)) if left == right => 1.0,
        (Some(left), Some(right)) if (left - right).abs() == 1 => 0.75,
        (None, _) | (_, None) => 0.5,
        _ => return 0.0,
    };
    if title < 0.80 || (!identity.artist.trim().is_empty() && artist < 0.70) {
        return 0.0;
    }
    title * 0.57 + artist * 0.35 + year * 0.08
}

pub fn match_download_release(
    identity: &DownloadReleaseIdentity,
    releases: &[ReleaseSummary],
) -> DownloadMatchResult {
    let mut ranked = releases
        .iter()
        .filter_map(|release| {
            let release_id = release.id?;
            let score = download_match_score(identity, release);
            (score >= DOWNLOAD_MATCH_THRESHOLD)
                .then_some(DownloadReleaseMatch { release_id, score })
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
    });
    let Some(best) = ranked.first().copied() else {
        return DownloadMatchResult::NotFound;
    };
    if let Some(runner_up) = ranked.get(1).copied()
        && best.release_id != runner_up.release_id
        && best.score - runner_up.score < DOWNLOAD_MATCH_MARGIN
    {
        return DownloadMatchResult::Ambiguous { best, runner_up };
    }
    DownloadMatchResult::Matched(best)
}

pub fn merge_search_group(primary: &mut SearchGroup, mut secondary: SearchGroup, score: f64) {
    primary.sources.push(ReleaseSource {
        tracker: secondary.tracker.clone(),
        group_id: secondary.group_id,
        match_score: score,
    });
    primary.torrents.append(&mut secondary.torrents);
    for tag in secondary.tags {
        if !primary
            .tags
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&tag))
        {
            primary.tags.push(tag);
        }
    }
    if primary.image.as_deref().is_none_or(str::is_empty) {
        primary.image = secondary.image;
    }
    primary.year = match (primary.year, secondary.year) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    };
}

#[allow(dead_code)]
pub fn merge_release_detail(primary: &mut ReleaseDetail, mut secondary: ReleaseDetail, score: f64) {
    primary.release.sources.push(ReleaseSource {
        tracker: secondary.release.tracker.clone(),
        group_id: secondary.release.group_id,
        match_score: score,
    });
    primary.variants.append(&mut secondary.variants);
    for tag in secondary.tags {
        if !primary
            .tags
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&tag))
        {
            primary.tags.push(tag);
        }
    }
    if secondary
        .description
        .as_ref()
        .is_some_and(|value| value.len() > primary.description.as_deref().unwrap_or_default().len())
    {
        primary.description = secondary.description;
    }
    if primary.record_label.as_deref().is_none_or(str::is_empty) {
        primary.record_label = secondary.record_label;
    }
    if primary.release.artwork.as_deref().is_none_or(str::is_empty) {
        primary.release.artwork = secondary.release.artwork;
    }
    for artist in secondary.release.artists {
        if !primary.release.artists.iter().any(|known| {
            known.role == artist.role && normalized(&known.name) == normalized(&artist.name)
        }) {
            primary.release.artists.push(artist);
        }
    }
    primary.release.year = match (primary.release.year, secondary.release.year) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    };
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use uuid::Uuid;

    use crate::model::{ReleaseSummary, SearchGroup};

    use super::{
        AUTO_MERGE_THRESHOLD, DownloadMatchResult, external_score, group_score,
        match_download_release, normalized, parse_download_release_name,
    };

    #[derive(Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        #[serde(rename = "match")]
        expected: bool,
        left_artist: String,
        left_title: String,
        left_year: i64,
        left_type: String,
        right_artist: String,
        right_title: String,
        right_year: i64,
        right_type: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DownloadValidationCorpus {
        catalog: Vec<DownloadCatalogEntry>,
        cases: Vec<DownloadCase>,
    }

    #[derive(Deserialize)]
    struct DownloadCatalogEntry {
        key: String,
        artist: String,
        title: String,
        year: i64,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DownloadCase {
        torrent_name: String,
        expected: Option<String>,
    }

    fn group(
        tracker: &str,
        artist: String,
        title: String,
        year: i64,
        release_type: String,
    ) -> SearchGroup {
        SearchGroup {
            id: None,
            tracker: tracker.into(),
            group_id: 1,
            name: title,
            artist: Some(artist),
            year: Some(year),
            release_type: Some(release_type),
            image: None,
            tags: Vec::new(),
            torrents: Vec::new(),
            sources: Vec::new(),
            album_coverage: None,
        }
    }

    fn harmless_noise(value: &str, seed: usize) -> String {
        match seed % 8 {
            0 => value.to_uppercase(),
            1 => format!(" {value} "),
            2 => value.replace(" and ", " & "),
            3 => value.replace(' ', "  "),
            4 => value.replace(' ', " - "),
            5 => value.replace('\'', "’"),
            6 => value.replace('-', "–"),
            _ => value.to_owned(),
        }
    }

    #[test]
    fn normalizes_cross_tracker_spelling_noise() {
        assert_eq!(normalized("Beyoncé & Jay-Z"), "beyonce and jay z");
    }

    #[test]
    fn album_feed_identity_can_match_without_release_type() {
        let score = external_score(
            "Discovery",
            "Daft Punk",
            Some(2001),
            "Discovery",
            Some("Daft Punk"),
            Some(2001),
            Some("Album"),
        );
        assert!(score >= AUTO_MERGE_THRESHOLD);
        assert!(
            external_score(
                "Discovery",
                "Daft Punk",
                Some(2001),
                "Homework",
                Some("Daft Punk"),
                Some(1997),
                Some("Album"),
            ) < AUTO_MERGE_THRESHOLD
        );
    }

    #[test]
    fn cross_tracker_validation_corpus_meets_accuracy_gate() {
        let cases: Vec<Case> = serde_json::from_str(include_str!(
            "../tests/fixtures/release_match_validation.json"
        ))
        .expect("release match validation fixture");
        let mut true_positive = 0usize;
        let mut false_positive = 0usize;
        let mut true_negative = 0usize;
        let mut false_negative = 0usize;
        for case in &cases {
            for seed in 0..128 {
                let left = group(
                    "ops",
                    harmless_noise(&case.left_artist, seed),
                    harmless_noise(&case.left_title, seed / 2),
                    case.left_year,
                    case.left_type.clone(),
                );
                let right = group(
                    "red",
                    harmless_noise(&case.right_artist, seed / 3),
                    harmless_noise(&case.right_title, seed / 5),
                    case.right_year,
                    case.right_type.clone(),
                );
                let predicted = group_score(&left, &right) >= AUTO_MERGE_THRESHOLD;
                match (case.expected, predicted) {
                    (true, true) => true_positive += 1,
                    (false, true) => false_positive += 1,
                    (false, false) => true_negative += 1,
                    (true, false) => false_negative += 1,
                }
            }
        }
        let precision = true_positive as f64 / (true_positive + false_positive) as f64;
        let recall = true_positive as f64 / (true_positive + false_negative) as f64;
        eprintln!(
            "release matcher: cases={} comparisons={} precision={precision:.4} recall={recall:.4} fp={false_positive} fn={false_negative}",
            cases.len(),
            cases.len() * 128
        );
        assert!(cases.len() * 128 >= 2_000);
        assert!(precision >= 0.995, "precision {precision:.4}");
        assert!(recall >= 0.98, "recall {recall:.4}");
        assert!(true_negative > 0);
    }

    #[test]
    fn offline_download_match_benchmark_meets_quality_gate() {
        let corpus: DownloadValidationCorpus = serde_json::from_str(include_str!(
            "../tests/fixtures/download_match_validation.json"
        ))
        .expect("download match validation fixture");
        let catalog = corpus
            .catalog
            .iter()
            .enumerate()
            .map(|(index, entry)| ReleaseSummary {
                id: Some(Uuid::from_u128(index as u128 + 1)),
                tracker: "fixture".into(),
                group_id: index as i64 + 1,
                title: entry.title.clone(),
                artist: Some(entry.artist.clone()),
                artists: Vec::new(),
                year: Some(entry.year),
                artwork: None,
                release_type: None,
                sources: Vec::new(),
                album_coverage: None,
            })
            .collect::<Vec<_>>();
        let expected_ids = corpus
            .catalog
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.key.as_str(), Uuid::from_u128(index as u128 + 1)))
            .collect::<std::collections::HashMap<_, _>>();
        let mut true_positive = 0usize;
        let mut false_positive = 0usize;
        let mut true_negative = 0usize;
        let mut false_negative = 0usize;
        for case in &corpus.cases {
            let identity = parse_download_release_name(&case.torrent_name);
            let predicted = match match_download_release(&identity, &catalog) {
                DownloadMatchResult::Matched(candidate) => Some(candidate.release_id),
                DownloadMatchResult::Ambiguous { .. } | DownloadMatchResult::NotFound => None,
            };
            let expected = case.expected.as_deref().map(|key| expected_ids[key]);
            match (expected, predicted) {
                (Some(expected), Some(predicted)) if expected == predicted => true_positive += 1,
                (None, None) => true_negative += 1,
                (Some(_), None) => false_negative += 1,
                _ => false_positive += 1,
            }
        }
        let precision = true_positive as f64 / (true_positive + false_positive) as f64;
        let recall = true_positive as f64 / (true_positive + false_negative) as f64;
        eprintln!(
            "download matcher: cases={} comparisons={} precision={precision:.4} recall={recall:.4} fp={false_positive} fn={false_negative}",
            corpus.cases.len(),
            corpus.cases.len() * catalog.len()
        );
        assert!(corpus.cases.len() * catalog.len() >= 1_500);
        assert!(precision >= 0.995, "precision {precision:.4}");
        assert!(recall >= 0.97, "recall {recall:.4}");
        assert!(true_negative >= 5);
    }
}
