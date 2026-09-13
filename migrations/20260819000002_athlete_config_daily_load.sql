-- FTP/HR live here, not just in env vars, because the dashboard lets the
-- user edit them (e.g. after a future FTP retest). Seeded from env on first
-- run; every sync recomputes TSS/IF/zones for ALL activities against the
-- current value here, so an edit retroactively re-dates history.
CREATE TABLE athlete_config (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    ftp_watts   INTEGER NOT NULL,
    hr_max      INTEGER NOT NULL,
    hr_rest     INTEGER NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Cached daily training load (CTL/ATL/TSB), recomputed at every sync so the
-- dashboard's "current week" summary is a cheap read, not a recursive calc
-- on every page load.
CREATE TABLE daily_load (
    date TEXT PRIMARY KEY,
    tss  REAL NOT NULL DEFAULT 0,
    ctl  REAL NOT NULL,
    atl  REAL NOT NULL,
    tsb  REAL NOT NULL
);
