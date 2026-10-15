-- Weight applied to hrTSS (running) when it is added to the total training
-- load (daily_load / CTL-ATL-TSB and the TSS totals). activities.tss keeps the
-- raw hrTSS; only the load aggregation multiplies it. 1.0 = counts like a
-- power TSS.
ALTER TABLE athlete_config ADD COLUMN hr_tss_coef REAL NOT NULL DEFAULT 1.0;

-- Sport of a session template / planned session: cycling | running | other.
ALTER TABLE workouts ADD COLUMN sport TEXT NOT NULL DEFAULT 'cycling';
ALTER TABLE planned_workouts ADD COLUMN sport TEXT NOT NULL DEFAULT 'cycling';
