pub mod tracker_snapshot {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "tracker_snapshots")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub tracker: String,
        pub resource_kind: String,
        pub resource_key: String,
        #[sea_orm(column_type = "Json")]
        pub normalized_json: Json,
        #[sea_orm(column_type = "Json")]
        pub sanitized_raw_json: Json,
        pub fetched_at: String,
        pub expires_at: String,
        #[sea_orm(default_value = 1)]
        pub schema_version: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod download_job {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "download_jobs")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub idempotency_key: Option<String>,
        pub tracker: String,
        pub torrent_id: i64,
        pub group_id: Option<i64>,
        pub profile: String,
        #[sea_orm(default_value = false)]
        pub use_token: bool,
        pub info_hash: Option<String>,
        pub name: Option<String>,
        pub state: String,
        #[sea_orm(default_value = 0.0)]
        pub progress: f64,
        #[sea_orm(default_value = 0)]
        pub download_speed: i64,
        #[sea_orm(default_value = 0)]
        pub upload_speed: i64,
        pub eta: Option<i64>,
        pub error_code: Option<String>,
        pub error_message: Option<String>,
        pub created_at: String,
        pub updated_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(has_many = "super::download_event::Entity")]
        DownloadEvent,
    }

    impl Related<super::download_event::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::DownloadEvent.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod download_event {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "download_events")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub job_id: String,
        pub state: String,
        pub detail: Option<String>,
        pub created_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::download_job::Entity",
            from = "Column::JobId",
            to = "super::download_job::Column::Id",
            on_delete = "Cascade"
        )]
        DownloadJob,
    }

    impl Related<super::download_job::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::DownloadJob.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod canonical_torrent {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "canonical_torrents")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tracker: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub torrent_id: i64,
        pub group_id: i64,
        pub info_hash: Option<String>,
        #[sea_orm(column_type = "Json")]
        pub canonical_json: Json,
        pub fetched_at: String,
        pub expires_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod download_release_link {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "download_release_links")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub client: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub info_hash: String,
        pub announce_host: Option<String>,
        pub tracker: Option<String>,
        pub group_id: Option<i64>,
        pub torrent_id: Option<i64>,
        pub resolution_state: String,
        #[sea_orm(default_value = 0)]
        pub attempts: i64,
        pub next_retry_at: Option<String>,
        pub error_code: Option<String>,
        pub error_message: Option<String>,
        pub first_seen_at: String,
        pub last_seen_at: String,
        pub updated_at: String,
        #[sea_orm(default_value = true)]
        pub present: bool,
        pub missing_since: Option<String>,
        pub library_added_at: Option<String>,
        pub completed_at: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod canonical_release_artist {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "canonical_release_artists")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tracker: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub group_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub artist_key: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub role: String,
        pub artist_id: Option<i64>,
        pub name: String,
        pub sort_name: String,
        pub source: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod download_client_scan {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "download_client_scans")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub client: String,
        pub last_successful_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod runtime_preference {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "runtime_preferences")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        #[sea_orm(column_type = "Json")]
        pub value_json: Json,
        pub updated_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod release_track_index {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "release_track_indexes")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tracker: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub group_id: i64,
        #[sea_orm(default_value = "pending")]
        pub state: String,
        #[sea_orm(column_type = "Json", nullable)]
        pub index_json: Option<Json>,
        #[sea_orm(default_value = 0)]
        pub attempts: i64,
        pub next_retry_at: Option<String>,
        pub error_message: Option<String>,
        pub fetched_at: Option<String>,
        pub expires_at: Option<String>,
        pub updated_at: String,
        #[sea_orm(default_value = 0)]
        pub priority: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod dedupe_catalog_membership {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "dedupe_catalog_memberships")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tracker: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub artist_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub group_id: i64,
        #[sea_orm(column_type = "Json")]
        pub group_json: Json,
        pub updated_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod single_album_coverage {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "single_album_coverages")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tracker: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub single_group_id: i64,
        pub state: String,
        #[sea_orm(column_type = "Json", nullable)]
        pub coverage_json: Option<Json>,
        pub updated_at: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
