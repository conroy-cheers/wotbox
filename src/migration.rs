use sea_orm::{ConnectionTrait, DbErr, EntityTrait, Iden, Schema};
use sea_orm_migration::{
    MigrationName, MigrationTrait, MigratorTrait, SchemaManager, async_trait::async_trait,
    prelude::Index, sea_query::Table,
};

use crate::entity::{
    artist_source, background_job, canonical_alias, canonical_artist, canonical_backfill_state,
    canonical_release, canonical_release_artist, canonical_release_credit, canonical_torrent,
    change_event, channel_config, channel_pack, channel_pack_item, channel_run,
    dedupe_catalog_membership, download_client_scan, download_event, download_job,
    download_release_link, external_release_link, import_supersession, import_task,
    library_admission, library_artist_projection, library_artist_release_projection, library_asset,
    library_asset_reference, library_projection_dirty, library_projection_state,
    library_release_projection, match_candidate, provider_state, release_source,
    release_track_index, runtime_preference, single_album_coverage, tracker_snapshot,
};

pub struct Migrator;

#[async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(InitialSchema),
            Box::new(CanonicalIdentitySchema),
            Box::new(ChannelSchema),
            Box::new(ChannelProgressSchema),
            Box::new(ProviderSafetySchema),
            Box::new(BackgroundJobSchema),
            Box::new(BackgroundRetryRepairSchema),
            Box::new(BackgroundTerminalNormalizationSchema),
            Box::new(QueryPerformanceSchema),
            Box::new(OpsTorrentDiagnosisSchema),
            Box::new(ImportQueueSchema),
            Box::new(ChannelProviderWaitSchema),
            Box::new(CanonicalReleaseReconciliationSchema),
            Box::new(ExternalReleaseLinksSchema),
            Box::new(ChangeEventSchema),
            Box::new(CanonicalArtistRepairSchema),
            Box::new(LocalLibraryStoreSchema),
            Box::new(LocalLibraryClosureSchema),
            Box::new(LocalLibraryClosureIndexes),
            Box::new(IncrementalLibraryProjectionSchema),
            Box::new(ProjectionTriggerSemantics),
            Box::new(ProjectionSemanticUpdates),
        ]
    }
}

struct ProjectionSemanticUpdates;

impl MigrationName for ProjectionSemanticUpdates {
    fn name(&self) -> &str {
        "m20260901_000022_projection_semantic_updates"
    }
}

#[async_trait]
impl MigrationTrait for ProjectionSemanticUpdates {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP TRIGGER IF EXISTS library_projection_admission_update;
                CREATE TRIGGER library_projection_admission_update
                AFTER UPDATE ON library_admissions
                WHEN OLD.state IS NOT NEW.state
                  OR OLD.error_code IS NOT NEW.error_code
                  OR OLD.error_message IS NOT NEW.error_message
                  OR OLD.admitted_at IS NOT NEW.admitted_at
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_canonical_release_update;
                CREATE TRIGGER library_projection_canonical_release_update
                AFTER UPDATE ON canonical_releases
                WHEN OLD.title IS NOT NEW.title
                  OR OLD.normalized_title IS NOT NEW.normalized_title
                  OR OLD.artist IS NOT NEW.artist
                  OR OLD.year IS NOT NEW.year
                  OR OLD.release_type IS NOT NEW.release_type
                  OR OLD.artwork IS NOT NEW.artwork
                  OR OLD.metadata_json IS NOT NEW.metadata_json
                  OR OLD.provenance_json IS NOT NEW.provenance_json
                  OR OLD.overrides_json IS NOT NEW.overrides_json
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_release_credit_update;
                CREATE TRIGGER library_projection_release_credit_update
                AFTER UPDATE ON canonical_release_credits
                WHEN OLD.release_id IS NOT NEW.release_id
                  OR OLD.artist_id IS NOT NEW.artist_id
                  OR OLD.role IS NOT NEW.role
                  OR OLD.source_count IS NOT NEW.source_count
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                    UNION SELECT NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_canonical_torrent_update;
                CREATE TRIGGER library_projection_canonical_torrent_update
                AFTER UPDATE ON canonical_torrents
                WHEN OLD.group_id IS NOT NEW.group_id
                  OR OLD.release_id IS NOT NEW.release_id
                  OR OLD.info_hash IS NOT NEW.info_hash
                  OR json_remove(OLD.canonical_json,
                        '$.variant.seeders', '$.variant.leechers', '$.variant.snatched')
                     IS NOT json_remove(NEW.canonical_json,
                        '$.variant.seeders', '$.variant.leechers', '$.variant.snatched')
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE OLD.release_id IS NOT NULL
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE NEW.release_id IS NOT NULL
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_release_source_update;
                CREATE TRIGGER library_projection_release_source_update
                AFTER UPDATE ON release_sources
                WHEN OLD.release_id IS NOT NEW.release_id
                  OR OLD.normalized_title IS NOT NEW.normalized_title
                  OR OLD.normalized_artist IS NOT NEW.normalized_artist
                  OR OLD.year IS NOT NEW.year
                  OR OLD.release_type IS NOT NEW.release_type
                  OR OLD.matcher_version IS NOT NEW.matcher_version
                  OR OLD.source_json IS NOT NEW.source_json
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                    UNION SELECT NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_artist_update;
                CREATE TRIGGER library_projection_artist_update
                AFTER UPDATE ON canonical_artists
                WHEN OLD.name IS NOT NEW.name
                  OR OLD.normalized_name IS NOT NEW.normalized_name
                  OR OLD.artwork IS NOT NEW.artwork
                  OR OLD.metadata_json IS NOT NEW.metadata_json
                  OR OLD.provenance_json IS NOT NEW.provenance_json
                  OR OLD.overrides_json IS NOT NEW.overrides_json
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM canonical_release_credits WHERE artist_id = NEW.id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_artist_source_change_update;
                CREATE TRIGGER library_projection_artist_source_change_update
                AFTER UPDATE ON artist_sources
                WHEN OLD.artist_id IS NOT NEW.artist_id
                  OR OLD.canonical_artist_id IS NOT NEW.canonical_artist_id
                  OR OLD.name IS NOT NEW.name
                  OR OLD.normalized_name IS NOT NEW.normalized_name
                  OR OLD.matcher_version IS NOT NEW.matcher_version
                  OR OLD.source_json IS NOT NEW.source_json
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM canonical_release_credits
                     WHERE artist_id IN (OLD.canonical_artist_id, NEW.canonical_artist_id)
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_coverage_update;
                CREATE TRIGGER library_projection_coverage_update
                AFTER UPDATE ON single_album_coverages
                WHEN OLD.tracker IS NOT NEW.tracker
                  OR OLD.single_group_id IS NOT NEW.single_group_id
                  OR OLD.state IS NOT NEW.state
                  OR OLD.coverage_json IS NOT NEW.coverage_json
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM release_sources
                     WHERE (lower(tracker) = lower(OLD.tracker) AND group_id = OLD.single_group_id)
                        OR (lower(tracker) = lower(NEW.tracker) AND group_id = NEW.single_group_id)
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_legacy_credit_update;
                CREATE TRIGGER library_projection_legacy_credit_update
                AFTER UPDATE ON canonical_release_artists
                WHEN OLD.artist_id IS NOT NEW.artist_id
                  OR OLD.name IS NOT NEW.name
                  OR OLD.sort_name IS NOT NEW.sort_name
                  OR OLD.source IS NOT NEW.source
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM release_sources
                     WHERE (lower(tracker) = lower(OLD.tracker) AND group_id = OLD.group_id)
                        OR (lower(tracker) = lower(NEW.tracker) AND group_id = NEW.group_id)
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_alias_update;
                CREATE TRIGGER library_projection_alias_update
                AFTER UPDATE ON canonical_aliases
                WHEN OLD.target_id IS NOT NEW.target_id
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES ('*', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                DROP TRIGGER IF EXISTS library_projection_preferences_update;
                CREATE TRIGGER library_projection_preferences_update
                AFTER UPDATE ON runtime_preferences
                WHEN OLD.value_json IS NOT NEW.value_json
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES ('*', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct ProjectionTriggerSemantics;

impl MigrationName for ProjectionTriggerSemantics {
    fn name(&self) -> &str {
        "m20260901_000021_projection_trigger_semantics"
    }
}

#[async_trait]
impl MigrationTrait for ProjectionTriggerSemantics {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP TRIGGER IF EXISTS library_projection_download_link_update;
                CREATE TRIGGER library_projection_download_link_update
                AFTER UPDATE OF release_id, resolution_state, present, library_added_at,
                                completed_at, missing_since, tracker, group_id, torrent_id
                ON download_release_links
                WHEN OLD.release_id IS NOT NEW.release_id
                  OR OLD.resolution_state IS NOT NEW.resolution_state
                  OR OLD.present IS NOT NEW.present
                  OR OLD.library_added_at IS NOT NEW.library_added_at
                  OR OLD.completed_at IS NOT NEW.completed_at
                  OR OLD.missing_since IS NOT NEW.missing_since
                  OR OLD.tracker IS NOT NEW.tracker
                  OR OLD.group_id IS NOT NEW.group_id
                  OR OLD.torrent_id IS NOT NEW.torrent_id
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE OLD.release_id IS NOT NULL
                    ON CONFLICT(release_id) DO UPDATE SET
                        version = version + 1,
                        updated_at = excluded.updated_at;
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE NEW.release_id IS NOT NULL
                    ON CONFLICT(release_id) DO UPDATE SET
                        version = version + 1,
                        updated_at = excluded.updated_at;
                END;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct IncrementalLibraryProjectionSchema;

impl MigrationName for IncrementalLibraryProjectionSchema {
    fn name(&self) -> &str {
        "m20260901_000020_incremental_library_projection"
    }
}

#[async_trait]
impl MigrationTrait for IncrementalLibraryProjectionSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create_entity(manager, &schema, library_release_projection::Entity).await?;
        create_entity(manager, &schema, library_artist_projection::Entity).await?;
        create_entity(manager, &schema, library_artist_release_projection::Entity).await?;
        create_entity(manager, &schema, library_projection_dirty::Entity).await?;
        create_entity(manager, &schema, library_projection_state::Entity).await?;
        add_column_if_missing(
            manager,
            &schema,
            change_event::Entity,
            change_event::Column::Resources,
        )
        .await?;
        add_column_if_missing(
            manager,
            &schema,
            change_event::Entity,
            change_event::Column::PayloadJson,
        )
        .await?;

        for index in [
            Index::create()
                .if_not_exists()
                .name("idx_library_release_projection_title")
                .table(library_release_projection::Entity)
                .col(library_release_projection::Column::NormalizedTitle)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_library_release_projection_search")
                .table(library_release_projection::Entity)
                .col(library_release_projection::Column::SearchText)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_library_release_projection_availability")
                .table(library_release_projection::Entity)
                .col(library_release_projection::Column::Availability)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_library_artist_projection_name")
                .table(library_artist_projection::Entity)
                .col(library_artist_projection::Column::NormalizedName)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_library_artist_release_release")
                .table(library_artist_release_projection::Entity)
                .col(library_artist_release_projection::Column::ReleaseId)
                .to_owned(),
        ] {
            manager.create_index(index).await?;
        }

        manager
            .get_connection()
            .execute_unprepared(
                r#"
                INSERT OR IGNORE INTO library_projection_state
                    (id, revision, schema_version, ready, updated_at)
                VALUES (1, 0, 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

                INSERT OR IGNORE INTO library_projection_dirty
                    (release_id, version, updated_at)
                VALUES ('*', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

                CREATE TRIGGER IF NOT EXISTS library_projection_download_link_insert
                AFTER INSERT ON download_release_links
                WHEN NEW.release_id IS NOT NULL
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET
                        version = version + 1,
                        updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_download_link_update
                AFTER UPDATE OF release_id, resolution_state, present, library_added_at,
                                completed_at, missing_since, tracker, group_id, torrent_id
                ON download_release_links
                WHEN OLD.release_id IS NOT NEW.release_id
                  OR OLD.resolution_state IS NOT NEW.resolution_state
                  OR OLD.present IS NOT NEW.present
                  OR OLD.library_added_at IS NOT NEW.library_added_at
                  OR OLD.completed_at IS NOT NEW.completed_at
                  OR OLD.missing_since IS NOT NEW.missing_since
                  OR OLD.tracker IS NOT NEW.tracker
                  OR OLD.group_id IS NOT NEW.group_id
                  OR OLD.torrent_id IS NOT NEW.torrent_id
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE OLD.release_id IS NOT NULL
                    ON CONFLICT(release_id) DO UPDATE SET
                        version = version + 1,
                        updated_at = excluded.updated_at;
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE NEW.release_id IS NOT NULL
                    ON CONFLICT(release_id) DO UPDATE SET
                        version = version + 1,
                        updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_download_link_delete
                AFTER DELETE ON download_release_links
                WHEN OLD.release_id IS NOT NULL
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET
                        version = version + 1,
                        updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_admission_insert
                AFTER INSERT ON library_admissions
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_admission_update
                AFTER UPDATE ON library_admissions
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_admission_delete
                AFTER DELETE ON library_admissions
                BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_canonical_release_insert
                AFTER INSERT ON canonical_releases BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_canonical_release_update
                AFTER UPDATE ON canonical_releases BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_canonical_release_delete
                AFTER DELETE ON canonical_releases BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (OLD.id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_release_credit_insert
                AFTER INSERT ON canonical_release_credits BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_release_credit_update
                AFTER UPDATE ON canonical_release_credits BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_release_credit_delete
                AFTER DELETE ON canonical_release_credits BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_canonical_torrent_insert
                AFTER INSERT ON canonical_torrents WHEN NEW.release_id IS NOT NULL BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_canonical_torrent_update
                AFTER UPDATE ON canonical_torrents BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE OLD.release_id IS NOT NULL
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE NEW.release_id IS NOT NULL
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_canonical_torrent_delete
                AFTER DELETE ON canonical_torrents WHEN OLD.release_id IS NOT NULL BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_release_source_insert
                AFTER INSERT ON release_sources BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_release_source_update
                AFTER UPDATE ON release_sources BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (NEW.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_release_source_delete
                AFTER DELETE ON release_sources BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES (OLD.release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_artist_insert
                AFTER INSERT ON canonical_artists BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM canonical_release_credits WHERE artist_id = NEW.id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_artist_update
                AFTER UPDATE ON canonical_artists BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM canonical_release_credits WHERE artist_id = NEW.id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_artist_delete
                AFTER DELETE ON canonical_artists BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES ('*', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_artist_source_change_insert
                AFTER INSERT ON artist_sources BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM canonical_release_credits WHERE artist_id = NEW.canonical_artist_id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_artist_source_change_update
                AFTER UPDATE ON artist_sources BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM canonical_release_credits
                     WHERE artist_id IN (OLD.canonical_artist_id, NEW.canonical_artist_id)
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_artist_source_change_delete
                AFTER DELETE ON artist_sources BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM canonical_release_credits WHERE artist_id = OLD.canonical_artist_id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_coverage_insert
                AFTER INSERT ON single_album_coverages BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM release_sources
                     WHERE lower(tracker) = lower(NEW.tracker) AND group_id = NEW.single_group_id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_coverage_update
                AFTER UPDATE ON single_album_coverages BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM release_sources
                     WHERE lower(tracker) = lower(NEW.tracker) AND group_id = NEW.single_group_id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_coverage_delete
                AFTER DELETE ON single_album_coverages BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM release_sources
                     WHERE lower(tracker) = lower(OLD.tracker) AND group_id = OLD.single_group_id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_legacy_credit_insert
                AFTER INSERT ON canonical_release_artists BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM release_sources
                     WHERE lower(tracker) = lower(NEW.tracker) AND group_id = NEW.group_id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_legacy_credit_update
                AFTER UPDATE ON canonical_release_artists BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM release_sources
                     WHERE (lower(tracker) = lower(OLD.tracker) AND group_id = OLD.group_id)
                        OR (lower(tracker) = lower(NEW.tracker) AND group_id = NEW.group_id)
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_legacy_credit_delete
                AFTER DELETE ON canonical_release_artists BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    SELECT release_id, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                      FROM release_sources
                     WHERE lower(tracker) = lower(OLD.tracker) AND group_id = OLD.group_id
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_alias_insert
                AFTER INSERT ON canonical_aliases BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES ('*', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_alias_update
                AFTER UPDATE ON canonical_aliases BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES ('*', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_alias_delete
                AFTER DELETE ON canonical_aliases BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES ('*', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;

                CREATE TRIGGER IF NOT EXISTS library_projection_preferences_insert
                AFTER INSERT ON runtime_preferences BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES ('*', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                CREATE TRIGGER IF NOT EXISTS library_projection_preferences_update
                AFTER UPDATE ON runtime_preferences BEGIN
                    INSERT INTO library_projection_dirty(release_id, version, updated_at)
                    VALUES ('*', 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                    ON CONFLICT(release_id) DO UPDATE SET version = version + 1, updated_at = excluded.updated_at;
                END;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct LocalLibraryClosureIndexes;

impl MigrationName for LocalLibraryClosureIndexes {
    fn name(&self) -> &str {
        "m20260831_000019_local_library_closure_indexes"
    }
}

#[async_trait]
impl MigrationTrait for LocalLibraryClosureIndexes {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_artist_sources_tracker_artist")
                    .table(artist_source::Entity)
                    .col(artist_source::Column::Tracker)
                    .col(artist_source::Column::ArtistId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Application data migrations are intentionally forward-only.
        Ok(())
    }
}

struct LocalLibraryClosureSchema;

impl MigrationName for LocalLibraryClosureSchema {
    fn name(&self) -> &str {
        "m20260831_000018_local_library_closure"
    }
}

#[async_trait]
impl MigrationTrait for LocalLibraryClosureSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        add_column_if_missing(
            manager,
            &schema,
            library_asset::Entity,
            library_asset::Column::RetryAfter,
        )
        .await?;
        add_column_if_missing(
            manager,
            &schema,
            library_asset::Entity,
            library_asset::Column::MaterializerVersion,
        )
        .await?;
        create_entity(manager, &schema, library_asset_reference::Entity).await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_library_asset_references_url")
                    .table(library_asset_reference::Entity)
                    .col(library_asset_reference::Column::SourceUrl)
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE library_assets
                   SET state = CASE
                       WHEN state <> 'failed' THEN state
                       WHEN error_message LIKE '%definitively absent%' THEN 'absent'
                       WHEN error_message LIKE '%unsupported static artwork format%'
                         OR error_message LIKE '%recognize artwork format%'
                         OR error_message LIKE '%decode artwork%'
                         OR error_message LIKE '%encoded limit%'
                         OR error_message LIKE '%decoded limit%'
                         OR error_message LIKE '%megapixel%' THEN 'unsupported'
                       WHEN EXISTS (
                           SELECT 1 FROM background_jobs j
                            WHERE j.deduplication_key = 'materialize-library-asset:' || library_assets.source_hash || ':v1'
                              AND j.state IN ('pending', 'running', 'retrying')
                       ) THEN 'retrying'
                       ELSE 'failed_terminal'
                   END,
                       retry_after = CASE
                           WHEN state = 'failed'
                            AND error_message NOT LIKE '%definitively absent%'
                            AND error_message NOT LIKE '%unsupported static artwork format%'
                            AND error_message NOT LIKE '%recognize artwork format%'
                            AND error_message NOT LIKE '%decode artwork%'
                            AND error_message NOT LIKE '%encoded limit%'
                            AND error_message NOT LIKE '%decoded limit%'
                            AND error_message NOT LIKE '%megapixel%'
                            AND NOT EXISTS (
                                SELECT 1 FROM background_jobs j
                                 WHERE j.deduplication_key = 'materialize-library-asset:' || library_assets.source_hash || ':v1'
                                   AND j.state IN ('pending', 'running', 'retrying')
                            ) THEN datetime('now', '+7 days')
                           ELSE NULL
                       END,
                       materializer_version = 1;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Application data migrations are intentionally forward-only.
        Ok(())
    }
}

struct LocalLibraryStoreSchema;

impl MigrationName for LocalLibraryStoreSchema {
    fn name(&self) -> &str {
        "m20260831_000017_local_library_store"
    }
}

#[async_trait]
impl MigrationTrait for LocalLibraryStoreSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create_entity(manager, &schema, library_asset::Entity).await?;
        create_entity(manager, &schema, library_admission::Entity).await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .unique()
                    .name("idx_library_assets_source_url")
                    .table(library_asset::Entity)
                    .col(library_asset::Column::SourceUrl)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_library_assets_state")
                    .table(library_asset::Entity)
                    .col(library_asset::Column::State)
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                INSERT OR IGNORE INTO library_admissions
                    (release_id, state, error_code, error_message, admitted_at, updated_at)
                SELECT DISTINCT release_id, 'published', NULL, NULL,
                       COALESCE(library_added_at, updated_at),
                       COALESCE(library_added_at, updated_at)
                  FROM download_release_links
                 WHERE library_added_at IS NOT NULL
                   AND release_id IS NOT NULL;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct CanonicalArtistRepairSchema;

impl MigrationName for CanonicalArtistRepairSchema {
    fn name(&self) -> &str {
        "m20260830_000016_canonical_artist_repair"
    }
}

#[async_trait]
impl MigrationTrait for CanonicalArtistRepairSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        if !manager
            .has_column("artist_sources", "matcher_version")
            .await?
        {
            manager
                .get_connection()
                .execute_unprepared(
                    "ALTER TABLE artist_sources ADD COLUMN matcher_version INTEGER NOT NULL DEFAULT 0",
                )
                .await?;
        }
        add_column_if_missing(
            manager,
            &schema,
            canonical_backfill_state::Entity,
            canonical_backfill_state::Column::Fingerprint,
        )
        .await?;
        add_column_if_missing(
            manager,
            &schema,
            canonical_backfill_state::Entity,
            canonical_backfill_state::Column::DetailsJson,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct ChangeEventSchema;

impl MigrationName for ChangeEventSchema {
    fn name(&self) -> &str {
        "m20260830_000015_change_events"
    }
}

#[async_trait]
impl MigrationTrait for ChangeEventSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create_entity(manager, &schema, change_event::Entity).await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_change_events_created")
                    .table(change_event::Entity)
                    .col(change_event::Column::CreatedAt)
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE background_jobs
                   SET state = 'cancelled',
                       cancelled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                       finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE kind = 'scan_download_client'
                   AND state IN ('pending', 'running', 'retrying', 'waiting');
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct ExternalReleaseLinksSchema;

impl MigrationName for ExternalReleaseLinksSchema {
    fn name(&self) -> &str {
        "m20260829_000014_external_release_links"
    }
}

#[async_trait]
impl MigrationTrait for ExternalReleaseLinksSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create_entity(manager, &schema, external_release_link::Entity).await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_external_release_links_release_id")
                    .table(external_release_link::Entity)
                    .col(external_release_link::Column::ReleaseId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct CanonicalReleaseReconciliationSchema;

impl MigrationName for CanonicalReleaseReconciliationSchema {
    fn name(&self) -> &str {
        "m20260803_000013_canonical_release_reconciliation"
    }
}

#[async_trait]
impl MigrationTrait for CanonicalReleaseReconciliationSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        add_column_if_missing(
            manager,
            &schema,
            release_source::Entity,
            release_source::Column::MatcherVersion,
        )
        .await?;
        for index in [
            Index::create()
                .if_not_exists()
                .name("idx_release_sources_matcher_version")
                .table(release_source::Entity)
                .col(release_source::Column::MatcherVersion)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_release_sources_artist_year")
                .table(release_source::Entity)
                .col(release_source::Column::NormalizedArtist)
                .col(release_source::Column::Year)
                .to_owned(),
        ] {
            manager.create_index(index).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct ChannelProviderWaitSchema;

impl MigrationName for ChannelProviderWaitSchema {
    fn name(&self) -> &str {
        "m20260803_000012_channel_provider_wait"
    }
}

#[async_trait]
impl MigrationTrait for ChannelProviderWaitSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        add_column_if_missing(
            manager,
            &schema,
            channel_run::Entity,
            channel_run::Column::RetryAt,
        )
        .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct ImportQueueSchema;

impl MigrationName for ImportQueueSchema {
    fn name(&self) -> &str {
        "m20260802_000011_import_queue"
    }
}

#[async_trait]
impl MigrationTrait for ImportQueueSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create_entity(manager, &schema, import_task::Entity).await?;
        create_entity(manager, &schema, import_supersession::Entity).await?;
        for index in [
            Index::create()
                .if_not_exists()
                .unique()
                .name("idx_import_tasks_client_hash")
                .table(import_task::Entity)
                .col(import_task::Column::Client)
                .col(import_task::Column::InfoHash)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .unique()
                .name("idx_import_tasks_download_job")
                .table(import_task::Entity)
                .col(import_task::Column::DownloadJobId)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_import_tasks_state_updated")
                .table(import_task::Entity)
                .col(import_task::Column::State)
                .col(import_task::Column::UpdatedAt)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_import_supersessions_state")
                .table(import_supersession::Entity)
                .col(import_supersession::Column::CleanupState)
                .col(import_supersession::Column::UpdatedAt)
                .to_owned(),
        ] {
            manager.create_index(index).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct OpsTorrentDiagnosisSchema;

impl MigrationName for OpsTorrentDiagnosisSchema {
    fn name(&self) -> &str {
        "m20260802_000010_ops_torrent_diagnosis"
    }
}

#[async_trait]
impl MigrationTrait for OpsTorrentDiagnosisSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        add_column_if_missing(
            manager,
            &schema,
            download_release_link::Entity,
            download_release_link::Column::TorrentName,
        )
        .await?;
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE download_release_links
                   SET resolution_state = 'pending',
                       attempts = 0,
                       next_retry_at = NULL,
                       error_code = NULL,
                       error_message = NULL,
                       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE lower(tracker) = 'ops'
                   AND resolution_state = 'not_found'
                   AND error_code = 'not_found';
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct QueryPerformanceSchema;

impl MigrationName for QueryPerformanceSchema {
    fn name(&self) -> &str {
        "m20260801_000009_query_performance"
    }
}

#[async_trait]
impl MigrationTrait for QueryPerformanceSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        for column in [
            download_release_link::Column::ObservedJson,
            download_release_link::Column::ObservedAt,
            download_release_link::Column::ClientAddedAt,
        ] {
            add_column_if_missing(manager, &schema, download_release_link::Entity, column).await?;
        }
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE download_release_links
                   SET client_added_at = first_seen_at
                 WHERE client_added_at IS NULL;
                "#,
            )
            .await?;
        for index in [
            Index::create()
                .if_not_exists()
                .name("idx_download_links_page")
                .table(download_release_link::Entity)
                .col(download_release_link::Column::ResolutionState)
                .col(download_release_link::Column::Present)
                .col(download_release_link::Column::ClientAddedAt)
                .col(download_release_link::Column::Client)
                .col(download_release_link::Column::InfoHash)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_download_links_library_release")
                .table(download_release_link::Entity)
                .col(download_release_link::Column::ResolutionState)
                .col(download_release_link::Column::ReleaseId)
                .col(download_release_link::Column::LibraryAddedAt)
                .col(download_release_link::Column::Present)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_canonical_torrents_release")
                .table(canonical_torrent::Entity)
                .col(canonical_torrent::Column::ReleaseId)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_release_credits_artist")
                .table(canonical_release_credit::Entity)
                .col(canonical_release_credit::Column::ArtistId)
                .col(canonical_release_credit::Column::ReleaseId)
                .to_owned(),
        ] {
            manager.create_index(index).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct BackgroundTerminalNormalizationSchema;

impl MigrationName for BackgroundTerminalNormalizationSchema {
    fn name(&self) -> &str {
        "m20260801_000008_background_terminal_normalization"
    }
}

#[async_trait]
impl MigrationTrait for BackgroundTerminalNormalizationSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE background_jobs
                   SET last_error_code = 'not_found',
                       last_error_message = 'OPS did not recognize this torrent hash; manual retry is available',
                       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE kind = 'resolve_download_hash'
                   AND state = 'failed'
                   AND lower(COALESCE(last_error_message, '')) LIKE '%bad parameters%';
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct BackgroundRetryRepairSchema;

impl MigrationName for BackgroundRetryRepairSchema {
    fn name(&self) -> &str {
        "m20260801_000007_background_retry_repair"
    }
}

#[async_trait]
impl MigrationTrait for BackgroundRetryRepairSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        for column in [
            background_job::Column::ProviderId,
            background_job::Column::Lane,
            background_job::Column::Deferrals,
        ] {
            add_column_if_missing(manager, &schema, background_job::Entity, column).await?;
        }
        for column in [
            provider_state::Column::LastBackgroundRequestAt,
            provider_state::Column::BackgroundMinimumIntervalMs,
        ] {
            add_column_if_missing(manager, &schema, provider_state::Entity, column).await?;
        }
        for index in [
            Index::create()
                .if_not_exists()
                .name("idx_background_jobs_lane_due")
                .table(background_job::Entity)
                .col(background_job::Column::Lane)
                .col(background_job::Column::State)
                .col(background_job::Column::NextRunAt)
                .col(background_job::Column::Priority)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_background_jobs_provider")
                .table(background_job::Entity)
                .col(background_job::Column::ProviderId)
                .col(background_job::Column::State)
                .to_owned(),
        ] {
            manager.create_index(index).await?;
        }

        // Retire jobs governed by the legacy retry loop before workers can see them.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE background_jobs
                   SET state = 'cancelled',
                       lease_owner = NULL,
                       lease_until = NULL,
                       next_run_at = NULL,
                       last_error_code = 'superseded_retry_model',
                       last_error_message = 'Quarantined during background retry repair',
                       cancelled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE kind IN ('resolve_download_hash', 'index_tracklist', 'compute_single_coverage')
                   AND state IN ('pending', 'running', 'retrying', 'waiting');

                UPDATE download_release_links
                   SET resolution_state = 'not_found',
                       attempts = 0,
                       next_retry_at = NULL,
                       error_code = 'not_found',
                       error_message = 'OPS did not recognize this torrent hash; manual retry is available',
                       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE lower(tracker) = 'ops'
                   AND lower(COALESCE(error_message, '')) LIKE '%bad parameters%';

                UPDATE download_release_links
                   SET resolution_state = 'pending',
                       attempts = 0,
                       next_retry_at = NULL,
                       error_code = NULL,
                       error_message = NULL,
                       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE resolution_state IN ('failed', 'resolving')
                   AND COALESCE(error_code, '') != 'not_found';

                UPDATE release_track_indexes
                   SET state = 'pending',
                       attempts = 0,
                       next_retry_at = NULL,
                       error_message = NULL,
                       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE state IN ('failed', 'resolving');
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct BackgroundJobSchema;

impl MigrationName for BackgroundJobSchema {
    fn name(&self) -> &str {
        "m20260731_000006_background_jobs"
    }
}

#[async_trait]
impl MigrationTrait for BackgroundJobSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create_entity(manager, &schema, background_job::Entity).await?;
        for index in [
            Index::create()
                .if_not_exists()
                .name("idx_background_jobs_due")
                .table(background_job::Entity)
                .col(background_job::Column::State)
                .col(background_job::Column::NextRunAt)
                .col(background_job::Column::Priority)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_background_jobs_lease")
                .table(background_job::Entity)
                .col(background_job::Column::State)
                .col(background_job::Column::LeaseUntil)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_background_jobs_updated")
                .table(background_job::Entity)
                .col(background_job::Column::UpdatedAt)
                .to_owned(),
        ] {
            manager.create_index(index).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct ProviderSafetySchema;

impl MigrationName for ProviderSafetySchema {
    fn name(&self) -> &str {
        "m20260730_000005_provider_safety"
    }
}

#[async_trait]
impl MigrationTrait for ProviderSafetySchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create_entity(manager, &schema, provider_state::Entity).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct ChannelProgressSchema;

impl MigrationName for ChannelProgressSchema {
    fn name(&self) -> &str {
        "m20260730_000004_channel_progress"
    }
}

#[async_trait]
impl MigrationTrait for ChannelProgressSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        for column in [
            channel_run::Column::Phase,
            channel_run::Column::ProgressCompleted,
            channel_run::Column::ProgressTotal,
            channel_run::Column::ProgressMessage,
            channel_run::Column::UpdatedAt,
        ] {
            add_column_if_missing(manager, &schema, channel_run::Entity, column).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct ChannelSchema;

impl MigrationName for ChannelSchema {
    fn name(&self) -> &str {
        "m20260730_000003_channels"
    }
}

#[async_trait]
impl MigrationTrait for ChannelSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create_entity(manager, &schema, channel_config::Entity).await?;
        create_entity(manager, &schema, channel_run::Entity).await?;
        create_entity(manager, &schema, channel_pack::Entity).await?;
        create_entity(manager, &schema, channel_pack_item::Entity).await?;
        for index in [
            Index::create()
                .if_not_exists()
                .name("idx_channel_runs_status")
                .table(channel_run::Entity)
                .col(channel_run::Column::ChannelId)
                .col(channel_run::Column::Status)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_channel_packs_history")
                .table(channel_pack::Entity)
                .col(channel_pack::Column::ChannelId)
                .col(channel_pack::Column::CreatedAt)
                .to_owned(),
        ] {
            manager.create_index(index).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct CanonicalIdentitySchema;

impl MigrationName for CanonicalIdentitySchema {
    fn name(&self) -> &str {
        "m20260729_000002_canonical_identity"
    }
}

#[async_trait]
impl MigrationTrait for CanonicalIdentitySchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        create_entity(manager, &schema, canonical_release::Entity).await?;
        create_entity(manager, &schema, release_source::Entity).await?;
        create_entity(manager, &schema, canonical_artist::Entity).await?;
        create_entity(manager, &schema, artist_source::Entity).await?;
        create_entity(manager, &schema, canonical_release_credit::Entity).await?;
        create_entity(manager, &schema, canonical_alias::Entity).await?;
        create_entity(manager, &schema, match_candidate::Entity).await?;
        create_entity(manager, &schema, canonical_backfill_state::Entity).await?;
        add_column_if_missing(
            manager,
            &schema,
            canonical_torrent::Entity,
            canonical_torrent::Column::ReleaseId,
        )
        .await?;
        add_column_if_missing(
            manager,
            &schema,
            download_release_link::Entity,
            download_release_link::Column::ReleaseId,
        )
        .await?;

        let indexes = [
            Index::create()
                .if_not_exists()
                .name("idx_release_sources_canonical")
                .table(release_source::Entity)
                .col(release_source::Column::ReleaseId)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_release_sources_match")
                .table(release_source::Entity)
                .col(release_source::Column::NormalizedTitle)
                .col(release_source::Column::NormalizedArtist)
                .col(release_source::Column::Year)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_artist_sources_canonical")
                .table(artist_source::Entity)
                .col(artist_source::Column::CanonicalArtistId)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_artist_sources_match")
                .table(artist_source::Entity)
                .col(artist_source::Column::NormalizedName)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_canonical_credits_artist")
                .table(canonical_release_credit::Entity)
                .col(canonical_release_credit::Column::ArtistId)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .unique()
                .name("idx_match_candidates_pair")
                .table(match_candidate::Entity)
                .col(match_candidate::Column::Kind)
                .col(match_candidate::Column::LeftId)
                .col(match_candidate::Column::RightId)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_match_candidates_status")
                .table(match_candidate::Entity)
                .col(match_candidate::Column::Status)
                .col(match_candidate::Column::Score)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_canonical_torrents_release_id")
                .table(canonical_torrent::Entity)
                .col(canonical_torrent::Column::ReleaseId)
                .to_owned(),
            Index::create()
                .if_not_exists()
                .name("idx_download_links_release_id")
                .table(download_release_link::Entity)
                .col(download_release_link::Column::ReleaseId)
                .to_owned(),
        ];
        for index in indexes {
            manager.create_index(index).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct InitialSchema;

impl MigrationName for InitialSchema {
    fn name(&self) -> &str {
        "m20260728_000001_initial_schema"
    }
}

#[async_trait]
impl MigrationTrait for InitialSchema {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());

        create_entity(manager, &schema, tracker_snapshot::Entity).await?;
        create_entity(manager, &schema, download_job::Entity).await?;
        create_entity(manager, &schema, download_event::Entity).await?;
        create_entity(manager, &schema, canonical_torrent::Entity).await?;
        create_entity(manager, &schema, download_release_link::Entity).await?;
        create_entity(manager, &schema, canonical_release_artist::Entity).await?;
        create_entity(manager, &schema, download_client_scan::Entity).await?;
        create_entity(manager, &schema, runtime_preference::Entity).await?;
        create_entity(manager, &schema, release_track_index::Entity).await?;
        create_entity(manager, &schema, dedupe_catalog_membership::Entity).await?;
        create_entity(manager, &schema, single_album_coverage::Entity).await?;

        add_missing_link_columns(manager, &schema).await?;
        add_missing_track_index_columns(manager, &schema).await?;
        create_indexes(manager).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Application data migrations are intentionally forward-only.
        Ok(())
    }
}

async fn create_entity<E>(
    manager: &SchemaManager<'_>,
    schema: &Schema,
    entity: E,
) -> Result<(), DbErr>
where
    E: EntityTrait,
{
    let mut statement = schema.create_table_from_entity(entity);
    statement.if_not_exists();
    manager.create_table(statement).await
}

async fn add_missing_link_columns(
    manager: &SchemaManager<'_>,
    schema: &Schema,
) -> Result<(), DbErr> {
    add_column_if_missing(
        manager,
        schema,
        download_release_link::Entity,
        download_release_link::Column::Present,
    )
    .await?;
    add_column_if_missing(
        manager,
        schema,
        download_release_link::Entity,
        download_release_link::Column::MissingSince,
    )
    .await?;
    add_column_if_missing(
        manager,
        schema,
        download_release_link::Entity,
        download_release_link::Column::LibraryAddedAt,
    )
    .await?;
    add_column_if_missing(
        manager,
        schema,
        download_release_link::Entity,
        download_release_link::Column::CompletedAt,
    )
    .await
}

async fn add_missing_track_index_columns(
    manager: &SchemaManager<'_>,
    schema: &Schema,
) -> Result<(), DbErr> {
    add_column_if_missing(
        manager,
        schema,
        release_track_index::Entity,
        release_track_index::Column::Priority,
    )
    .await
}

async fn add_column_if_missing<E>(
    manager: &SchemaManager<'_>,
    schema: &Schema,
    entity: E,
    column: E::Column,
) -> Result<(), DbErr>
where
    E: EntityTrait,
{
    let table_name = entity.table_name();
    let column_name = column.to_string();
    if manager.has_column(table_name, &column_name).await? {
        return Ok(());
    }

    let mut definition = schema.get_column_def::<E>(column);
    let mut statement = Table::alter();
    statement.table(entity).add_column(&mut definition);
    manager.alter_table(statement.to_owned()).await
}

async fn create_indexes(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let indexes = [
        Index::create()
            .if_not_exists()
            .unique()
            .name("idx_tracker_snapshots_resource")
            .table(tracker_snapshot::Entity)
            .col(tracker_snapshot::Column::Tracker)
            .col(tracker_snapshot::Column::ResourceKind)
            .col(tracker_snapshot::Column::ResourceKey)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_tracker_snapshots_expiry")
            .table(tracker_snapshot::Entity)
            .col(tracker_snapshot::Column::ExpiresAt)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .unique()
            .name("idx_download_jobs_torrent_profile")
            .table(download_job::Entity)
            .col(download_job::Column::Tracker)
            .col(download_job::Column::TorrentId)
            .col(download_job::Column::Profile)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .unique()
            .name("idx_download_jobs_idempotency")
            .table(download_job::Entity)
            .col(download_job::Column::IdempotencyKey)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_download_events_job_created")
            .table(download_event::Entity)
            .col(download_event::Column::JobId)
            .col(download_event::Column::CreatedAt)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .unique()
            .name("idx_canonical_torrents_hash")
            .table(canonical_torrent::Entity)
            .col(canonical_torrent::Column::Tracker)
            .col(canonical_torrent::Column::InfoHash)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_canonical_torrents_group")
            .table(canonical_torrent::Entity)
            .col(canonical_torrent::Column::Tracker)
            .col(canonical_torrent::Column::GroupId)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_download_links_resolution")
            .table(download_release_link::Entity)
            .col(download_release_link::Column::ResolutionState)
            .col(download_release_link::Column::NextRetryAt)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_download_links_release")
            .table(download_release_link::Entity)
            .col(download_release_link::Column::Tracker)
            .col(download_release_link::Column::GroupId)
            .col(download_release_link::Column::TorrentId)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_download_links_library")
            .table(download_release_link::Entity)
            .col(download_release_link::Column::LibraryAddedAt)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_release_artists_browse")
            .table(canonical_release_artist::Entity)
            .col(canonical_release_artist::Column::Role)
            .col(canonical_release_artist::Column::SortName)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_track_indexes_due")
            .table(release_track_index::Entity)
            .col(release_track_index::Column::State)
            .col(release_track_index::Column::NextRetryAt)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_track_indexes_priority")
            .table(release_track_index::Entity)
            .col(release_track_index::Column::Priority)
            .col(release_track_index::Column::UpdatedAt)
            .to_owned(),
        Index::create()
            .if_not_exists()
            .name("idx_catalog_membership_group")
            .table(dedupe_catalog_membership::Entity)
            .col(dedupe_catalog_membership::Column::Tracker)
            .col(dedupe_catalog_membership::Column::GroupId)
            .to_owned(),
    ];

    for index in indexes {
        manager.create_index(index).await?;
    }
    Ok(())
}
