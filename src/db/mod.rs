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

/// Seeds the single `gym_config` row (empty equipment list) so the digest
/// pipeline always has a row to read, even before the user configures it
/// via the dashboard.
pub async fn seed_gym_config(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO gym_config (id, equipment_json) VALUES (1, '[]')")
        .execute(pool)
        .await?;
    Ok(())
}
