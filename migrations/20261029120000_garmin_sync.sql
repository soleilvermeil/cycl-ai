-- Origine d'une activité. Les id Strava restent la clé primaire (positifs).
-- Garmin et les imports fichier ont un id interne négatif : les id Garmin
-- sont aussi positifs et entreraient en collision avec Strava, et l'interface
-- traite un id positif comme un lien strava.com.
-- external_id = id d'activité Garmin quand source = 'garmin'.
ALTER TABLE activities ADD COLUMN source TEXT NOT NULL DEFAULT 'strava';
UPDATE activities SET source = 'file' WHERE id < 0;
ALTER TABLE activities ADD COLUMN external_id INTEGER;
CREATE UNIQUE INDEX activities_source_external
    ON activities(source, external_id) WHERE external_id IS NOT NULL;

ALTER TABLE settings ADD COLUMN garmin_api_key TEXT NOT NULL DEFAULT '';
-- 'strava' ou 'garmin' : une seule source est interrogée par la synchro.
ALTER TABLE settings ADD COLUMN sync_source TEXT NOT NULL DEFAULT 'strava';
