-- Running threshold pace in seconds per km (0 = not set). Reference for
-- placing pace-targeted steps on the intensity scale of the session chart and
-- of the TSS estimate: intensity ~ threshold pace / step pace.
ALTER TABLE athlete_config ADD COLUMN run_threshold_pace_s INTEGER NOT NULL DEFAULT 0;
