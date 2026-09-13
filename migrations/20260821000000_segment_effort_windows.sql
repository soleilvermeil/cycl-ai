ALTER TABLE segment_efforts ADD COLUMN start_time_s REAL;
ALTER TABLE segment_efforts ADD COLUMN end_time_s REAL;
ALTER TABLE segment_efforts ADD COLUMN pr_activity_id INTEGER REFERENCES activities(id);
ALTER TABLE segment_efforts ADD COLUMN last_activity_id INTEGER REFERENCES activities(id);
