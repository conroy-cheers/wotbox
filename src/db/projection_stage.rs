use super::*;
use sea_orm::TryIntoModel;

const STAGE_TARGETS: &[(&str, &str, &[&str])] = &[
    (
        "release",
        "library_release_projections",
        &[
            "release_id",
            "normalized_title",
            "search_text",
            "tracker_keys",
            "format_keys",
            "encoding_keys",
            "media_keys",
            "sort_year",
            "release_type",
            "availability",
            "covered_single",
            "unresolved_credits",
            "document_json",
            "updated_at",
        ],
    ),
    (
        "artist",
        "library_artist_projections",
        &[
            "artist_key",
            "canonical_id",
            "normalized_name",
            "document_json",
            "updated_at",
        ],
    ),
    (
        "membership",
        "library_artist_release_projections",
        &["artist_key", "release_id"],
    ),
];

impl Database {
    pub(super) fn library_release_row(
        release: &LibraryRelease,
        release_id: Uuid,
        now: &str,
    ) -> Result<library_release_projection::ActiveModel> {
        Ok(library_release_projection::ActiveModel {
            release_id: Set(release_id.to_string()),
            normalized_title: Set(release.release.title.trim().to_lowercase()),
            search_text: Set(projection_search_text(release)),
            tracker_keys: Set(projection_facet(
                std::iter::once(release.release.tracker.as_str())
                    .chain(
                        release
                            .release
                            .sources
                            .iter()
                            .map(|source| source.tracker.as_str()),
                    )
                    .chain(
                        release
                            .variants
                            .iter()
                            .map(|variant| variant.tracker.as_str()),
                    ),
            )),
            format_keys: Set(projection_facet(
                release
                    .variants
                    .iter()
                    .filter_map(|variant| variant.format.as_deref()),
            )),
            encoding_keys: Set(projection_facet(
                release
                    .variants
                    .iter()
                    .filter_map(|variant| variant.encoding.as_deref()),
            )),
            media_keys: Set(projection_facet(
                release
                    .variants
                    .iter()
                    .filter_map(|variant| variant.media.as_deref()),
            )),
            sort_year: Set(release.release.year),
            release_type: Set(release
                .release
                .release_type
                .as_deref()
                .map(str::to_lowercase)),
            availability: Set(match release.availability {
                LibraryAvailability::Present => "present",
                LibraryAvailability::Partial => "partial",
                LibraryAvailability::Missing => "missing",
            }
            .to_owned()),
            covered_single: Set(release.release.album_coverage.is_some()),
            unresolved_credits: Set(release
                .release
                .artists
                .iter()
                .all(|artist| artist.source == ArtistCreditSource::DisplayFallback)),
            document_json: Set(serde_json::to_value(release)?),
            updated_at: Set(now.to_owned()),
        })
    }

    pub(super) fn library_artist_row(
        artist: &LibraryArtistSummary,
        now: &str,
    ) -> Result<library_artist_projection::ActiveModel> {
        let artist_key = artist
            .id
            .map(|id| id.to_string())
            .unwrap_or_else(|| format!("{}:{}", artist.tracker.to_ascii_lowercase(), artist.key));
        Ok(library_artist_projection::ActiveModel {
            artist_key: Set(artist_key),
            canonical_id: Set(artist.id.map(|id| id.to_string())),
            normalized_name: Set(artist_sort_name_for_projection(&artist.name)),
            document_json: Set(serde_json::to_value(artist)?),
            updated_at: Set(now.to_owned()),
        })
    }

    pub(super) async fn stage_library_projection(
        &self,
        releases: &[LibraryRelease],
        artists: &[LibraryArtistSummary],
        relations: &[(String, Uuid)],
        now: &str,
    ) -> Result<String> {
        let generation = format!("{}:{}", Utc::now().timestamp(), Uuid::new_v4());
        for batch in releases.chunks(200) {
            let rows = batch
                .iter()
                .filter_map(|release| release.release.id.map(|id| (release, id)))
                .map(|(release, id)| {
                    Ok(Self::library_release_row(release, id, now)?.try_into_model()?)
                })
                .collect::<Result<Vec<_>>>()?;
            self.stage_library_rows(&generation, "release", &rows)
                .await?;
        }
        for batch in artists.chunks(200) {
            let rows = batch
                .iter()
                .map(|artist| Ok(Self::library_artist_row(artist, now)?.try_into_model()?))
                .collect::<Result<Vec<_>>>()?;
            self.stage_library_rows(&generation, "artist", &rows)
                .await?;
        }
        for batch in relations.chunks(300) {
            let rows = batch
                .iter()
                .map(
                    |(artist_key, release_id)| library_artist_release_projection::Model {
                        artist_key: artist_key.clone(),
                        release_id: release_id.to_string(),
                    },
                )
                .collect::<Vec<_>>();
            self.stage_library_rows(&generation, "membership", &rows)
                .await?;
        }
        Ok(generation)
    }

    async fn stage_library_rows(
        &self,
        generation: &str,
        kind: &str,
        rows: &[impl Serialize],
    ) -> Result<()> {
        let transaction = self.begin_write().await?;
        transaction
            .execute_raw(Statement::from_sql_and_values(
                transaction.get_database_backend(),
                "INSERT INTO library_projection_stage(generation, kind, document_json)
             SELECT ?, ?, value FROM json_each(?)",
                [
                    generation.into(),
                    kind.into(),
                    serde_json::to_string(rows)?.into(),
                ],
            ))
            .await?;
        transaction.commit().await?;
        tokio::task::yield_now().await;
        Ok(())
    }

    pub(super) async fn activate_library_projection_stage(
        transaction: &DatabaseTransaction,
        generation: &str,
    ) -> Result<()> {
        for (kind, table, columns) in STAGE_TARGETS {
            let values = columns
                .iter()
                .map(|column| format!("json_extract(document_json, '$.{column}')"))
                .collect::<Vec<_>>()
                .join(", ");
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    transaction.get_database_backend(),
                    format!(
                        "INSERT INTO {table} ({}) SELECT {values}
                         FROM library_projection_stage WHERE generation = ? AND kind = ?",
                        columns.join(", ")
                    ),
                    [generation.into(), (*kind).into()],
                ))
                .await?;
        }
        Ok(())
    }

    pub async fn prune_library_projection_stage(&self, generation: Option<&str>) -> Result<()> {
        let cutoff = (Utc::now() - chrono::Duration::hours(1))
            .timestamp()
            .to_string();
        loop {
            let transaction = self.begin_write().await?;
            let result = transaction
                .execute_raw(Statement::from_sql_and_values(
                    transaction.get_database_backend(),
                    "DELETE FROM library_projection_stage WHERE rowid IN (
                    SELECT rowid FROM library_projection_stage
                     WHERE generation = ? OR generation < ? LIMIT 500
                 )",
                    [generation.into(), cutoff.clone().into()],
                ))
                .await?;
            transaction.commit().await?;
            if result.rows_affected() < 500 {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
    }
}
