PRAGMA foreign_keys = ON;

CREATE TABLE tracker_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tracker TEXT NOT NULL,
    resource_kind TEXT NOT NULL,
    resource_key TEXT NOT NULL,
    normalized_json TEXT NOT NULL,
    sanitized_raw_json TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    UNIQUE (tracker, resource_kind, resource_key)
);

CREATE INDEX tracker_snapshots_expiry
    ON tracker_snapshots (tracker, resource_kind, expires_at);

CREATE TABLE download_jobs (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT,
    tracker TEXT NOT NULL,
    torrent_id INTEGER NOT NULL,
    group_id INTEGER,
    profile TEXT NOT NULL,
    use_token INTEGER NOT NULL DEFAULT 0,
    info_hash TEXT,
    name TEXT,
    state TEXT NOT NULL,
    progress REAL NOT NULL DEFAULT 0,
    download_speed INTEGER NOT NULL DEFAULT 0,
    upload_speed INTEGER NOT NULL DEFAULT 0,
    eta INTEGER,
    error_code TEXT,
    error_message TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (tracker, torrent_id, profile),
    UNIQUE (idempotency_key)
);

CREATE TABLE download_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES download_jobs(id) ON DELETE CASCADE,
    state TEXT NOT NULL,
    detail TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX download_events_job ON download_events (job_id, created_at);
