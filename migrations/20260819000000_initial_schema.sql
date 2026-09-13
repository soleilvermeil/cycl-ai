CREATE TABLE activities (
    id              INTEGER PRIMARY KEY,       -- Strava activity id
    started_at      TEXT NOT NULL,
    duration_s      INTEGER NOT NULL,
    distance_m      REAL NOT NULL,
    elevation_gain_m REAL NOT NULL,
    avg_power_w     REAL,
    normalized_power_w REAL,
    avg_hr          REAL,
    max_hr          REAL,
    tss             REAL,
    intensity_factor REAL,
    zone1_s         INTEGER NOT NULL DEFAULT 0,
    zone2_s         INTEGER NOT NULL DEFAULT 0,
    zone3_s         INTEGER NOT NULL DEFAULT 0,
    zone4_s         INTEGER NOT NULL DEFAULT 0,
    zone5_s         INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE reference_segments (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE,
    start_lat   REAL NOT NULL,
    start_lng   REAL NOT NULL,
    end_lat     REAL NOT NULL,
    end_lng     REAL NOT NULL,
    distance_m  REAL NOT NULL,
    elevation_gain_m REAL NOT NULL
);

CREATE TABLE segment_efforts (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    activity_id             INTEGER NOT NULL REFERENCES activities(id),
    reference_segment_id    INTEGER NOT NULL REFERENCES reference_segments(id),
    avg_power_w             REAL,
    avg_speed_ms            REAL,
    avg_hr                  REAL,
    delta_vs_pr_pct         REAL,
    delta_vs_recent_avg_pct REAL,
    delta_vs_last_pct       REAL,
    created_at              TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE activity_bilans (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    activity_id INTEGER NOT NULL REFERENCES activities(id),
    text        TEXT NOT NULL,
    generated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE digests (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    date            TEXT NOT NULL DEFAULT (datetime('now')),
    free_text       TEXT,
    structured_summary_json TEXT NOT NULL,
    llm_response_json TEXT NOT NULL,
    llm_response_text TEXT NOT NULL
);

CREATE TABLE gym_config (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    equipment_json TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX idx_segment_efforts_activity ON segment_efforts(activity_id);
CREATE INDEX idx_segment_efforts_segment ON segment_efforts(reference_segment_id);
CREATE INDEX idx_activity_bilans_activity ON activity_bilans(activity_id);
CREATE INDEX idx_activities_started_at ON activities(started_at);
