-- Page Statistiques : traces simplifiées pour la carte globale et records
-- par activité (courbe de puissance, meilleurs temps sur distance).
ALTER TABLE activities ADD COLUMN route_points_json TEXT;           -- [[lat,lng],...] ~150 points, NULL = pas encore calculé
ALTER TABLE activities ADD COLUMN bests_computed INTEGER NOT NULL DEFAULT 0;

CREATE TABLE activity_bests (
    activity_id INTEGER NOT NULL REFERENCES activities(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,       -- 'power' : key = durée (s), value = W
                                     -- 'dist'  : key = distance (m), value = meilleur temps (s)
    key         INTEGER NOT NULL,
    value       REAL NOT NULL,
    PRIMARY KEY (activity_id, kind, key)
);
