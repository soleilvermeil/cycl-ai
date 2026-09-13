use std::env;

/// Runtime configuration loaded from environment variables (`.env` in dev).
/// FTP and other athlete constants live here, not hardcoded, so they can be
/// updated after a future FTP retest without touching source.
#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub strava_client_id: String,
    pub strava_client_secret: String,
    pub strava_refresh_token: String,
    pub anthropic_api_key: String,
    pub ftp_watts: u32,
    pub hr_max: u32,
    pub hr_rest: u32,
    /// Free-form athlete profile (goals, terrain, equipment, physiology)
    /// re-injected into every LLM prompt alongside the deterministic
    /// deltas — kept out of source entirely since it's personal, unlike
    /// FTP/HR which double as calculation inputs.
    pub athlete_profile: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        Ok(Self {
            database_url: env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://cyclai.db".into()),
            strava_client_id: env::var("STRAVA_CLIENT_ID")?,
            strava_client_secret: env::var("STRAVA_CLIENT_SECRET")?,
            strava_refresh_token: env::var("STRAVA_REFRESH_TOKEN")?,
            anthropic_api_key: env::var("ANTHROPIC_API_KEY")?,
            ftp_watts: env::var("FTP_WATTS")
                .unwrap_or_else(|_| "200".into())
                .parse()?,
            hr_max: env::var("HR_MAX").unwrap_or_else(|_| "190".into()).parse()?,
            hr_rest: env::var("HR_REST").unwrap_or_else(|_| "50".into()).parse()?,
            athlete_profile: env::var("ATHLETE_PROFILE").unwrap_or_default(),
        })
    }
}
