use crate::config::Config;
use crate::strava::StravaClient;
use chrono::Utc;
use sqlx::{FromRow, SqlitePool};

#[derive(FromRow)]
struct SyncState {
    last_sync_epoch: i64,
    strava_refresh_token: Option<String>,
}

/// Fetches new Strava activities since the last sync, persists them and
/// their raw streams, then runs deterministic metrics and segment matching.
/// LLM bilan generation is NOT triggered here — it's on-demand from the
/// activity detail page (`web::detail::regenerate_bilan`), so the user can
/// add optional context before the first analysis instead of paying for an
/// automatic one that ignores it.
pub async fn run(pool: &SqlitePool, config: &Config) -> anyhow::Result<()> {
    let state: Option<SyncState> = sqlx::query_as(
        "SELECT last_sync_epoch, strava_refresh_token FROM sync_state WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;

    let last_sync_epoch = state.as_ref().map(|s| s.last_sync_epoch).unwrap_or(0);
    let refresh_token = state
        .and_then(|s| s.strava_refresh_token)
        .unwrap_or_else(|| config.strava_refresh_token.clone());

    let client = StravaClient::authenticate(
        &config.strava_client_id,
        &config.strava_client_secret,
        &refresh_token,
    )
    .await?;

    let fetched = client.list_activities_after(last_sync_epoch).await?;
    tracing::info!(count = fetched.len(), "activities fetched from Strava");

    // CyclAI is scoped to road cycling — running, MTB/gravel, weight
    // training etc. from the same Strava account must never enter the
    // training-load/segment/bilan pipeline. sport_type (not the legacy
    // `type` field) is what actually distinguishes GravelRide from Ride.
    let activities: Vec<_> = fetched
        .into_iter()
        .filter(|a| a.sport_type == "Ride")
        .collect();
    tracing::info!(count = activities.len(), "road rides after filtering");

    for activity in &activities {
        let normalized_power = activity
            .weighted_average_watts
            .filter(|_| activity.device_watts.unwrap_or(false));
        let avg_power = activity
            .average_watts
            .filter(|_| activity.device_watts.unwrap_or(false));

        // Only the title is free with the summary list already fetched above;
        // the description needs its own per-activity detail call.
        let description = client.get_activity_description(activity.id).await?;

        sqlx::query(
            r#"
            INSERT INTO activities
                (id, started_at, sport_type, duration_s, distance_m, elevation_gain_m,
                 avg_power_w, normalized_power_w, avg_hr, max_hr, title, description)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (id) DO NOTHING
            "#,
        )
        .bind(activity.id)
        .bind(&activity.start_date)
        .bind(&activity.sport_type)
        .bind(activity.moving_time)
        .bind(activity.distance)
        .bind(activity.total_elevation_gain)
        .bind(avg_power)
        .bind(normalized_power)
        .bind(activity.average_heartrate)
        .bind(activity.max_heartrate)
        .bind(&activity.name)
        .bind(&description)
        .execute(pool)
        .await?;

        let streams = client.get_streams(activity.id).await?;
        let streams_json = streams.to_string();
        sqlx::query(
            r#"
            INSERT INTO activity_streams (activity_id, streams_json)
            VALUES (?, ?)
            ON CONFLICT (activity_id) DO UPDATE SET streams_json = excluded.streams_json
            "#,
        )
        .bind(activity.id)
        .bind(streams_json)
        .execute(pool)
        .await?;

        let laps = client.get_laps(activity.id).await?;
        for lap in &laps {
            let avg_power = lap.average_watts.filter(|_| lap.device_watts.unwrap_or(false));
            sqlx::query(
                r#"
                INSERT INTO laps (activity_id, lap_index, elapsed_time_s, distance_m, avg_power_w, avg_hr, max_hr)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(activity.id)
            .bind(lap.lap_index)
            .bind(lap.elapsed_time)
            .bind(lap.distance)
            .bind(avg_power)
            .bind(lap.average_heartrate)
            .bind(lap.max_heartrate)
            .execute(pool)
            .await?;
        }
    }

    let now = Utc::now().timestamp();
    sqlx::query(
        r#"
        INSERT INTO sync_state (id, last_sync_epoch, strava_refresh_token)
        VALUES (1, ?, ?)
        ON CONFLICT (id) DO UPDATE SET
            last_sync_epoch = excluded.last_sync_epoch,
            strava_refresh_token = excluded.strava_refresh_token
        "#,
    )
    .bind(now)
    .bind(&client.refresh_token)
    .execute(pool)
    .await?;

    crate::metrics::run(pool).await?;
    crate::segments::run(pool).await?;

    Ok(())
}
