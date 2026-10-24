use crate::config::Config;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;

pub async fn connect(database_url: &str) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// Last migration of the pre-squash history (the one that added
/// `activity_bests`). A database that has it applied has the exact same
/// schema (modulo a leftover `activities.bests_computed` column) as the
/// single initial migration.
const LAST_SQUASHED_VERSION: i64 = 20261012000000;
/// The single migration that replaced the pre-squash history.
const INITIAL_VERSION: i64 = 20260819000000;

pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    let migrator = sqlx::migrate!("./migrations");
    adopt_squashed_history(pool, &migrator).await?;
    migrator.run(pool).await?;
    Ok(())
}

/// One-time bridge for databases created before the 20 historical migrations
/// were squashed into `initial_schema`: their `_sqlx_migrations` table lists
/// versions that no longer exist (and a different checksum for the initial
/// one), which sqlx rejects. When that table shows the full old history,
/// replace it with a single row for the initial migration; the schema is
/// already in place. Fresh databases and already-adopted ones are untouched.
/// Safe to delete once no pre-squash database remains.
async fn adopt_squashed_history(pool: &SqlitePool, migrator: &sqlx::migrate::Migrator) -> anyhow::Result<()> {
    let has_table: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'")
            .fetch_one(pool)
            .await?;
    if has_table == 0 {
        return Ok(());
    }
    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(pool)
        .await?;
    let known = |v: &i64| migrator.migrations.iter().any(|m| m.version == *v);
    if applied.iter().all(known) {
        return Ok(());
    }
    if !applied.contains(&LAST_SQUASHED_VERSION) {
        anyhow::bail!(
            "database was created by an older CyclAI build that is not fully migrated; \
             run that build once to finish its migrations, or recreate the database"
        );
    }
    let initial = migrator
        .migrations
        .iter()
        .find(|m| m.version == INITIAL_VERSION)
        .ok_or_else(|| anyhow::anyhow!("no embedded migration found"))?;
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM _sqlx_migrations").execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (?, ?, 1, ?, 0)",
    )
    .bind(initial.version)
    .bind(initial.description.as_ref())
    .bind(initial.checksum.as_ref())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!("adopted pre-squash migration history");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pre_squash_database_is_adopted_and_fresh_one_is_untouched() {
        let pool = SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
        migrate(&pool).await.unwrap();

        // Simulate the old history: 20 rows, wrong checksums, last one present,
        // and no api_keys table yet.
        sqlx::query("DROP TABLE api_keys").execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations").execute(&pool).await.unwrap();
        for i in 0..19i64 {
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, 'old', 1, x'00', 0)",
            )
            .bind(20260819000001 + i)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, 'old', 1, x'00', 0)",
        )
        .bind(LAST_SQUASHED_VERSION)
        .execute(&pool)
        .await
        .unwrap();
        assert!(sqlx::migrate!("./migrations").run(&pool).await.is_err());

        migrate(&pool).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations").fetch_one(&pool).await.unwrap();
        assert_eq!(n, 2); // initial (adopted) + api_keys (applied after)
        migrate(&pool).await.unwrap(); // idempotent
    }

    #[tokio::test]
    async fn partially_migrated_old_database_is_rejected() {
        let pool = SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
        migrate(&pool).await.unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version > 20260819000000").execute(&pool).await.unwrap();
        sqlx::query("UPDATE _sqlx_migrations SET version = 20260901000000").execute(&pool).await.unwrap();
        assert!(migrate(&pool).await.is_err());
    }
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
