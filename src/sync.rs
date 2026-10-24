use crate::strava::StravaClient;
use chrono::Utc;
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};
use std::sync::Mutex;

/// What the running sync is doing right now, polled by the dashboard
/// (`GET /sync/status`) so a slow sync (e.g. on a NAS) isn't a blank wait.
#[derive(Clone, Serialize)]
pub struct Progress {
    pub running: bool,
    pub step: String,
    pub done: usize,
    pub total: usize,
    pub error: Option<String>,
}

static PROGRESS: Mutex<Progress> = Mutex::new(Progress {
    running: false,
    step: String::new(),
    done: 0,
    total: 0,
    error: None,
});

pub fn progress() -> Progress {
    PROGRESS.lock().unwrap().clone()
}

fn set_step(step: impl Into<String>, done: usize, total: usize) {
    let mut p = PROGRESS.lock().unwrap();
    p.step = step.into();
    p.done = done;
    p.total = total;
}

/// Marks a sync as started; false if one is already running.
fn try_start() -> bool {
    let mut p = PROGRESS.lock().unwrap();
    if p.running {
        return false;
    }
    *p = Progress { running: true, step: "Démarrage".into(), done: 0, total: 0, error: None };
    true
}

fn finish(result: &anyhow::Result<()>) {
    let mut p = PROGRESS.lock().unwrap();
    p.running = false;
    p.error = result.as_ref().err().map(|e| format!("{e:#}"));
    p.step = if p.error.is_some() { "Échec".into() } else { "Terminé".into() };
}

/// Runs a sync in the background for the web UI; returns immediately.
/// No-op if one is already running.
pub fn spawn(pool: SqlitePool, full: bool) {
    if !try_start() {
        return;
    }
    tokio::spawn(async move {
        let result = run_inner(&pool, full).await;
        if let Err(err) = &result {
            tracing::error!(?err, "sync failed");
        }
        finish(&result);
    });
}

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
pub async fn run(pool: &SqlitePool) -> anyhow::Result<()> {
    run_inner(pool, false).await
}

/// Like `run`, but ignores the stored last-sync timestamp and walks the whole
/// Strava history, importing only activities not stored yet (e.g. runs, which
/// were filtered out before running was supported). Strava's rate limit
/// (100 requests / 15 min, ~3 requests per imported activity) can cut a big
/// backfill short — the next call simply resumes with what's still missing.
pub async fn run_full(pool: &SqlitePool) -> anyhow::Result<()> {
    run_inner(pool, true).await
}

async fn run_inner(pool: &SqlitePool, full: bool) -> anyhow::Result<()> {
    set_step("Chargement de la configuration", 0, 0);
    let settings = crate::settings::fetch(pool).await?;

    let state: Option<SyncState> = sqlx::query_as(
        "SELECT last_sync_epoch, strava_refresh_token FROM sync_state WHERE id = 1",
    )
    .fetch_optional(pool)
    .await?;

    let last_sync_epoch = if full { 0 } else { state.as_ref().map(|s| s.last_sync_epoch).unwrap_or(0) };
    let refresh_token = state.and_then(|s| s.strava_refresh_token).unwrap_or_default();

    set_step("Authentification Strava", 0, 0);
    let client = StravaClient::authenticate(
        &settings.strava_client_id,
        &settings.strava_client_secret,
        &refresh_token,
    )
    .await?;

    set_step("Récupération de la liste des activités Strava", 0, 0);
    let fetched = client.list_activities_after(last_sync_epoch).await?;
    tracing::info!(count = fetched.len(), "activities fetched from Strava");

    // CyclAI covers road and indoor cycling and running — MTB/gravel, weight
    // training etc. from the same Strava account must never enter the pipeline.
    // sport_type (not the legacy `type` field) is what actually distinguishes
    // GravelRide from Ride.
    let activities: Vec<_> = fetched
        .into_iter()
        .filter(|a| crate::metrics::is_supported_sport(&a.sport_type))
        .collect();
    tracing::info!(count = activities.len(), "road rides and runs after filtering");

    let total = activities.len();
    let mut imported_ids: Vec<i64> = Vec::new();
    for (i, activity) in activities.iter().enumerate() {
        let label = format!("Activité {}/{} « {} »", i + 1, total, activity.name);
        // Already fully imported (activity + streams): nothing to fetch.
        let already_imported: Option<(i64,)> =
            sqlx::query_as("SELECT activity_id FROM activity_streams WHERE activity_id = ?")
                .bind(activity.id)
                .fetch_optional(pool)
                .await?;
        if already_imported.is_some() {
            continue;
        }

        imported_ids.push(activity.id);

        // Power-based metrics (NP/TSS/IF/zones) are cycling-only and relative
        // to the cycling FTP — a run's power (e.g. a foot pod) is ignored.
        let is_ride = crate::metrics::is_ride(&activity.sport_type);
        let normalized_power = activity
            .weighted_average_watts
            .filter(|_| is_ride && activity.device_watts.unwrap_or(false));
        let avg_power = activity
            .average_watts
            .filter(|_| is_ride && activity.device_watts.unwrap_or(false));

        // Only the title is free with the summary list already fetched above;
        // the description needs its own per-activity detail call.
        set_step(format!("{label} : description"), i, total);
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

        set_step(format!("{label} : téléchargement des streams"), i, total);
        let streams = client.get_streams(activity.id).await?;
        set_step(format!("{label} : traitement des données"), i, total);
        let streams_json = streams.to_string();
        let streams_json_for_points = streams_json.clone();
        // '' (not NULL) when there's no GPS data.
        let route_thumb_svg = crate::route_thumb::compute(&streams_json).unwrap_or_default();
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
        let route_points = crate::route_thumb::compute_points(&streams_json_for_points).unwrap_or_default();
        sqlx::query("UPDATE activities SET route_thumb_svg = ?, route_points_json = ? WHERE id = ?")
            .bind(route_thumb_svg)
            .bind(route_points)
            .bind(activity.id)
            .execute(pool)
            .await?;
        crate::bests::store(pool, activity.id, !is_ride, &streams).await?;

        set_step(format!("{label} : tours"), i, total);
        let laps = client.get_laps(activity.id).await?;
        sqlx::query("DELETE FROM laps WHERE activity_id = ?")
            .bind(activity.id)
            .execute(pool)
            .await?;
        for lap in &laps {
            let avg_power = lap.average_watts.filter(|_| is_ride && lap.device_watts.unwrap_or(false));
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

    set_step("Enregistrement de l'état de synchronisation", total, total);
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

    set_step("Calcul des métriques (TSS, CTL/ATL, zones)", total, total);
    // Only the activities imported just now need their streams re-parsed;
    // FTP/HR setting changes trigger the full recompute themselves.
    crate::metrics::run_incremental(pool, &imported_ids).await?;
    // The segment replay re-parses every stored ride, so it only runs when
    // something new was imported (`import-segments` runs it itself too).
    if imported_ids.is_empty() {
        set_step("Détection des segments (rien de nouveau, ignorée)", total, total);
    } else {
        set_step("Détection des segments", total, total);
        crate::segments::run(pool).await?;
    }

    Ok(())
}
