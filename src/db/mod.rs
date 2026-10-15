use crate::config::Config;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;

pub async fn connect(database_url: &str) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// Seeds `athlete_config` from env defaults on first run only — once the row
/// exists, env vars no longer take precedence (the dashboard owns it).
pub async fn seed_athlete_config(pool: &SqlitePool, config: &Config) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, ?, ?, ?)",
    )
    .bind(config.ftp_watts)
    .bind(config.hr_max)
    .bind(config.hr_rest)
    .execute(pool)
    .await?;
    Ok(())
}

/// Seeds `settings` from env defaults and the LLM prompts' hardcoded
/// defaults, on first run only — once the row exists, the /settings page
/// owns it and env vars no longer take precedence.
pub async fn seed_settings(pool: &SqlitePool, config: &Config) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT OR IGNORE INTO settings
            (id, strava_client_id, strava_client_secret, anthropic_api_key, athlete_profile,
             bilan_system_prompt, digest_system_prompt)
        VALUES (1, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&config.strava_client_id)
    .bind(&config.strava_client_secret)
    .bind(&config.anthropic_api_key)
    .bind(&config.athlete_profile)
    .bind(crate::llm::BILAN_DEFAULT_SYSTEM_PROMPT)
    .bind(crate::digest::DEFAULT_SYSTEM_PROMPT)
    .execute(pool)
    .await?;
    Ok(())
}

/// Seeds `sync_state`'s refresh token from env on first run — afterwards
/// Strava rotates it on every use and /settings can edit it directly.
pub async fn seed_sync_state(pool: &SqlitePool, config: &Config) -> anyhow::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO sync_state (id, last_sync_epoch, strava_refresh_token) VALUES (1, 0, ?)")
        .bind(&config.strava_refresh_token)
        .execute(pool)
        .await?;
    Ok(())
}

/// One-time backfill for activities stored before `route_thumb_svg` existed
/// (see migrations `route_thumb_cache` / `consolidation`): computes and caches
/// the thumbnail for any row that doesn't have one yet, so the dashboard
/// never has to parse a full GPS stream at request time. Run at every boot;
/// a no-op once every row is backfilled (sync/import populate it directly).
pub async fn backfill_route_thumbnails(pool: &SqlitePool) -> anyhow::Result<()> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT a.id, s.streams_json FROM activities a          JOIN activity_streams s ON s.activity_id = a.id WHERE a.route_thumb_svg IS NULL OR a.route_points_json IS NULL",
    )
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }
    tracing::info!(count = rows.len(), "backfilling route thumbnails");

    for (activity_id, streams_json) in rows {
        // Stored as '' rather than left NULL when there's no GPS data, so
        // this backfill (and the WHERE above) doesn't reprocess the same
        // GPS-less activity on every boot.
        let route_thumb_svg = crate::route_thumb::compute(&streams_json).unwrap_or_default();
        let route_points = crate::route_thumb::compute_points(&streams_json).unwrap_or_default();
        sqlx::query("UPDATE activities SET route_thumb_svg = ?, route_points_json = ? WHERE id = ?")
            .bind(route_thumb_svg)
            .bind(route_points)
            .bind(activity_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}
