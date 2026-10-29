//! JSON API (`/api/v1/...`) over the data the dashboard shows, for external
//! tools (home dashboards, scripts, other assistants). Everything is read-only
//! except scheduling: `POST/DELETE /planned` put sessions on the calendar.
//! Served by `web::serve_api` only, behind the RS256 bearer-token guard in
//! `web::auth`.

use super::detail::{HR_ZONE_BOUNDS, HR_ZONE_NAMES};
use super::{AppState, TSB_ZONES, ZONE_BOUNDS, ZONE_NAMES, tsb_zone_index};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::SqlitePool;

pub(super) struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        tracing::error!(?err, "api request failed");
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal error".to_string(),
        )
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        anyhow::Error::from(err).into()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn round(v: f64, digits: i32) -> f64 {
    let p = 10f64.powi(digits);
    // `+ 0.0` turns -0.0 (a tiny negative rounded away) into plain 0.0.
    (v * p).round() / p + 0.0
}

/// ISO date `days` days before today (UTC) — `started_at` is ISO-8601, so a
/// plain string comparison against it is exact and can use the index.
fn days_ago(days: i64) -> String {
    (chrono::Utc::now().date_naive() - chrono::Duration::days(days)).to_string()
}

/// `GET /api/v1/profile` — FTP/HR settings with the derived zone tables, plus
/// the current training load (CTL/ATL/TSB) from the precomputed summary.
pub(super) async fn profile(State(state): State<AppState>) -> ApiResult {
    let (ftp, hr_max, hr_rest): (i64, i64, i64) =
        sqlx::query_as("SELECT ftp_watts, hr_max, hr_rest FROM athlete_config WHERE id = 1")
            .fetch_one(&state.pool)
            .await?;
    let load = crate::metrics::dashboard_summary(&state.pool).await?;
    let hr_cfg = crate::metrics::get_hr_config(&state.pool).await?;
    let ath = crate::workout::Athlete::load(&state.pool).await;

    let power_zones: Vec<Value> = (0..7)
        .map(|i| {
            let high = ZONE_BOUNDS[i + 1];
            json!({
                "zone": i + 1,
                "name": ZONE_NAMES[i],
                "min_w": (ZONE_BOUNDS[i] * ftp as f64).round(),
                "max_w": if high.is_finite() { json!((high * ftp as f64).round()) } else { Value::Null },
            })
        })
        .collect();

    let hrr = (hr_max - hr_rest) as f64;
    let hr_zones: Vec<Value> = (0..5)
        .map(|i| {
            let high = HR_ZONE_BOUNDS[i + 1];
            json!({
                "zone": i + 1,
                "name": HR_ZONE_NAMES[i],
                "min_bpm": (hr_rest as f64 + HR_ZONE_BOUNDS[i] * hrr).round(),
                "max_bpm": if high.is_finite() { json!((hr_rest as f64 + high * hrr).round()) } else { Value::Null },
            })
        })
        .collect();

    let total_zone_s: i64 = load.zone_secs.iter().sum();
    Ok(Json(json!({
        "ftp_watts": ftp,
        "hr_max": hr_max,
        "hr_rest": hr_rest,
        "hr_threshold_bpm": hr_cfg.lthr.round(),
        "hr_tss_coefficient": hr_cfg.tss_coef,
        "run_threshold_pace": if ath.run_threshold_pace_s > 0.0 { Some(super::format::format_pace_secs(ath.run_threshold_pace_s)) } else { None },
        "run_threshold_pace_s_per_km": if ath.run_threshold_pace_s > 0.0 { Some(ath.run_threshold_pace_s) } else { None },
        "power_zones": power_zones,
        "hr_zones": hr_zones,
        "load": {
            "ctl": round(load.ctl, 1),
            "atl": round(load.atl, 1),
            "tsb": round(load.tsb, 1),
            "tsb_status": TSB_ZONES[tsb_zone_index(load.tsb)].1,
            "rest_days_to_next_tsb_band": load.rest_days_to_next_band,
            "tss_28d": round(load.tss_period, 0),
            "power_zone_seconds_28d": load.zone_secs,
            "power_zone_pct_28d": load.zone_secs.iter()
                .map(|s| if total_zone_s > 0 { round(*s as f64 / total_zone_s as f64 * 100.0, 1) } else { 0.0 })
                .collect::<Vec<_>>(),
        },
    })))
}

#[derive(Deserialize)]
pub(super) struct LoadQuery {
    days: Option<i64>,
}

/// `GET /api/v1/load?days=90` — daily TSS/CTL/ATL/TSB series, oldest first.
pub(super) async fn load(State(state): State<AppState>, Query(q): Query<LoadQuery>) -> ApiResult {
    let days = q.days.unwrap_or(90).clamp(1, 1500);
    // Goes through the summary first so daily_load is topped up to today
    // (a rest streak since the last sync is part of the series).
    crate::metrics::dashboard_summary(&state.pool).await?;
    let rows: Vec<(String, f64, f64, f64, f64)> = sqlx::query_as(
        "SELECT date, tss, ctl, atl, tsb FROM daily_load WHERE date >= ? ORDER BY date",
    )
    .bind(days_ago(days - 1))
    .fetch_all(&state.pool)
    .await?;
    let series: Vec<Value> = rows
        .into_iter()
        .map(|(date, tss, ctl, atl, tsb)| {
            json!({ "date": date, "tss": round(tss, 1), "ctl": round(ctl, 1), "atl": round(atl, 1), "tsb": round(tsb, 1) })
        })
        .collect();
    Ok(Json(json!({ "days": days, "series": series })))
}

#[derive(sqlx::FromRow)]
struct ActivityRecord {
    id: i64,
    sport_type: String,
    started_at: String,
    title: Option<String>,
    distance_m: f64,
    duration_s: i64,
    elevation_gain_m: f64,
    avg_power_w: Option<f64>,
    normalized_power_w: Option<f64>,
    avg_hr: Option<f64>,
    max_hr: Option<f64>,
    tss: Option<f64>,
    tss_source: Option<String>,
    intensity_factor: Option<f64>,
    zone1_s: i64,
    zone2_s: i64,
    zone3_s: i64,
    zone4_s: i64,
    zone5_s: i64,
    zone6_s: i64,
    zone7_s: i64,
}

const ACTIVITY_COLUMNS: &str = "id, sport_type, started_at, title, distance_m, duration_s, elevation_gain_m, \
     avg_power_w, normalized_power_w, avg_hr, max_hr, tss, tss_source, intensity_factor, \
     zone1_s, zone2_s, zone3_s, zone4_s, zone5_s, zone6_s, zone7_s";

/// `coef` = the hrTSS weight (see `metrics::HrConfig::tss_coef`): `tss` stays
/// the activity's own score, `tss_load` is what it adds to the total load.
fn activity_json(a: &ActivityRecord, coef: f64) -> Value {
    let is_run = crate::metrics::is_run(&a.sport_type);
    let km = a.distance_m / 1000.0;
    // Runs are expressed as pace (min/km), rides as speed (km/h).
    let pace = if is_run {
        super::format::format_pace(a.distance_m, a.duration_s)
    } else {
        None
    };
    let speed = if !is_run && a.duration_s > 0 {
        Some(round(km / (a.duration_s as f64 / 3600.0), 1))
    } else {
        None
    };
    json!({
        "id": a.id,
        "sport": if is_run { "run" } else { "ride" },
        "sport_type": a.sport_type,
        "started_at": a.started_at,
        "title": a.title,
        "distance_km": round(km, 2),
        "duration_s": a.duration_s,
        "elevation_gain_m": a.elevation_gain_m.round(),
        "avg_pace_min_per_km": pace,
        "avg_speed_kmh": speed,
        "avg_power_w": a.avg_power_w.map(|v| v.round()),
        "normalized_power_w": a.normalized_power_w.map(|v| v.round()),
        "avg_hr": a.avg_hr.map(|v| v.round()),
        "max_hr": a.max_hr.map(|v| v.round()),
        "tss": a.tss.map(|v| round(v, 0)),
        "tss_source": a.tss_source,
        "tss_load": a.tss.map(|v| round(if a.tss_source.as_deref() == Some("hr") { v * coef } else { v }, 0)),
        "intensity_factor": a.intensity_factor.map(|v| round(v, 2)),
        "power_zone_seconds": [a.zone1_s, a.zone2_s, a.zone3_s, a.zone4_s, a.zone5_s, a.zone6_s, a.zone7_s],
    })
}

#[derive(Deserialize)]
pub(super) struct ActivitiesQuery {
    limit: Option<i64>,
    days: Option<i64>,
    /// `ride` or `run`; anything else (or absent) means both.
    sport: Option<String>,
}

/// `GET /api/v1/activities?limit=10&days=28&sport=run` — latest activities,
/// newest first.
pub(super) async fn activities(
    State(state): State<AppState>,
    Query(q): Query<ActivitiesQuery>,
) -> ApiResult {
    let limit = q.limit.unwrap_or(10).clamp(1, 200);
    // No `days` means no lower bound: every started_at sorts after "".
    let since = q
        .days
        .map(|d| days_ago(d.clamp(1, 3650)))
        .unwrap_or_default();
    let sport_filter = match q.sport.as_deref() {
        Some("ride") => "AND sport_type IN ('Ride', 'VirtualRide')",
        Some("run") => "AND sport_type IN ('Run', 'TrailRun')",
        _ => "",
    };
    let rows: Vec<ActivityRecord> = sqlx::query_as(&format!(
        "SELECT {ACTIVITY_COLUMNS} FROM activities WHERE started_at >= ? {sport_filter} \
         ORDER BY started_at DESC LIMIT ?"
    ))
    .bind(since)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;
    let coef = crate::metrics::get_hr_config(&state.pool).await?.tss_coef;
    let items: Vec<Value> = rows.iter().map(|r| activity_json(r, coef)).collect();
    Ok(Json(json!({ "count": items.len(), "activities": items })))
}

/// `GET /api/v1/activities/{id}` — one activity with its laps, local segment
/// efforts and the generated bilan (if any).
pub(super) async fn activity(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult {
    let row: Option<ActivityRecord> = sqlx::query_as(&format!(
        "SELECT {ACTIVITY_COLUMNS} FROM activities WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(row) = row else {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("activity {id} not found"),
        ));
    };
    let coef = crate::metrics::get_hr_config(&state.pool).await?.tss_coef;
    let mut out = activity_json(&row, coef);
    let is_run = crate::metrics::is_run(&row.sport_type);

    let laps: Vec<(i64, i64, f64, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT lap_index, elapsed_time_s, distance_m, avg_power_w, avg_hr FROM laps \
         WHERE activity_id = ? ORDER BY lap_index",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;

    // Raw streams give per-lap and whole-ride detail beyond what Strava's
    // lap summary carries; without them (rare) laps fall back to the stored
    // summary values and `performance` is null.
    let ath = current_athlete(&state.pool).await?;
    let series = load_series(&state.pool, id, is_run).await?;

    let mut cursor_s = 0usize;
    let mut lap_items = Vec::with_capacity(laps.len());
    for (index, elapsed_s, distance_m, power, hr) in laps {
        let start = cursor_s;
        cursor_s += elapsed_s.max(0) as usize;
        let stats = series
            .as_ref()
            .map(|s| crate::performance::window_stats(s, start, cursor_s, &ath));
        lap_items.push(lap_json(
            index,
            start,
            elapsed_s,
            distance_m,
            power,
            hr,
            stats.as_ref(),
            is_run,
            ath.ftp_watts,
        ));
    }
    out["laps"] = Value::Array(lap_items);
    out["performance"] = match &series {
        Some(s) => performance_json(s, is_run, &ath),
        None => Value::Null,
    };

    let efforts: Vec<(String, f64, Option<f64>, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT rs.name, se.avg_power_w, se.delta_vs_pr_pct, se.delta_vs_recent_avg_pct, se.delta_vs_last_pct \
         FROM segment_efforts se JOIN reference_segments rs ON rs.id = se.reference_segment_id \
         WHERE se.activity_id = ?",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    out["segment_efforts"] = Value::Array(
        efforts
            .into_iter()
            .map(|(name, power, vs_pr, vs_recent, vs_last)| {
                json!({
                    "segment": name,
                    "avg_power_w": round(power, 0),
                    "delta_vs_pr_pct": vs_pr.map(|v| round(v, 1)),
                    "delta_vs_recent_avg_pct": vs_recent.map(|v| round(v, 1)),
                    "delta_vs_last_pct": vs_last.map(|v| round(v, 1)),
                })
            })
            .collect(),
    );

    let bilan: Option<String> =
        sqlx::query_scalar("SELECT text FROM activity_bilans WHERE activity_id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    out["bilan"] = json!(bilan);
    Ok(Json(out))
}

#[derive(Deserialize)]
pub(super) struct SummaryQuery {
    days: Option<i64>,
}

/// `GET /api/v1/summary?days=28` — synthesis of recent training: totals,
/// per-sport totals and a Monday-based weekly breakdown over the window.
pub(super) async fn summary(
    State(state): State<AppState>,
    Query(q): Query<SummaryQuery>,
) -> ApiResult {
    let days = q.days.unwrap_or(28).clamp(1, 3650);
    let since = days_ago(days - 1);
    // hrTSS counts towards `tss` weighted by the configured coefficient, the
    // same way it does in the training load.
    let coef = crate::metrics::get_hr_config(&state.pool).await?.tss_coef;

    let totals = aggregate(&state.pool, &since, None, coef).await?;

    let sports: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT sport_type FROM activities WHERE started_at >= ? ORDER BY sport_type",
    )
    .bind(&since)
    .fetch_all(&state.pool)
    .await?;
    let mut by_sport = serde_json::Map::new();
    for (sport,) in sports {
        let agg = aggregate(&state.pool, &since, Some(&sport), coef).await?;
        by_sport.insert(sport, agg);
    }

    // 'weekday 0' jumps forward to the next Sunday (or stays on one), so
    // minus 6 days is always that week's Monday.
    let weeks: Vec<(String, i64, f64, i64, f64, f64)> = sqlx::query_as(&format!(
        "SELECT date(started_at, 'weekday 0', '-6 days') AS week_start, COUNT(*), SUM(distance_m), \
                SUM(duration_s), SUM(elevation_gain_m), COALESCE(SUM({}), 0.0) \
         FROM activities WHERE started_at >= ?2 GROUP BY week_start ORDER BY week_start",
        crate::metrics::load_tss_sql(1)
    ))
    .bind(coef)
    .bind(&since)
    .fetch_all(&state.pool)
    .await?;

    let latest: Option<ActivityRecord> = sqlx::query_as(&format!(
        "SELECT {ACTIVITY_COLUMNS} FROM activities ORDER BY started_at DESC LIMIT 1"
    ))
    .fetch_optional(&state.pool)
    .await?;

    Ok(Json(json!({
        "days": days,
        "since": since,
        "totals": totals,
        "by_sport": by_sport,
        "by_week": weeks
            .into_iter()
            .map(|(week_start, n, dist, dur, elev, tss)| json!({
                "week_start": week_start,
                "activities": n,
                "distance_km": round(dist / 1000.0, 1),
                "duration_s": dur,
                "elevation_gain_m": elev.round(),
                "tss": round(tss, 0),
            }))
            .collect::<Vec<_>>(),
        "latest_activity": latest.as_ref().map(|a| activity_json(a, coef)),
    })))
}

/// Totals over `[since, now]`, optionally for one `sport_type` only.
async fn aggregate(
    pool: &SqlitePool,
    since: &str,
    sport: Option<&str>,
    coef: f64,
) -> Result<Value, ApiError> {
    let (n, dist, dur, elev, tss, avg_if): (i64, f64, i64, f64, f64, Option<f64>) = sqlx::query_as(&format!(
        "SELECT COUNT(*), COALESCE(SUM(distance_m), 0.0), COALESCE(SUM(duration_s), 0), \
                COALESCE(SUM(elevation_gain_m), 0.0), COALESCE(SUM({}), 0.0), AVG(intensity_factor) \
         FROM activities WHERE started_at >= ?1 AND (?2 IS NULL OR sport_type = ?2)",
        crate::metrics::load_tss_sql(3)
    ))
    .bind(since)
    .bind(sport)
    .bind(coef)
    .fetch_one(pool)
    .await?;
    Ok(json!({
        "activities": n,
        "distance_km": round(dist / 1000.0, 1),
        "duration_s": dur,
        "elevation_gain_m": elev.round(),
        "tss": round(tss, 0),
        "avg_intensity_factor": avg_if.map(|v| round(v, 2)),
    }))
}

// ---------------------------------------------------------------------------
// Workout templates and planned (calendar) sessions
// ---------------------------------------------------------------------------

fn planned_json(p: &crate::planned::Planned, ath: &crate::workout::Athlete) -> Value {
    let workout: Value = serde_json::from_str(&p.steps_json).unwrap_or(Value::Null);
    let (minutes, tss) = serde_json::from_value::<crate::workout::Workout>(workout.clone())
        .map(|w| w.load_estimate(ath))
        .unwrap_or((0.0, 0.0));
    json!({
        "id": p.id,
        "date": p.planned_date,
        "name": p.name,
        "notes": p.notes,
        "workout_id": p.workout_id,
        "source": p.source,
        "sport": p.sport,
        "duration_min": round(minutes, 1),
        "estimated_tss": round(tss, 0),
        "estimated_distance_km": workout.get("estimated_distance_km"),
        "workout": workout,
    })
}

async fn current_athlete(pool: &SqlitePool) -> Result<crate::workout::Athlete, ApiError> {
    Ok(crate::workout::Athlete::load(pool).await)
}

/// `GET /api/v1/workouts` — the reusable session templates (use an `id` as
/// `workout_id` when planning).
pub(super) async fn workouts(State(state): State<AppState>) -> ApiResult {
    let ath = current_athlete(&state.pool).await?;
    let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT id, name, steps_json, source, sport FROM workouts ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, steps_json, source, sport)| {
            let workout: Value = serde_json::from_str(&steps_json).unwrap_or(Value::Null);
            let (minutes, tss) = serde_json::from_value::<crate::workout::Workout>(workout.clone())
                .map(|w| w.load_estimate(&ath))
                .unwrap_or((0.0, 0.0));
            json!({
                "id": id, "name": name, "source": source, "sport": sport,
                "duration_min": round(minutes, 1), "estimated_tss": round(tss, 0),
                "estimated_distance_km": workout.get("estimated_distance_km"),
                "workout": workout,
            })
        })
        .collect();
    Ok(Json(json!({ "count": items.len(), "workouts": items })))
}

#[derive(Deserialize)]
pub(super) struct PlannedQuery {
    from: Option<String>,
    to: Option<String>,
    /// `cycling`, `running` or `other`; absent means all.
    sport: Option<String>,
}

/// `GET /api/v1/planned?from=2026-10-01&to=2026-11-01` — planned sessions with
/// `from <= date < to`. Defaults: a week back to six weeks ahead.
pub(super) async fn planned_list(
    State(state): State<AppState>,
    Query(q): Query<PlannedQuery>,
) -> ApiResult {
    let today = chrono::Utc::now().date_naive();
    let parse = |v: &Option<String>, name: &str, default: chrono::NaiveDate| match v {
        None => Ok(default),
        Some(s) => crate::planned::parse_date(s).ok_or_else(|| {
            ApiError(
                StatusCode::BAD_REQUEST,
                format!("`{name}` must be a YYYY-MM-DD date"),
            )
        }),
    };
    let from = parse(&q.from, "from", today - chrono::Duration::days(7))?;
    let to = parse(&q.to, "to", today + chrono::Duration::days(42))?;
    let ath = current_athlete(&state.pool).await?;
    let sport = match q.sport.as_deref() {
        None => None,
        Some(s) => Some(crate::planned::Sport::parse(s).ok_or_else(|| {
            ApiError(
                StatusCode::BAD_REQUEST,
                "`sport` must be cycling, running or other".to_string(),
            )
        })?),
    };
    let rows = crate::planned::list_between(&state.pool, from, to).await?;
    let items: Vec<Value> = rows
        .iter()
        .filter(|p| sport.is_none_or(|s| crate::planned::Sport::from_db(&p.sport) == s))
        .map(|p| planned_json(p, &ath))
        .collect();
    Ok(Json(
        json!({ "from": from.to_string(), "to": to.to_string(), "count": items.len(), "planned": items }),
    ))
}

#[derive(Deserialize)]
pub(super) struct PlanRequest {
    /// `YYYY-MM-DD`.
    date: String,
    /// Copy the steps of this template (see `GET /api/v1/workouts`)…
    workout_id: Option<i64>,
    /// …or give the session inline: `{"name": "...", "steps": [...]}`, same
    /// shape as a template's `workout`. Exactly one of the two.
    workout: Option<Value>,
    /// Overrides the session name (handy with `workout_id`).
    name: Option<String>,
    notes: Option<String>,
    /// `cycling` (default), `running` or `other`. With `workout_id`, defaults
    /// to the template's sport.
    sport: Option<String>,
}

/// `POST /api/v1/planned` — put a session on a day. The planned session owns
/// a copy of its steps (editing the template later doesn't change it); several
/// sessions on the same day are allowed. Returns 201 with the created session.
pub(super) async fn planned_create(
    State(state): State<AppState>,
    Json(req): Json<PlanRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let bad = |msg: String| ApiError(StatusCode::BAD_REQUEST, msg);
    let date = crate::planned::parse_date(&req.date)
        .ok_or_else(|| bad("`date` must be a YYYY-MM-DD date".into()))?;
    let ath = current_athlete(&state.pool).await?;

    let requested_sport = match req.sport.as_deref() {
        None => None,
        Some(s) => Some(
            crate::planned::Sport::parse(s)
                .ok_or_else(|| bad("`sport` must be cycling, running or other".into()))?,
        ),
    };

    let (mut workout, template_id, template_sport) = match (req.workout_id, req.workout) {
        (Some(_), Some(_)) => {
            return Err(bad("give either `workout_id` or `workout`, not both".into()));
        }
        (None, None) => return Err(bad("`workout_id` or `workout` is required".into())),
        (Some(id), None) => {
            let row: Option<(String, String)> =
                sqlx::query_as("SELECT steps_json, sport FROM workouts WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&state.pool)
                    .await?;
            let (steps, sport) = row.ok_or_else(|| {
                ApiError(
                    StatusCode::NOT_FOUND,
                    format!("workout template {id} not found"),
                )
            })?;
            (
                serde_json::from_str::<Value>(&steps).unwrap_or(Value::Null),
                Some(id),
                Some(crate::planned::Sport::from_db(&sport)),
            )
        }
        (None, Some(inline)) => (inline, None, None),
    };
    let sport = requested_sport.or(template_sport).unwrap_or_default();
    let name_override = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if let (Some(name), Some(obj)) = (name_override, workout.as_object_mut()) {
        obj.insert("name".to_string(), json!(name));
    }

    let prepared = crate::planned::prepare(&workout, &ath, sport).map_err(bad)?;
    let id = crate::planned::insert(
        &state.pool,
        date,
        &prepared,
        req.notes.as_deref(),
        template_id,
        "api",
        sport,
    )
    .await?;
    let created = crate::planned::get(&state.pool, id).await?.ok_or_else(|| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "planned session vanished".into(),
        )
    })?;
    Ok((StatusCode::CREATED, Json(planned_json(&created, &ath))))
}

/// `DELETE /api/v1/planned/{id}` — remove a session from the calendar.
pub(super) async fn planned_delete(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    if crate::planned::delete(&state.pool, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("planned session {id} not found"),
        ))
    }
}

// ---------------------------------------------------------------------------
// Detailed performance (per lap, whole ride) and raw-ish streams
// ---------------------------------------------------------------------------

async fn load_series(
    pool: &SqlitePool,
    activity_id: i64,
    is_run: bool,
) -> Result<Option<crate::performance::Series>, ApiError> {
    let streams: Option<String> =
        sqlx::query_scalar("SELECT streams_json FROM activity_streams WHERE activity_id = ?")
            .bind(activity_id)
            .fetch_optional(pool)
            .await?;
    Ok(streams
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| crate::performance::Series::from_streams(&v, !is_run)))
}

fn opt_round(v: Option<f64>, digits: i32) -> Option<f64> {
    v.map(|v| round(v, digits))
}

#[allow(clippy::too_many_arguments)]
fn lap_json(
    index: i64,
    start_s: usize,
    duration_s: i64,
    stored_distance_m: f64,
    stored_power: Option<f64>,
    stored_hr: Option<f64>,
    stats: Option<&crate::performance::WindowStats>,
    is_run: bool,
    ftp: f64,
) -> Value {
    let distance_m = stats
        .and_then(|s| s.distance_m)
        .unwrap_or(stored_distance_m);
    let avg_power = stats.and_then(|s| s.avg_power_w).or(stored_power);
    let np = stats.and_then(|s| s.normalized_power_w);
    let cadence = stats.and_then(|s| s.avg_cadence);
    json!({
        "index": index,
        "start_s": start_s,
        "duration_s": duration_s,
        "distance_km": round(distance_m / 1000.0, 2),
        "avg_pace_min_per_km": if is_run { super::format::format_pace(distance_m, duration_s) } else { None },
        "avg_speed_kmh": if !is_run && duration_s > 0 { Some(round(distance_m / duration_s as f64 * 3.6, 1)) } else { None },
        "avg_power_w": opt_round(avg_power, 0),
        "max_power_w": opt_round(stats.and_then(|s| s.max_power_w), 0),
        "normalized_power_w": opt_round(np, 0),
        "intensity_factor": opt_round(np.map(|np| np / ftp), 2),
        "avg_hr": opt_round(stats.and_then(|s| s.avg_hr).or(stored_hr), 0),
        "max_hr": opt_round(stats.and_then(|s| s.max_hr), 0),
        // Strava reports running cadence per leg: x2 gives steps per minute.
        "avg_cadence_rpm": if is_run { None } else { opt_round(cadence, 0) },
        "avg_cadence_spm": if is_run { opt_round(cadence.map(|c| c * 2.0), 0) } else { None },
        "elevation_gain_m": opt_round(stats.and_then(|s| s.elevation_gain_m), 0),
        "avg_grade_pct": opt_round(stats.and_then(|s| s.avg_grade_pct), 1),
        "power_zone_seconds": stats.and_then(|s| s.power_zone_s),
        "hr_zone_seconds": stats.and_then(|s| s.hr_zone_s),
    })
}

/// Whole-activity metrics computed from the streams: peak/normalized power,
/// variability, best-power curve, HR zones, power:HR decoupling.
fn performance_json(
    series: &crate::performance::Series,
    is_run: bool,
    ath: &crate::workout::Athlete,
) -> Value {
    let w = crate::performance::window_stats(series, 0, series.len, ath);
    let np = w.normalized_power_w;
    let curve = series
        .power
        .as_ref()
        .map(|p| {
            crate::performance::power_curve(p)
                .into_iter()
                .map(|(label, watts)| (label.to_string(), json!(opt_round(watts, 0))))
                .collect::<serde_json::Map<_, _>>()
        })
        .map(Value::Object)
        .unwrap_or(Value::Null);
    let cadence = w.avg_cadence;
    json!({
        "duration_s": w.duration_s,
        "avg_power_w": opt_round(w.avg_power_w, 0),
        "max_power_w": opt_round(w.max_power_w, 0),
        "normalized_power_w": opt_round(np, 0),
        "variability_index": match (np, w.avg_power_w) {
            (Some(np), Some(avg)) if avg > 0.0 => Some(round(np / avg, 2)),
            _ => None,
        },
        "intensity_factor": opt_round(np.map(|np| np / ath.ftp_watts), 2),
        "power_curve_w": curve,
        "avg_hr": opt_round(w.avg_hr, 0),
        "max_hr": opt_round(w.max_hr, 0),
        "hr_zone_seconds": w.hr_zone_s,
        "avg_cadence_rpm": if is_run { None } else { opt_round(cadence, 0) },
        "avg_cadence_spm": if is_run { opt_round(cadence.map(|c| c * 2.0), 0) } else { None },
        "avg_grade_pct": opt_round(w.avg_grade_pct, 1),
        "power_hr_decoupling_pct": opt_round(crate::performance::decoupling_pct(series), 1),
    })
}

#[derive(Deserialize)]
pub(super) struct StreamsQuery {
    /// Bucket size in seconds (default 10, 1 to 300).
    step: Option<usize>,
    /// Window start/end in seconds since the activity start (e.g. a lap's
    /// `start_s` and `start_s + duration_s`).
    from_s: Option<usize>,
    to_s: Option<usize>,
}

/// `GET /api/v1/activities/{id}/streams?step=10&from_s=0&to_s=3600` — the
/// activity's time series (power, HR, cadence, altitude, distance, speed or
/// pace) averaged over `step` seconds, optionally restricted to a window.
pub(super) async fn activity_streams(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<StreamsQuery>,
) -> ApiResult {
    let sport: Option<String> =
        sqlx::query_scalar("SELECT sport_type FROM activities WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    let Some(sport) = sport else {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("activity {id} not found"),
        ));
    };
    let is_run = crate::metrics::is_run(&sport);
    let Some(series) = load_series(&state.pool, id, is_run).await? else {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("activity {id} has no stream data"),
        ));
    };

    let step = q.step.unwrap_or(10).clamp(1, 300);
    let from = q.from_s.unwrap_or(0);
    let to = q.to_s.unwrap_or(series.len);
    if from >= to.min(series.len) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "`from_s` must be before `to_s` and inside the activity".into(),
        ));
    }
    let d = crate::performance::downsample(&series, from, to, step);
    let round_all = |v: &Option<Vec<f64>>, digits: i32| {
        v.as_ref()
            .map(|v| v.iter().map(|x| round(*x, digits)).collect::<Vec<_>>())
    };

    // Runs get pace (min/km, null while standing still) instead of speed.
    let (speed, pace) = match (&d.speed_kmh, is_run) {
        (Some(sp), true) => (
            None,
            Some(
                sp.iter()
                    .map(|v| {
                        if *v > 1.0 {
                            Some(round(60.0 / v, 2))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>(),
            ),
        ),
        (sp, _) => (round_all(&sp.clone(), 1), None),
    };
    Ok(Json(json!({
        "activity_id": id,
        "step_s": step,
        "from_s": from,
        "count": d.time_s.len(),
        "time_s": d.time_s,
        "distance_km": round_all(&d.distance_km, 3),
        "speed_kmh": speed,
        "pace_min_per_km": pace,
        "power_w": round_all(&d.power_w, 0),
        "hr_bpm": round_all(&d.hr_bpm, 0),
        "cadence": round_all(&d.cadence, 0),
        "altitude_m": round_all(&d.altitude_m, 1),
    })))
}
