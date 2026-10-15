-- Apparence : couleur d'accent de l'interface et mode clair / sombre.
ALTER TABLE settings ADD COLUMN theme_color TEXT NOT NULL DEFAULT '#2563eb';
ALTER TABLE settings ADD COLUMN theme_mode  TEXT NOT NULL DEFAULT 'auto';  -- auto | light | dark
