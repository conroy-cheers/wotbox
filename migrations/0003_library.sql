ALTER TABLE download_release_links ADD COLUMN present INTEGER NOT NULL DEFAULT 1;
ALTER TABLE download_release_links ADD COLUMN missing_since TEXT;
ALTER TABLE download_release_links ADD COLUMN library_added_at TEXT;
ALTER TABLE download_release_links ADD COLUMN completed_at TEXT;

CREATE INDEX download_release_links_library
    ON download_release_links (library_added_at, tracker, group_id, torrent_id);

CREATE TABLE canonical_release_artists (
    tracker TEXT NOT NULL,
    group_id INTEGER NOT NULL,
    artist_key TEXT NOT NULL,
    artist_id INTEGER,
    name TEXT NOT NULL,
    sort_name TEXT NOT NULL,
    role TEXT NOT NULL,
    source TEXT NOT NULL,
    PRIMARY KEY (tracker, group_id, artist_key, role)
);

CREATE INDEX canonical_release_artists_browse
    ON canonical_release_artists (tracker, sort_name, artist_key);

CREATE TABLE download_client_scans (
    client TEXT PRIMARY KEY,
    last_successful_at TEXT NOT NULL
);
