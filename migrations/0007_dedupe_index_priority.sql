ALTER TABLE release_track_indexes ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;

CREATE INDEX release_track_indexes_priority
    ON release_track_indexes (state, priority DESC, next_retry_at, updated_at);
