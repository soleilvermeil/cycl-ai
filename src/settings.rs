use sqlx::SqlitePool;

/// Runtime settings, DB-backed (see `db::seed_settings`) so they're editable
/// from the dashboard's /settings page instead of requiring a `.env` edit
/// and restart. `strava_refresh_token` is deliberately not here — it lives
/// in `sync_state` since Strava rotates it on every use (see `sync::run`).
#[derive(sqlx::FromRow, Clone)]
pub struct Settings {
    pub strava_client_id: String,
    pub strava_client_secret: String,
    pub anthropic_api_key: String,
    pub athlete_profile: String,
    pub bilan_system_prompt: String,
    pub digest_system_prompt: String,
    pub theme_color: String,
    pub theme_mode: String,
}

impl Settings {
    /// False when no Anthropic key is configured (blank or whitespace) — the
    /// UI then disables the LLM actions and offers the downloadable report.
    pub fn has_llm_key(&self) -> bool {
        !self.anthropic_api_key.trim().is_empty()
    }
}

pub async fn fetch(pool: &SqlitePool) -> anyhow::Result<Settings> {
    Ok(sqlx::query_as(
        "SELECT strava_client_id, strava_client_secret, anthropic_api_key, athlete_profile, \
         bilan_system_prompt, digest_system_prompt, theme_color, theme_mode FROM settings WHERE id = 1",
    )
    .fetch_one(pool)
    .await?)
}
