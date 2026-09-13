-- Structured workouts: hand-built via the /workouts builder page, or
-- generated as a side-effect of the weekly digest (source='digest',
-- digest_id set). steps_json holds the full Workout JSON (name/steps/
-- estimated_distance_km) — same untyped-JSON convention as digests'
-- llm_response_json, since a recursive step tree with nested repeats is a
-- poor fit for normalized tables.
CREATE TABLE workouts (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    name                TEXT NOT NULL,
    steps_json          TEXT NOT NULL,
    source              TEXT NOT NULL DEFAULT 'manual',
    digest_id           INTEGER REFERENCES digests(id),
    created_at          TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at          TEXT NOT NULL DEFAULT (datetime('now')),
    garmin_workout_id   TEXT,
    garmin_synced_at    TEXT,
    garmin_last_error   TEXT
);
