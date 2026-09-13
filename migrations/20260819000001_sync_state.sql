-- Single-row sync bookkeeping. The refresh_token is persisted here (not just
-- read from env) because Strava rotates it on every use; the env var only
-- seeds the first run.
CREATE TABLE sync_state (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    last_sync_epoch     INTEGER NOT NULL DEFAULT 0,
    strava_refresh_token TEXT
);

CREATE TABLE activity_streams (
    activity_id INTEGER PRIMARY KEY REFERENCES activities(id),
    streams_json TEXT NOT NULL
);
