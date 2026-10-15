-- Caches the dashboard's route-shape thumbnail SVG so it's computed once at
-- sync time (see sync::run) instead of being re-parsed from the full GPS
-- stream on every dashboard load, for every activity in the list.
ALTER TABLE activity_streams ADD COLUMN route_thumb_svg TEXT;
