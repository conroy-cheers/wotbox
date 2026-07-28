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
            album.variants.iter().any(|variant| {
                preferences.allows(variant.format.as_deref(), variant.encoding.as_deref())
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
                tracks: parse_file_list(file_list),
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
                    let best = index
                        .variants
                        .iter()
                        .flat_map(|variant| &variant.tracks)
                        .filter_map(|candidate| match_fingerprints(track, candidate))
                        .min_by_key(|kind| match kind {
                            MatchKind::Exact => 0,
                            MatchKind::Fuzzy => 1,
                        });
                    if let Some(kind) = best {
                        matches.push(RawAlbumMatch {
                            album: AlbumReference {
                                tracker: group.release.tracker.clone(),
                                group_id: group.release.group_id,
                                title: group.release.title.clone(),
                                year: group.release.year,
                            },
                            variants: group.variants.clone(),
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

pub fn parse_file_list(file_list: &str) -> Vec<TitleFingerprint> {
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
                .then(|| fingerprint(path))
        })
        .collect()
}

fn fingerprint(path: &str) -> TitleFingerprint {
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
    let candidates = values.into_iter().map(candidate).collect();
    TitleFingerprint {
        display,
        candidates,
    }
}

fn normalize_title(value: &str) -> String {
    let folded = value
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
    words.join(" ")
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
        .filter(|word| word.chars().any(|character| character.is_ascii_digit()))
        .map(|word| (*word).to_owned())
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
    left.alphanumeric_len.abs_diff(right.alphanumeric_len) <= limit
        && damerau_levenshtein(&left.value, &right.value) <= limit
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
                tracker: "ops".into(),
                group_id,
                title: title.into(),
                artist: Some("Artist".into()),
                artists: vec![ArtistCredit {
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
                can_use_token: false,
                token_eligibility_known: true,
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
            minimum_quality: "320".into(),
            ..ReleasePreferences::default()
        };
        let resolved = coverage.resolve(&preferences).expect("320 is now eligible");
        assert_eq!(resolved.confidence, CoverageConfidence::Fuzzy);
    }
}
