use sea_orm::{DbErr, EntityTrait, Iden, Schema};
use sea_orm_migration::{
    MigrationName, MigrationTrait, MigratorTrait, SchemaManager, async_trait::async_trait,
    prelude::Index, sea_query::Table,
};

use crate::entity::{
    canonical_release_artist, canonical_torrent, dedupe_catalog_membership, download_client_scan,
    download_event, download_job, download_release_link, release_track_index, runtime_preference,
    single_album_coverage, tracker_snapshot,
};

pub struct Migrator;

#[async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(InitialSchema)]
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
