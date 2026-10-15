-- hrTSS for running: an HR-based training stress score (TRIMP normalised to
-- one hour at threshold heart rate), stored in activities.tss next to the
-- power-based TSS of rides so both feed the same CTL/ATL/TSB.
--
-- lthr = lactate-threshold heart rate in bpm; 0 means "derive it from
-- HR rest/max" (see metrics::get_hr_config).
ALTER TABLE athlete_config ADD COLUMN lthr INTEGER NOT NULL DEFAULT 0;

-- Where an activity's tss comes from: 'power' (rides, NP-based) or 'hr' (runs).
ALTER TABLE activities ADD COLUMN tss_source TEXT;
UPDATE activities SET tss_source = 'power' WHERE tss IS NOT NULL;
