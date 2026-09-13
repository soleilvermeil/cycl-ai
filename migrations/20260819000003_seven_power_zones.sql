-- Switch from 5-zone to Coggan's extended 7-zone power model.
ALTER TABLE activities ADD COLUMN zone6_s INTEGER NOT NULL DEFAULT 0;
ALTER TABLE activities ADD COLUMN zone7_s INTEGER NOT NULL DEFAULT 0;
