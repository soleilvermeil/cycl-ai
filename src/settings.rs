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
    pub openai_api_key: String,
    /// `anthropic` or `openai`. Bilans and digests call only this provider.
    pub llm_provider: String,
    /// Responses API model id, used when `llm_provider` is `openai`.
    pub openai_model: String,
    pub athlete_profile: String,
    pub bilan_system_prompt: String,
    pub digest_system_prompt: String,
    pub theme_color: String,
    pub theme_mode: String,
    pub garmin_api_key: String,
    /// `strava` or `garmin`. The sync reads only this source.
    pub sync_source: String,
    /// Repeated climbs, interval blocks and peer comparisons. Off until set.
    pub autodetect_enabled: i64,
    /// Physics estimate for rides with no power meter. Off until set.
    pub power_estimate_enabled: i64,
}

impl Settings {
    /// False when the selected provider has no key — the UI then offers the
    /// downloadable report instead of calling the API.
    pub fn has_llm_key(&self) -> bool {
        let key = if self.llm_provider == "openai" {
            &self.openai_api_key
        } else {
            &self.anthropic_api_key
        };
        !key.trim().is_empty()
    }
}

pub async fn fetch(pool: &SqlitePool) -> anyhow::Result<Settings> {
    Ok(sqlx::query_as(
        "SELECT strava_client_id, strava_client_secret, anthropic_api_key, openai_api_key, \
         llm_provider, openai_model, athlete_profile, bilan_system_prompt, digest_system_prompt, \
         theme_color, theme_mode, garmin_api_key, sync_source, \
         autodetect_enabled, power_estimate_enabled FROM settings WHERE id = 1",
    )
    .fetch_one(pool)
    .await?)
}
