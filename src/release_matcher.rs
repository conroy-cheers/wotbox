use std::collections::HashSet;

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::model::{ReleaseDetail, ReleaseSource, ReleaseSummary, SearchGroup};

pub const MATCHER_VERSION: i32 = 1;
pub const AUTO_MERGE_THRESHOLD: f64 = 0.88;

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
        (Some(left), Some(right)) => token_similarity(left, right),
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

    use crate::model::SearchGroup;

    use super::{AUTO_MERGE_THRESHOLD, group_score, normalized};

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
}
