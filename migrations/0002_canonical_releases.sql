CREATE TABLE canonical_torrents (
    tracker TEXT NOT NULL,
    torrent_id INTEGER NOT NULL,
    group_id INTEGER NOT NULL,
    info_hash TEXT,
    canonical_json TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (tracker, torrent_id),
    UNIQUE (tracker, info_hash)
);

CREATE INDEX canonical_torrents_group
    ON canonical_torrents (tracker, group_id);

CREATE TABLE download_release_links (
    client TEXT NOT NULL,
    info_hash TEXT NOT NULL,
    announce_host TEXT,
    tracker TEXT,
    group_id INTEGER,
    torrent_id INTEGER,
    resolution_state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at TEXT,
    error_code TEXT,
    error_message TEXT,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (client, info_hash)
);

CREATE INDEX download_release_links_resolution
    ON download_release_links (resolution_state, next_retry_at);

CREATE INDEX download_release_links_release
    ON download_release_links (tracker, group_id, torrent_id);

-- Version 1 stored raw tracker group JSON under this key. The public group
-- contract is now normalized and intentionally incompatible with that shape.
DELETE FROM tracker_snapshots WHERE resource_kind = 'group';
