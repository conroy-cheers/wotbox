CREATE TABLE release_track_indexes (
    tracker TEXT NOT NULL,
    group_id INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    index_json TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at TEXT,
    error_message TEXT,
    fetched_at TEXT,
    expires_at TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (tracker, group_id)
);

CREATE INDEX release_track_indexes_due
    ON release_track_indexes (state, next_retry_at, updated_at);

CREATE TABLE dedupe_catalog_memberships (
    tracker TEXT NOT NULL,
    artist_id INTEGER NOT NULL,
    group_id INTEGER NOT NULL,
    group_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (tracker, artist_id, group_id)
);

CREATE INDEX dedupe_catalog_memberships_group
    ON dedupe_catalog_memberships (tracker, group_id);

CREATE TABLE single_album_coverages (
    tracker TEXT NOT NULL,
    single_group_id INTEGER NOT NULL,
    state TEXT NOT NULL,
    coverage_json TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (tracker, single_group_id)
);
