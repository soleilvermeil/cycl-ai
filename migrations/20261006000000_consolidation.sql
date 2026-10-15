-- Consolidation: drop the unused Garmin push columns and the gym equipment
-- config, move the route thumbnail cache next to the data the dashboard
-- actually lists, and add a precomputed dashboard summary row.

-- Garmin Connect integration was never finished; only Strava sync and FIT/GPX
-- upload remain as activity sources.
ALTER TABLE workouts DROP COLUMN garmin_workout_id;
ALTER TABLE workouts DROP COLUMN garmin_synced_at;
ALTER TABLE workouts DROP COLUMN garmin_last_error;

-- Gym equipment module removed.
DROP TABLE gym_config;

-- Strip the gym wording out of an unedited seeded digest prompt (a prompt the
-- user already customised from /settings simply won't match and is left alone).
UPDATE settings
SET digest_system_prompt = REPLACE(
        REPLACE(
            REPLACE(digest_system_prompt,
                ', équipement salle disponible,', ','),
            ',' || char(10) || '  "gym_suggestion": "string ou null — séance de renfort complémentaire si pertinent selon la charge et le profil du cycliste, en utilisant seulement l''équipement disponible listé"', ''),
        ',' || char(13) || char(10) || '  "gym_suggestion": "string ou null — séance de renfort complémentaire si pertinent selon la charge et le profil du cycliste, en utilisant seulement l''équipement disponible listé"', '')
WHERE id = 1;

-- The dashboard lists the 10 latest activities with their route thumbnail.
-- Kept in activity_streams, the thumbnail sat behind the multi-hundred-KB
-- streams_json blob in the same row, which SQLite has to walk through
-- overflow pages to get past. On `activities` it's a plain small column.
ALTER TABLE activities ADD COLUMN route_thumb_svg TEXT;
UPDATE activities
SET route_thumb_svg = (SELECT s.route_thumb_svg FROM activity_streams s WHERE s.activity_id = activities.id);
ALTER TABLE activity_streams DROP COLUMN route_thumb_svg;

-- Single-row cache of everything the dashboard's "4 dernières semaines" card
-- shows, so a page load is a single-row read instead of aggregating over
-- activities/daily_load and simulating rest days. Rebuilt by
-- metrics::refresh_dashboard_summary after sync/import/FTP edit, and once per
-- day by the dashboard itself (the 28-day window slides with the date).
CREATE TABLE dashboard_summary (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    computed_for_date   TEXT NOT NULL,
    tss_period          REAL NOT NULL,
    ctl                 REAL NOT NULL,
    atl                 REAL NOT NULL,
    tsb                 REAL NOT NULL,
    zone1_s             INTEGER NOT NULL,
    zone2_s             INTEGER NOT NULL,
    zone3_s             INTEGER NOT NULL,
    zone4_s             INTEGER NOT NULL,
    zone5_s             INTEGER NOT NULL,
    zone6_s             INTEGER NOT NULL,
    zone7_s             INTEGER NOT NULL,
    rest_days_to_next_band INTEGER   -- NULL when already at the top band or unreachable
);
