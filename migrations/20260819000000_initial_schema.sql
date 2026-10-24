-- Schéma complet de CyclAI.
--
-- Tables à une seule ligne (CHECK (id = 1)) : athlete_config, settings,
-- sync_state, dashboard_summary. Seedées au premier lancement (voir db/mod.rs).

CREATE TABLE activities (
    id                  INTEGER PRIMARY KEY,       -- Strava activity id
    started_at          TEXT NOT NULL,
    duration_s          INTEGER NOT NULL,
    distance_m          REAL NOT NULL,
    elevation_gain_m    REAL NOT NULL,
    avg_power_w         REAL,
    normalized_power_w  REAL,
    avg_hr              REAL,
    max_hr              REAL,
    tss                 REAL,
    tss_source          TEXT,                      -- 'power' (vélo, PN) | 'hr' (course, hrTSS)
    intensity_factor    REAL,
    zone1_s             INTEGER NOT NULL DEFAULT 0,
    zone2_s             INTEGER NOT NULL DEFAULT 0,
    zone3_s             INTEGER NOT NULL DEFAULT 0,
    zone4_s             INTEGER NOT NULL DEFAULT 0,
    zone5_s             INTEGER NOT NULL DEFAULT 0,
    zone6_s             INTEGER NOT NULL DEFAULT 0,
    zone7_s             INTEGER NOT NULL DEFAULT 0,
    sport_type          TEXT NOT NULL DEFAULT 'Ride',
    user_context        TEXT,
    title               TEXT,
    description         TEXT,
    route_thumb_svg     TEXT,                      -- '' = pas de GPS
    route_points_json   TEXT,                      -- [[lat,lng],...] ~150 points, '' = pas de GPS
    created_at          TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE activity_streams (
    activity_id  INTEGER PRIMARY KEY REFERENCES activities(id),
    streams_json TEXT NOT NULL
);

-- Records par activité : 'power' (key = durée en s, value = W) et
-- 'dist' (key = distance en m, value = meilleur temps en s).
CREATE TABLE activity_bests (
    activity_id INTEGER NOT NULL REFERENCES activities(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    key         INTEGER NOT NULL,
    value       REAL NOT NULL,
    PRIMARY KEY (activity_id, kind, key)
);

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

CREATE TABLE reference_segments (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    name             TEXT NOT NULL UNIQUE,
    start_lat        REAL NOT NULL,
    start_lng        REAL NOT NULL,
    end_lat          REAL NOT NULL,
    end_lng          REAL NOT NULL,
    distance_m       REAL NOT NULL,
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
    start_time_s            REAL,
    end_time_s              REAL,
    start_distance_m        REAL,
    end_distance_m          REAL,
    pr_activity_id          INTEGER REFERENCES activities(id),
    last_activity_id        INTEGER REFERENCES activities(id),
    created_at              TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE activity_bilans (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    activity_id  INTEGER NOT NULL REFERENCES activities(id),
    text         TEXT NOT NULL,
    generated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE digests (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    date                    TEXT NOT NULL DEFAULT (datetime('now')),
    free_text               TEXT,
    structured_summary_json TEXT NOT NULL,
    llm_response_json       TEXT NOT NULL,
    llm_response_text       TEXT NOT NULL
);

-- Modèles de séance : saisis à la main (/workouts) ou générés par le digest
-- (source = 'digest', digest_id renseigné). steps_json = JSON complet du Workout.
CREATE TABLE workouts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    steps_json  TEXT NOT NULL,
    source      TEXT NOT NULL DEFAULT 'manual',
    digest_id   INTEGER REFERENCES digests(id),
    sport       TEXT NOT NULL DEFAULT 'cycling',   -- cycling | running | other
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Séances planifiées (calendrier du dashboard). Chaque ligne possède ses
-- étapes ; workout_id indique seulement le modèle d'origine.
CREATE TABLE planned_workouts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    planned_date  TEXT NOT NULL,                       -- YYYY-MM-DD
    name          TEXT NOT NULL,
    steps_json    TEXT NOT NULL,
    notes         TEXT,
    workout_id    INTEGER REFERENCES workouts(id) ON DELETE SET NULL,
    source        TEXT NOT NULL DEFAULT 'manual',      -- manual | api
    sport         TEXT NOT NULL DEFAULT 'cycling',
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE sync_state (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    last_sync_epoch      INTEGER NOT NULL DEFAULT 0,
    strava_refresh_token TEXT                           -- Strava le fait tourner à chaque usage
);

-- FTP / FC / seuils éditables depuis /settings ; tout l'historique est
-- recalculé contre ces valeurs. lthr = 0 : dérivée de FC repos/max.
-- hr_tss_coef pondère l'hrTSS (course) dans la charge totale.
-- run_threshold_pace_s : allure seuil en s/km (0 = non renseignée).
CREATE TABLE athlete_config (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    ftp_watts            INTEGER NOT NULL,
    hr_max               INTEGER NOT NULL,
    hr_rest              INTEGER NOT NULL,
    lthr                 INTEGER NOT NULL DEFAULT 0,
    hr_tss_coef          REAL NOT NULL DEFAULT 1.0,
    run_threshold_pace_s INTEGER NOT NULL DEFAULT 0,
    updated_at           TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE settings (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    strava_client_id     TEXT NOT NULL DEFAULT '',
    strava_client_secret TEXT NOT NULL DEFAULT '',
    anthropic_api_key    TEXT NOT NULL DEFAULT '',
    athlete_profile      TEXT NOT NULL DEFAULT '',
    bilan_system_prompt  TEXT NOT NULL DEFAULT '',
    digest_system_prompt TEXT NOT NULL DEFAULT '',
    theme_color          TEXT NOT NULL DEFAULT '#2563eb',
    theme_mode           TEXT NOT NULL DEFAULT 'auto'  -- auto | light | dark
);

-- Charge quotidienne (CTL/ATL/TSB), recalculée à chaque sync.
CREATE TABLE daily_load (
    date TEXT PRIMARY KEY,
    tss  REAL NOT NULL DEFAULT 0,
    ctl  REAL NOT NULL,
    atl  REAL NOT NULL,
    tsb  REAL NOT NULL
);

-- Cache d'une ligne de la carte « 4 dernières semaines » du dashboard
-- (metrics::refresh_dashboard_summary).
CREATE TABLE dashboard_summary (
    id                      INTEGER PRIMARY KEY CHECK (id = 1),
    computed_for_date       TEXT NOT NULL,
    tss_period              REAL NOT NULL,
    ctl                     REAL NOT NULL,
    atl                     REAL NOT NULL,
    tsb                     REAL NOT NULL,
    zone1_s                 INTEGER NOT NULL,
    zone2_s                 INTEGER NOT NULL,
    zone3_s                 INTEGER NOT NULL,
    zone4_s                 INTEGER NOT NULL,
    zone5_s                 INTEGER NOT NULL,
    zone6_s                 INTEGER NOT NULL,
    zone7_s                 INTEGER NOT NULL,
    rest_days_to_next_band  INTEGER                     -- NULL : déjà dans la bande haute ou inatteignable
);

CREATE INDEX idx_activities_started_at ON activities(started_at);
CREATE INDEX idx_laps_activity ON laps(activity_id);
CREATE INDEX idx_segment_efforts_activity ON segment_efforts(activity_id);
CREATE INDEX idx_segment_efforts_segment ON segment_efforts(reference_segment_id);
CREATE INDEX idx_activity_bilans_activity ON activity_bilans(activity_id);
CREATE INDEX idx_planned_workouts_date ON planned_workouts(planned_date);
