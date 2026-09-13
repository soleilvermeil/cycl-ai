-- CyclAI is scoped to road cycling training (README profile, TSS/zones,
-- puncheur analysis all assume road riding). Running, MTB/gravel, weight
-- training etc. from the same Strava account must never enter this table.
ALTER TABLE activities ADD COLUMN sport_type TEXT NOT NULL DEFAULT 'Ride';
