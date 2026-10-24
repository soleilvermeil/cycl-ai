-- Clés publiques autorisées à appeler l'API exposée (`cyclai serve-api`).
-- `name` sert de `kid` dans l'en-tête du JWT RS256 que signe le client.
-- scope : 'read' (GET) | 'write' (GET + POST/DELETE, ex. planification).
CREATE TABLE api_keys (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT NOT NULL UNIQUE,
    public_key_pem  TEXT NOT NULL,
    scope           TEXT NOT NULL DEFAULT 'read' CHECK (scope IN ('read', 'write')),
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_at      TEXT,
    last_used_at    TEXT
);
