use std::env;

/// Bootstrap configuration loaded from environment variables (`.env` in
/// dev), used only to seed the DB-backed `settings`/`athlete_config`/
/// `sync_state` tables on first boot. After that first run, the dashboard's
/// /settings page owns these values and env vars are no longer read —
/// fields below default to empty/zero rather than erroring so the app still
/// starts (and lets the user fill them in via /settings) even without a
/// populated .env.
#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub strava_client_id: String,
    pub strava_client_secret: String,
    pub strava_refresh_token: String,
    pub anthropic_api_key: String,
    /// Personal OpenAI key. Empty until the user creates one on platform.openai.com.
    pub openai_api_key: String,
    pub ftp_watts: u32,
    pub hr_max: u32,
    pub hr_rest: u32,
    pub athlete_profile: String,
    /// Personal API key. Empty until the user creates one on Garmin Connect.
    pub garmin_api_key: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        Ok(Self {
            database_url: env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://cyclai.db".into()),
            strava_client_id: env::var("STRAVA_CLIENT_ID").unwrap_or_default(),
            strava_client_secret: env::var("STRAVA_CLIENT_SECRET").unwrap_or_default(),
            strava_refresh_token: env::var("STRAVA_REFRESH_TOKEN").unwrap_or_default(),
            anthropic_api_key: env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            openai_api_key: env::var("OPENAI_API_KEY").unwrap_or_default(),
            ftp_watts: env::var("FTP_WATTS")
                .unwrap_or_else(|_| "200".into())
                .parse()?,
            hr_max: env::var("HR_MAX")
                .unwrap_or_else(|_| "190".into())
                .parse()?,
            hr_rest: env::var("HR_REST")
                .unwrap_or_else(|_| "50".into())
                .parse()?,
            athlete_profile: env::var("ATHLETE_PROFILE").unwrap_or_default(),
            garmin_api_key: env::var("GARMIN_API_KEY").unwrap_or_default(),
        })
    }
}
