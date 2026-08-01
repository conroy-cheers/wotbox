use sea_orm::{ConnectionTrait, DbErr, EntityTrait, Iden, Schema};
use sea_orm_migration::{
    MigrationName, MigrationTrait, MigratorTrait, SchemaManager, async_trait::async_trait,
    prelude::Index, sea_query::Table,
};

use crate::entity::{
    artist_source, background_job, canonical_alias, canonical_artist, canonical_backfill_state,
    canonical_release, canonical_release_artist, canonical_release_credit, canonical_torrent,
    channel_config, channel_pack, channel_pack_item, channel_run, dedupe_catalog_membership,
    download_client_scan, download_event, download_job, download_release_link, match_candidate,
    provider_state, release_source, release_track_index, runtime_preference, single_album_coverage,
    tracker_snapshot,
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
        ]
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
