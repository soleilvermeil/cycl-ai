-- Workouts scheduled on a given day (shown on the dashboard calendar).
--
-- Each row owns its steps (a snapshot of the full Workout JSON, same shape as
-- workouts.steps_json), so a planned session can be unique to its day. The
-- optional workout_id only records which template it was copied from; editing
-- or deleting the template never touches planned sessions (deleting it just
-- clears the link).
CREATE TABLE planned_workouts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    planned_date  TEXT NOT NULL,                       -- YYYY-MM-DD
    name          TEXT NOT NULL,
    steps_json    TEXT NOT NULL,
    notes         TEXT,
    workout_id    INTEGER REFERENCES workouts(id) ON DELETE SET NULL,
    source        TEXT NOT NULL DEFAULT 'manual',      -- manual | api
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_planned_workouts_date ON planned_workouts(planned_date);
