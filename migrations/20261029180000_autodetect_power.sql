-- Détection automatique (côtes répétées, lectures de sortie) et estimation
-- de puissance. Les deux sont désactivées tant que l'utilisateur ne les
-- active pas dans /settings.

ALTER TABLE settings ADD COLUMN autodetect_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE settings ADD COLUMN power_estimate_enabled INTEGER NOT NULL DEFAULT 0;

-- 0 = non renseigné. L'estimation refuse de tourner tant que les quatre
-- valeurs ne sont pas dans une plage plausible.
ALTER TABLE athlete_config ADD COLUMN rider_weight_kg REAL NOT NULL DEFAULT 0;
ALTER TABLE athlete_config ADD COLUMN bike_weight_kg REAL NOT NULL DEFAULT 0;
ALTER TABLE athlete_config ADD COLUMN cda REAL NOT NULL DEFAULT 0;
ALTER TABLE athlete_config ADD COLUMN crr REAL NOT NULL DEFAULT 0;

-- 'estimate' quand les watts ont été écrits par le modèle. NULL = fichier
-- d'origine (capteur, ou pas de puissance).
ALTER TABLE activities ADD COLUMN power_source TEXT;
-- Nom de séance lu dans le FIT (message workout), quand il y en a un.
ALTER TABLE activities ADD COLUMN workout_name TEXT;

ALTER TABLE laps ADD COLUMN lap_trigger TEXT;
ALTER TABLE laps ADD COLUMN intensity TEXT;

-- 'strava' (favori importé) ou 'auto' (côte répétée détectée ici).
ALTER TABLE reference_segments ADD COLUMN source TEXT NOT NULL DEFAULT 'strava';

-- Texte ajouté au bilan quand la détection automatique est activée.
CREATE TABLE activity_insights (
    activity_id INTEGER PRIMARY KEY REFERENCES activities(id) ON DELETE CASCADE,
    summary     TEXT NOT NULL
);
