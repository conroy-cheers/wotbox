use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::model::{
    AlbumCoverage, AlbumReference, ArtistCatalogRelease, ArtistCredit, CoverageConfidence,
    ReleaseDetail, ReleasePreferences, TorrentVariant,
};

const AUDIO_EXTENSIONS: &[&str] = &[
    "aac", "aif", "aiff", "alac", "flac", "m4a", "mp3", "ogg", "opus", "wav", "wv",
];
const PROTECTED_QUALIFIERS: &[&str] = &[
    "acoustic",
    "demo",
    "edit",
    "instrumental",
    "live",
    "mix",
    "mono",
    "radio",
    "remaster",
    "remastered",
    "remix",
    "stereo",
    "version",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleFingerprint {
    pub display: String,
    pub candidates: Vec<TitleCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleCandidate {
    pub value: String,
    pub alphanumeric_len: usize,
    pub word_count: usize,
    pub numbers: Vec<String>,
    pub qualifiers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantTrackIndex {
    pub torrent_id: i64,
    pub tracks: Vec<TitleFingerprint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseTrackIndex {
    pub tracker: String,
    pub group_id: i64,
    pub title: String,
    pub release_type: Option<String>,
    pub artists: Vec<ArtistCredit>,
    pub variants: Vec<VariantTrackIndex>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogMembership {
    pub artist_id: i64,
    pub group: ArtistCatalogRelease,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Exact,
    Fuzzy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawAlbumMatch {
    pub album: AlbumReference,
    pub variants: Vec<TorrentVariant>,
    #[serde(default)]
    pub matched_torrent_ids: Vec<i64>,
    pub kind: MatchKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawTrackCoverage {
    pub track: String,
    pub matches: Vec<RawAlbumMatch>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawSingleCoverage {
    pub tracks: Vec<RawTrackCoverage>,
}

impl RawSingleCoverage {
    pub fn resolve(&self, preferences: &ReleasePreferences) -> Option<AlbumCoverage> {
        if self.tracks.is_empty() {
            return None;
        }
        let eligible = |album: &RawAlbumMatch| {
            album
                .variants
                .iter()
                .filter(|variant| {
                    album.matched_torrent_ids.is_empty()
                        || album.matched_torrent_ids.contains(&variant.torrent_id)
                })
                .any(|variant| {
                    preferences
                        .allows_quality(variant.format.as_deref(), variant.encoding.as_deref())
                        && preferences.allows_media(variant.media.as_deref())
                })
        };
        if self
            .tracks
            .iter()
            .any(|track| !track.matches.iter().any(eligible))
        {
            return None;
        }

        let confidence = if self.tracks.iter().all(|track| {
            track
                .matches
                .iter()
                .any(|album| eligible(album) && album.kind == MatchKind::Exact)
        }) {
            CoverageConfidence::Exact
        } else {
            CoverageConfidence::Fuzzy
        };
        let mut albums = HashMap::new();
        for matched in self
            .tracks
            .iter()
            .flat_map(|track| track.matches.iter())
            .filter(|album| eligible(album))
        {
            albums
                .entry(matched.album.group_id)
                .or_insert_with(|| matched.album.clone());
        }
        let mut albums = albums.into_values().collect::<Vec<_>>();
        albums.sort_by(|left, right| {
            right
                .year
                .unwrap_or_default()
                .cmp(&left.year.unwrap_or_default())
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
        });
        Some(AlbumCoverage { albums, confidence })
    }
}

pub fn track_index_from_group(
    tracker: &str,
    detail: &ReleaseDetail,
    raw: &Value,
) -> ReleaseTrackIndex {
    let response = raw.get("response").unwrap_or(raw);
    let artist_names = detail
        .release
        .artists
        .iter()
        .filter(|artist| artist.role == crate::model::ArtistRole::Primary)
        .map(|artist| artist.name.as_str())
        .collect::<Vec<_>>();
    let torrents = response
        .get("torrents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|torrent| {
            let torrent_id = torrent
                .get("id")
                .or_else(|| torrent.get("torrentId"))
                .and_then(Value::as_i64)?;
            let file_list = torrent.get("fileList").and_then(Value::as_str)?;
            Some(VariantTrackIndex {
                torrent_id,
                tracks: parse_file_list_for_artists(file_list, &artist_names),
            })
        })
        .collect();
    ReleaseTrackIndex {
        tracker: tracker.to_owned(),
        group_id: detail.release.group_id,
        title: detail.release.title.clone(),
        release_type: detail.release.release_type.clone(),
        artists: detail.release.artists.clone(),
        variants: torrents,
    }
}

pub fn compute_raw_coverage(
    single: &ReleaseTrackIndex,
    albums: &[(ReleaseTrackIndex, ArtistCatalogRelease)],
) -> RawSingleCoverage {
    let mut unique_tracks: Vec<&TitleFingerprint> = Vec::new();
    for track in single.variants.iter().flat_map(|variant| &variant.tracks) {
        if !unique_tracks
            .iter()
            .any(|known| match_fingerprints(known, track).is_some())
        {
            unique_tracks.push(track);
        }
    }
    RawSingleCoverage {
        tracks: unique_tracks
            .into_iter()
            .map(|track| {
                let mut matches = Vec::new();
                for (index, group) in albums {
                    if !recording_versions_compatible(&single.title, &index.title) {
                        continue;
                    }
                    let matching_variants = index
                        .variants
                        .iter()
                        .filter_map(|variant| {
                            variant
                                .tracks
                                .iter()
                                .filter_map(|candidate| match_fingerprints(track, candidate))
                                .min_by_key(match_priority)
                                .map(|kind| (variant.torrent_id, kind))
                        })
                        .collect::<Vec<_>>();
                    let best = matching_variants
                        .iter()
                        .map(|(_, kind)| *kind)
                        .min_by_key(match_priority);
                    if let Some(kind) = best {
                        let matched_torrent_ids = matching_variants
                            .iter()
                            .map(|(torrent_id, _)| *torrent_id)
                            .collect::<Vec<_>>();
                        matches.push(RawAlbumMatch {
                            album: AlbumReference {
                                tracker: group.release.tracker.clone(),
                                group_id: group.release.group_id,
                                title: group.release.title.clone(),
                                year: group.release.year,
                            },
                            variants: group.variants.clone(),
                            matched_torrent_ids,
                            kind,
                        });
                    }
                }
                RawTrackCoverage {
                    track: track.display.clone(),
                    matches,
                }
            })
            .collect(),
    }
}

#[cfg(test)]
pub fn parse_file_list(file_list: &str) -> Vec<TitleFingerprint> {
    parse_file_list_for_artists(file_list, &[])
}

fn parse_file_list_for_artists(file_list: &str, artist_names: &[&str]) -> Vec<TitleFingerprint> {
    file_list
        .split("|||")
        .filter_map(|entry| {
            let path = entry.split("{{{").next()?.trim();
            let extension = Path::new(path)
                .extension()
                .and_then(|value| value.to_str())?
                .to_ascii_lowercase();
            AUDIO_EXTENSIONS
                .contains(&extension.as_str())
                .then(|| fingerprint_for_artists(path, artist_names))
        })
        .collect()
}

#[cfg(test)]
fn fingerprint(path: &str) -> TitleFingerprint {
    fingerprint_for_artists(path, &[])
}

fn fingerprint_for_artists(path: &str, artist_names: &[&str]) -> TitleFingerprint {
    let display = Path::new(path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(path)
        .to_owned();
    let prepared = display.replace('_', " ").replace(['–', '—'], "-");
    let parts = prepared.split(" - ").collect::<Vec<_>>();
    let mut values = BTreeSet::new();
    for start in 0..parts.len() {
        if start > 0
            && !parts[..=start]
                .iter()
                .any(|part| part.chars().any(|character| character.is_ascii_digit()))
        {
            continue;
        }
        let joined = parts[start..].join(" - ");
        let normalized = normalize_title(&joined);
        if !normalized.is_empty() {
            values.insert(normalized.clone());
            if let Some(without_credit) = strip_featured_credit(&normalized) {
                values.insert(without_credit);
            }
        }
    }
    let artist_prefixes = artist_names
        .iter()
        .map(|artist| normalize_title(artist))
        .filter(|artist| !artist.is_empty())
        .collect::<Vec<_>>();
    for value in values.clone() {
        for artist in &artist_prefixes {
            if let Some(title) = value.strip_prefix(&format!("{artist} "))
                && !title.is_empty()
            {
                values.insert(title.to_owned());
            }
        }
    }
    let candidates = values.into_iter().map(candidate).collect();
    TitleFingerprint {
        display,
        candidates,
    }
}

fn normalize_title(value: &str) -> String {
    let prepared = value.replace(['\'', '’'], "").replace(['&', '+'], " and ");
    let folded = prepared
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
        .collect::<String>();
    let mut words = folded.split_whitespace().collect::<Vec<_>>();
    while words.first().is_some_and(|word| {
        word.chars().all(|character| character.is_ascii_digit())
            || word
                .strip_prefix("disc")
                .or_else(|| word.strip_prefix("cd"))
                .is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
                })
    }) {
        words.remove(0);
    }
    words
        .into_iter()
        .map(|word| match word {
            "remastered" => "remaster",
            _ => word,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_featured_credit(value: &str) -> Option<String> {
    let words = value.split_whitespace().collect::<Vec<_>>();
    let index = words
        .iter()
        .position(|word| matches!(*word, "feat" | "featuring" | "ft"))?;
    (index > 0).then(|| words[..index].join(" "))
}

fn candidate(value: String) -> TitleCandidate {
    let words = value.split_whitespace().collect::<Vec<_>>();
    let numbers = words
        .iter()
        .enumerate()
        .filter(|(index, word)| {
            word.chars().any(|character| character.is_ascii_digit())
                || (is_roman_numeral(word)
                    && index.checked_sub(1).is_some_and(|previous| {
                        matches!(
                            words[previous],
                            "act"
                                | "book"
                                | "chapter"
                                | "disc"
                                | "movement"
                                | "part"
                                | "vol"
                                | "volume"
                        )
                    }))
        })
        .map(|(_, word)| (*word).to_owned())
        .collect();
    let qualifiers = words
        .iter()
        .filter(|word| PROTECTED_QUALIFIERS.contains(word))
        .map(|word| (*word).to_owned())
        .collect();
    TitleCandidate {
        alphanumeric_len: value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .count(),
        word_count: words.len(),
        numbers,
        qualifiers,
        value,
    }
}

fn is_roman_numeral(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8
        && value
            .chars()
            .all(|character| matches!(character, 'i' | 'v' | 'x' | 'l' | 'c'))
}

fn match_priority(kind: &MatchKind) -> u8 {
    match kind {
        MatchKind::Exact => 0,
        MatchKind::Fuzzy => 1,
    }
}

fn recording_versions_compatible(single_title: &str, album_title: &str) -> bool {
    recording_version_markers(single_title) == recording_version_markers(album_title)
}

fn recording_version_markers(title: &str) -> BTreeSet<&'static str> {
    let normalized = normalize_title(title);
    [
        ("taylors version", "taylors_version"),
        ("re recorded", "re_recorded"),
        ("rerecorded", "re_recorded"),
        ("new recording", "new_recording"),
    ]
    .into_iter()
    .filter_map(|(phrase, marker)| normalized.contains(phrase).then_some(marker))
    .collect()
}

fn match_fingerprints(left: &TitleFingerprint, right: &TitleFingerprint) -> Option<MatchKind> {
    if left.candidates.iter().any(|left| {
        right
            .candidates
            .iter()
            .any(|right| left.value == right.value)
    }) {
        return Some(MatchKind::Exact);
    }
    left.candidates
        .iter()
        .any(|left| {
            right
                .candidates
                .iter()
                .any(|right| fuzzy_match(left, right))
        })
        .then_some(MatchKind::Fuzzy)
}

fn fuzzy_match(left: &TitleCandidate, right: &TitleCandidate) -> bool {
    if left.word_count != right.word_count
        || left.numbers != right.numbers
        || left.qualifiers != right.qualifiers
    {
        return false;
    }
    let shortest = left.alphanumeric_len.min(right.alphanumeric_len);
    let limit = match shortest {
        0..=7 => return false,
        8..=19 => 1,
        _ => 2,
    };
    if semantic_affix_difference(&left.value, &right.value) {
        return false;
    }
    if left.alphanumeric_len == right.alphanumeric_len
        && left
            .value
            .chars()
            .zip(right.value.chars())
            .filter(|(left, right)| left != right)
            .count()
            == 1
    {
        return false;
    }
    left.alphanumeric_len.abs_diff(right.alphanumeric_len) <= limit
        && damerau_levenshtein(&left.value, &right.value) <= limit
}

fn semantic_affix_difference(left: &str, right: &str) -> bool {
    let one_is_plural_of_other = |left: &str, right: &str| {
        left.strip_suffix('s')
            .is_some_and(|singular| singular == right)
            || right
                .strip_suffix('s')
                .is_some_and(|singular| singular == left)
    };
    if one_is_plural_of_other(left, right) {
        return true;
    }
    const SEMANTIC_PREFIXES: &[&str] = &["anti", "dis", "im", "in", "non", "re", "un"];
    left.split_whitespace()
        .zip(right.split_whitespace())
        .any(|(left, right)| {
            SEMANTIC_PREFIXES.iter().any(|prefix| {
                left.strip_prefix(prefix)
                    .is_some_and(|value| value == right)
                    || right
                        .strip_prefix(prefix)
                        .is_some_and(|value| value == left)
            })
        })
}

fn damerau_levenshtein(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let mut matrix = vec![vec![0; right.len() + 1]; left.len() + 1];
    for (index, row) in matrix.iter_mut().enumerate() {
        row[0] = index;
    }
    for (index, cell) in matrix[0].iter_mut().enumerate() {
        *cell = index;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            let cost = usize::from(left[i - 1] != right[j - 1]);
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                matrix[i][j] = matrix[i][j].min(matrix[i - 2][j - 2] + cost);
            }
        }
    }
    matrix[left.len()][right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ArtistCatalogRole, ArtistCreditSource, ArtistRole, LibraryVariantState, ReleaseSummary,
    };
    use serde::Deserialize;
    use std::collections::BTreeMap;

    fn one(path: &str) -> TitleFingerprint {
        fingerprint(path)
    }

    fn index(group_id: i64, kind: &str, paths: &[&str]) -> ReleaseTrackIndex {
        ReleaseTrackIndex {
            tracker: "ops".into(),
            group_id,
            title: format!("Release {group_id}"),
            release_type: Some(kind.into()),
            artists: Vec::new(),
            variants: vec![VariantTrackIndex {
                torrent_id: group_id * 10,
                tracks: paths.iter().map(|path| one(path)).collect(),
            }],
        }
    }

    fn album(group_id: i64, title: &str, encoding: &str) -> ArtistCatalogRelease {
        ArtistCatalogRelease {
            release: ReleaseSummary {
                id: None,
                tracker: "ops".into(),
                group_id,
                title: title.into(),
                artist: Some("Artist".into()),
                artists: vec![ArtistCredit {
                    canonical_id: None,
                    key: "id:1".into(),
                    tracker: "ops".into(),
                    artist_id: Some(1),
                    name: "Artist".into(),
                    role: ArtistRole::Primary,
                    source: ArtistCreditSource::Structured,
                }],
                year: Some(2020),
                artwork: None,
                release_type: Some("Album".into()),
                sources: vec![crate::model::ReleaseSource {
                    tracker: "ops".into(),
                    group_id,
                    match_score: 1.0,
                }],
                album_coverage: None,
            },
            tags: Vec::new(),
            variants: vec![TorrentVariant {
                tracker: "ops".into(),
                torrent_id: group_id * 10,
                group_id,
                info_hash: None,
                format: Some(
                    if encoding == "Lossless" {
                        "FLAC"
                    } else {
                        "MP3"
                    }
                    .into(),
                ),
                encoding: Some(encoding.into()),
                media: Some("WEB".into()),
                size: None,
                seeders: Some(1),
                leechers: Some(0),
                snatched: None,
                freeleech: false,
                leech_status: crate::model::LeechStatus::Regular,
                can_use_token: false,
                token_eligibility_known: true,
                eligibility: None,
                remaster_title: None,
                downloads: Vec::new(),
                library: None::<LibraryVariantState>,
            }],
            roles: vec![ArtistCatalogRole::Primary],
            listed_on_tracker: true,
            library_availability: None,
            library_added_at: None,
        }
    }

    #[test]
    fn parses_audio_files_and_ignores_extras() {
        let tracks =
            parse_file_list("01 - A Song.flac{{{123}}}|||cover.jpg{{{5}}}|||album.log{{{3}}}");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].display, "01 - A Song");
    }

    #[test]
    fn scene_and_plain_names_match_exactly() {
        assert_eq!(
            match_fingerprints(
                &one("01-Artist_-_Album_-_A_Song.flac"),
                &one("01. A Song.flac")
            ),
            Some(MatchKind::Exact)
        );
    }

    #[test]
    fn permits_only_bounded_typos() {
        assert_eq!(
            match_fingerprints(
                &one("Everything Matters.flac"),
                &one("Everythnig Matters.flac")
            ),
            Some(MatchKind::Fuzzy)
        );
        assert_eq!(
            match_fingerprints(
                &one("Everything Matters.flac"),
                &one("Anything Matters.flac")
            ),
            None
        );
    }

    #[test]
    fn protects_numbers_versions_and_short_titles() {
        assert_eq!(
            match_fingerprints(&one("Song Part 1.flac"), &one("Song Part 2.flac")),
            None
        );
        assert_eq!(
            match_fingerprints(&one("A Song (Live).flac"), &one("A Song.flac")),
            None
        );
        assert_eq!(
            match_fingerprints(&one("Hallo.flac"), &one("Hello.flac")),
            None
        );
        assert_eq!(
            match_fingerprints(
                &one("Different Song - Live.flac"),
                &one("Another Song - Live.flac")
            ),
            None
        );
    }

    #[test]
    fn requires_every_single_track_but_allows_coverage_across_albums() {
        let single = index(1, "Single", &["01 - Main Song.flac", "02 - B Side.flac"]);
        let first = (
            index(2, "Album", &["Main Song.flac"]),
            album(2, "First", "Lossless"),
        );
        let second = (
            index(3, "Album", &["B Side.flac"]),
            album(3, "Second", "Lossless"),
        );
        let coverage = compute_raw_coverage(&single, &[first.clone(), second]);
        let resolved = coverage
            .resolve(&ReleasePreferences::default())
            .expect("all tracks covered");
        assert_eq!(resolved.albums.len(), 2);

        let incomplete = compute_raw_coverage(&single, &[first]);
        assert!(incomplete.resolve(&ReleasePreferences::default()).is_none());
    }

    #[test]
    fn respects_quality_cutoff_and_reports_fuzzy_confidence() {
        let single = index(1, "Single", &["Everything Matters.flac"]);
        let candidate = (
            index(2, "Album", &["Everythnig Matters.flac"]),
            album(2, "Album", "320"),
        );
        let coverage = compute_raw_coverage(&single, &[candidate]);
        assert!(coverage.resolve(&ReleasePreferences::default()).is_none());

        let preferences = ReleasePreferences {
            quality_cutoff_index: 3,
            ..ReleasePreferences::default()
        };
        let resolved = coverage.resolve(&preferences).expect("320 is now eligible");
        assert_eq!(resolved.confidence, CoverageConfidence::Fuzzy);
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ValidationCorpus {
        schema_version: u32,
        title_families: Vec<TitleFamily>,
        coverage_cases: Vec<CoverageCase>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TitleFamily {
        id: String,
        category: String,
        canonical: String,
        #[serde(default)]
        artist: Option<String>,
        #[serde(default)]
        exact: Vec<String>,
        #[serde(default)]
        fuzzy: Vec<String>,
        #[serde(default)]
        distinct: Vec<String>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CoverageCase {
        id: String,
        category: String,
        #[serde(default)]
        minimum_quality: Option<String>,
        single: ValidationRelease,
        albums: Vec<ValidationRelease>,
        expected_covered: bool,
        expected_confidence: Option<String>,
        expected_album_ids: Vec<i64>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ValidationRelease {
        id: i64,
        title: String,
        artist: String,
        variants: Vec<ValidationVariant>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ValidationVariant {
        torrent_id: i64,
        #[serde(default = "default_format")]
        format: String,
        #[serde(default = "default_encoding")]
        encoding: String,
        tracks: Vec<String>,
    }

    fn default_format() -> String {
        "FLAC".into()
    }

    fn default_encoding() -> String {
        "Lossless".into()
    }

    fn validation_corpus() -> ValidationCorpus {
        serde_json::from_str(include_str!("../tests/fixtures/dedupe_validation.json"))
            .expect("validation corpus must be valid")
    }

    fn validation_index(release: &ValidationRelease, kind: &str) -> ReleaseTrackIndex {
        let artist = ArtistCredit {
            canonical_id: None,
            key: format!("name:{}", release.artist.to_lowercase()),
            tracker: "ops".into(),
            artist_id: Some(1),
            name: release.artist.clone(),
            role: ArtistRole::Primary,
            source: ArtistCreditSource::Structured,
        };
        ReleaseTrackIndex {
            tracker: "ops".into(),
            group_id: release.id,
            title: release.title.clone(),
            release_type: Some(kind.into()),
            artists: vec![artist],
            variants: release
                .variants
                .iter()
                .map(|variant| VariantTrackIndex {
                    torrent_id: variant.torrent_id,
                    tracks: variant
                        .tracks
                        .iter()
                        .flat_map(|path| {
                            parse_file_list_for_artists(
                                &format!("{path}{{{{{{1}}}}}}"),
                                &[release.artist.as_str()],
                            )
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn validation_album(release: &ValidationRelease) -> ArtistCatalogRelease {
        let mut value = album(release.id, &release.title, "Lossless");
        value.release.title = release.title.clone();
        value.release.artist = Some(release.artist.clone());
        value.variants = release
            .variants
            .iter()
            .map(|variant| TorrentVariant {
                tracker: "ops".into(),
                torrent_id: variant.torrent_id,
                group_id: release.id,
                info_hash: None,
                format: Some(variant.format.clone()),
                encoding: Some(variant.encoding.clone()),
                media: Some("WEB".into()),
                size: None,
                seeders: Some(1),
                leechers: Some(0),
                snatched: None,
                freeleech: false,
                leech_status: crate::model::LeechStatus::Regular,
                can_use_token: false,
                token_eligibility_known: true,
                eligibility: None,
                remaster_title: None,
                downloads: Vec::new(),
                library: None,
            })
            .collect();
        value
    }

    #[test]
    fn dedupe_validation_corpus_meets_accuracy_bars() {
        let corpus = validation_corpus();
        assert_eq!(corpus.schema_version, 1);

        let mut true_positive = 0usize;
        let mut false_positive = 0usize;
        let mut true_negative = 0usize;
        let mut false_negative = 0usize;
        let mut kind_correct = 0usize;
        let mut positive_total = 0usize;
        let mut category_totals = BTreeMap::<String, usize>::new();
        let mut failures = Vec::new();

        for family in &corpus.title_families {
            let artist_names = family.artist.iter().map(String::as_str).collect::<Vec<_>>();
            let canonical = fingerprint_for_artists(&family.canonical, &artist_names);
            for (expected, examples) in [
                (Some(MatchKind::Exact), &family.exact),
                (Some(MatchKind::Fuzzy), &family.fuzzy),
                (None, &family.distinct),
            ] {
                for example in examples {
                    *category_totals.entry(family.category.clone()).or_default() += 1;
                    let actual = match_fingerprints(
                        &canonical,
                        &fingerprint_for_artists(example, &artist_names),
                    );
                    match (expected, actual) {
                        (Some(expected_kind), Some(actual_kind)) => {
                            true_positive += 1;
                            positive_total += 1;
                            if expected_kind == actual_kind {
                                kind_correct += 1;
                            } else {
                                failures.push(format!(
                                    "{}: expected {expected_kind:?}, got {actual_kind:?} for {:?} vs {:?}",
                                    family.id, family.canonical, example
                                ));
                            }
                        }
                        (None, None) => true_negative += 1,
                        (None, Some(actual_kind)) => {
                            false_positive += 1;
                            failures.push(format!(
                                "{}: expected no match, got {actual_kind:?} for {:?} vs {:?}",
                                family.id, family.canonical, example
                            ));
                        }
                        (Some(expected_kind), None) => {
                            false_negative += 1;
                            positive_total += 1;
                            failures.push(format!(
                                "{}: expected {expected_kind:?}, got no match for {:?} vs {:?}",
                                family.id, family.canonical, example
                            ));
                        }
                    }
                }
            }
        }

        let total = true_positive + false_positive + true_negative + false_negative;
        assert!(
            total >= 200,
            "validation corpus is too small to be meaningful: {total}"
        );
        let precision = true_positive as f64 / (true_positive + false_positive) as f64;
        let recall = true_positive as f64 / (true_positive + false_negative) as f64;
        let specificity = true_negative as f64 / (true_negative + false_positive) as f64;
        let kind_accuracy = kind_correct as f64 / positive_total as f64;
        eprintln!(
            "dedupe title validation: cases={total}, precision={precision:.3}, recall={recall:.3}, specificity={specificity:.3}, kind_accuracy={kind_accuracy:.3}, categories={category_totals:?}"
        );
        assert!(
            precision >= 0.995,
            "precision {precision:.3} is below 0.995\n{}",
            failures.join("\n")
        );
        assert!(
            recall >= 0.980,
            "recall {recall:.3} is below 0.980\n{}",
            failures.join("\n")
        );
        assert!(
            specificity >= 0.995,
            "specificity {specificity:.3} is below 0.995\n{}",
            failures.join("\n")
        );
        assert!(
            kind_accuracy >= 0.970,
            "match-kind accuracy {kind_accuracy:.3} is below 0.970\n{}",
            failures.join("\n")
        );

        let mut coverage_failures = Vec::new();
        let mut coverage_categories = BTreeMap::<String, usize>::new();
        for case in &corpus.coverage_cases {
            *coverage_categories
                .entry(case.category.clone())
                .or_default() += 1;
            let single = validation_index(&case.single, "Single");
            let albums = case
                .albums
                .iter()
                .map(|release| {
                    (
                        validation_index(release, "Album"),
                        validation_album(release),
                    )
                })
                .collect::<Vec<_>>();
            let coverage = compute_raw_coverage(&single, &albums);
            let mut preferences = ReleasePreferences::default();
            if let Some(minimum) = &case.minimum_quality {
                preferences.quality_cutoff_index = preferences
                    .quality_tiers
                    .iter()
                    .position(|tier| tier.iter().any(|value| value == minimum))
                    .map(|index| index + 1)
                    .unwrap_or(preferences.quality_cutoff_index);
            }
            let resolved = coverage.resolve(&preferences);
            if resolved.is_some() != case.expected_covered {
                coverage_failures.push(format!(
                    "{}: expected covered={}, got covered={}",
                    case.id,
                    case.expected_covered,
                    resolved.is_some()
                ));
                continue;
            }
            if let Some(resolved) = resolved {
                let actual_confidence = match resolved.confidence {
                    CoverageConfidence::Exact => "exact",
                    CoverageConfidence::Fuzzy => "fuzzy",
                };
                if case.expected_confidence.as_deref() != Some(actual_confidence) {
                    coverage_failures.push(format!(
                        "{}: expected confidence {:?}, got {actual_confidence}",
                        case.id, case.expected_confidence
                    ));
                }
                let mut actual_ids = resolved
                    .albums
                    .iter()
                    .map(|album| album.group_id)
                    .collect::<Vec<_>>();
                actual_ids.sort_unstable();
                let mut expected_ids = case.expected_album_ids.clone();
                expected_ids.sort_unstable();
                if actual_ids != expected_ids {
                    coverage_failures.push(format!(
                        "{}: expected album ids {expected_ids:?}, got {actual_ids:?}",
                        case.id
                    ));
                }
            }
        }
        eprintln!(
            "dedupe coverage validation: cases={}, categories={coverage_categories:?}",
            corpus.coverage_cases.len()
        );
        assert!(
            corpus.coverage_cases.len() >= 20,
            "coverage corpus is too small"
        );
        assert!(
            coverage_failures.is_empty(),
            "coverage validation failures:\n{}",
            coverage_failures.join("\n")
        );
    }
}
