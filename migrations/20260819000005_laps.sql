-- Manual/auto laps from Strava — how the athlete's device split a
-- structured workout (each interval), independent of our own local
-- segment matching.
CREATE TABLE laps (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    activity_id     INTEGER NOT NULL REFERENCES activities(id),
    lap_index       INTEGER NOT NULL,
    elapsed_time_s  INTEGER NOT NULL,
    distance_m      REAL NOT NULL,
    avg_power_w     REAL,
    avg_hr          REAL,
    max_hr          REAL
);

CREATE INDEX idx_laps_activity ON laps(activity_id);
