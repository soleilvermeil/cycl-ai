-- Runtime settings editable from the dashboard's /settings page, so the
-- Anthropic key, Strava app credentials, athlete profile and the two LLM
-- system prompts can be changed without touching the .env file or
-- redeploying. Seeded once from env vars (and the prompts' hardcoded
-- defaults) on first boot, same pattern as athlete_config/gym_config —
-- afterwards this table is the source of truth, not the environment.
CREATE TABLE settings (
    id                      INTEGER PRIMARY KEY CHECK (id = 1),
    strava_client_id        TEXT NOT NULL DEFAULT '',
    strava_client_secret    TEXT NOT NULL DEFAULT '',
    anthropic_api_key       TEXT NOT NULL DEFAULT '',
    athlete_profile         TEXT NOT NULL DEFAULT '',
    bilan_system_prompt     TEXT NOT NULL DEFAULT '',
    digest_system_prompt    TEXT NOT NULL DEFAULT ''
);
