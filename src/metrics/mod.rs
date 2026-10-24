use chrono::NaiveDate;
use sqlx::SqlitePool;

/// Sports imported into the app. Rides get the power-based pipeline
/// (NP/TSS/IF/zones/segments); runs get pace + HR and an HR-based TSS
/// (`hr_tss`), and no power zones/segments.
pub fn is_supported_sport(sport_type: &str) -> bool {
    matches!(sport_type, "Ride" | "VirtualRide" | "Run" | "TrailRun")
}

/// Road rides and indoor/trainer rides (Strava `VirtualRide`: Zwift, smart
/// trainer) share the power-based pipeline.
pub fn is_ride(sport_type: &str) -> bool {
    matches!(sport_type, "Ride" | "VirtualRide")
}

pub fn is_run(sport_type: &str) -> bool {
    matches!(sport_type, "Run" | "TrailRun")
}

/// Heart-rate settings the hrTSS model needs. `lthr` is the *effective*
/// threshold HR: the configured one, or 85 % of heart-rate reserve above
/// resting when none is set (`athlete_config.lthr = 0`).
#[derive(Debug, Clone, Copy)]
pub struct HrConfig {
    pub rest: f64,
    pub max: f64,
    pub lthr: f64,
    /// Weight of hrTSS when it's added to the total load next to power TSS
    /// (`athlete_config.hr_tss_coef`; 1.0 = counts the same). Applied only by
    /// the load aggregations (see `LOAD_TSS_SQL`), never to the stored hrTSS.
    pub tss_coef: f64,
}

/// SQL expression (over `activities`) for an activity's contribution to the
/// total training load: the hrTSS weighted by the coefficient bound to
/// `?{n}`, power TSS unchanged. `{n}` is the 1-based bind index.
pub fn load_tss_sql(coef_param: usize) -> String {
    format!("CASE WHEN tss_source = 'hr' THEN tss * ?{coef_param} ELSE tss END")
}

pub async fn get_hr_config(pool: &SqlitePool) -> anyhow::Result<HrConfig> {
    let (hr_max, hr_rest, lthr, tss_coef): (i64, i64, i64, f64) =
        sqlx::query_as("SELECT hr_max, hr_rest, lthr, hr_tss_coef FROM athlete_config WHERE id = 1")
            .fetch_one(pool)
            .await?;
    let (rest, max) = (hr_rest as f64, hr_max as f64);
    let lthr = if lthr > 0 { lthr as f64 } else { rest + 0.85 * (max - rest) };
    Ok(HrConfig { rest, max, lthr, tss_coef })
}

/// Banister TRIMP per minute at a given heart rate (male coefficients, the
/// usual default): HRr * 0.64 * e^(1.92 * HRr), HRr = fraction of HR reserve.
fn trimp_per_min(hr: f64, cfg: &HrConfig) -> f64 {
    if cfg.max <= cfg.rest {
        return 0.0;
    }
    let hrr = ((hr - cfg.rest) / (cfg.max - cfg.rest)).clamp(0.0, 1.0);
    hrr * 0.64 * (1.92 * hrr).exp()
}

/// Gaps longer than this between two stream samples (auto-pause, GPS loss)
/// are counted as this long, so a pause can't masquerade as hard effort.
const MAX_SAMPLE_GAP_S: f64 = 30.0;

/// hrTSS from a heart-rate stream: the activity's TRIMP relative to one hour
/// at threshold HR, x100 (so an hour at LTHR = 100, like power TSS at FTP).
pub fn hr_tss(heartrate: &[f64], time: &[f64], cfg: &HrConfig) -> Option<f64> {
    let n = heartrate.len().min(time.len());
    if n < 2 {
        return None;
    }
    let mut trimp = 0.0;
    for i in 1..n {
        let dt = (time[i] - time[i - 1]).clamp(0.0, MAX_SAMPLE_GAP_S);
        trimp += trimp_per_min(heartrate[i], cfg) * dt / 60.0;
    }
    let one_hour_at_threshold = trimp_per_min(cfg.lthr, cfg) * 60.0;
    (one_hour_at_threshold > 0.0).then(|| trimp / one_hour_at_threshold * 100.0)
}

/// hrTSS from the average HR only (activities without a stream).
fn hr_tss_from_average(avg_hr: f64, duration_s: i64, cfg: &HrConfig) -> Option<f64> {
    let one_hour_at_threshold = trimp_per_min(cfg.lthr, cfg) * 60.0;
    (one_hour_at_threshold > 0.0)
        .then(|| trimp_per_min(avg_hr, cfg) * duration_s as f64 / 60.0 / one_hour_at_threshold * 100.0)
}

/// Runs all deterministic post-sync calculations: TSS/IF, power zones,
/// hrTSS for runs, CTL/ATL/TSB. Recomputes for *all* activities every time
/// (not just new ones) so that editing the FTP or HR settings in the
/// dashboard retroactively re-dates the whole history.
pub async fn run(pool: &SqlitePool) -> anyhow::Result<()> {
    run_for(pool, None).await
}

/// Same as `run`, but the per-activity stream work (hrTSS, power zones — it
/// parses every stored stream, which is slow on a NAS) is limited to
/// `only`. Used by the sync, where nothing but the newly imported activities
/// can have changed; settings changes keep using the full `run`.
pub async fn run_incremental(pool: &SqlitePool, only: &[i64]) -> anyhow::Result<()> {
    run_for(pool, Some(only)).await
}

async fn run_for(pool: &SqlitePool, only: Option<&[i64]>) -> anyhow::Result<()> {
    let ftp = get_ftp(pool).await?;
    update_tss_if(pool, ftp).await?;
    update_run_hr_tss(pool, only).await?;
    recompute_all_zones(pool, ftp, only).await?;
    recompute_daily_load(pool).await?;
    refresh_dashboard_summary(pool).await?;
    Ok(())
}

/// Window is 4 weeks, not 1 — a weekend-only rider can have zero activity
/// on 5 of 7 days, so a 7-day window is too sparse to be a useful summary.
const LOAD_WINDOW_DAYS: i64 = 27;

/// Upper bounds of the TSB interpretation bands (Coggan's PMC guidelines),
/// lowest first; anything above the last bound is the open-ended top band.
/// `web::TSB_ZONES` carries the labels/colors for the same bands (a test
/// there keeps the two in sync).
pub const TSB_BAND_UPPER_BOUNDS: [f64; 5] = [-30.0, -10.0, 0.0, 10.0, 25.0];

/// Everything the dashboard's "4 dernières semaines" card displays,
/// precomputed (table `dashboard_summary`).
pub struct DashboardSummary {
    pub tss_period: f64,
    pub ctl: f64,
    pub atl: f64,
    pub tsb: f64,
    pub zone_secs: [i64; 7],
    pub rest_days_to_next_band: Option<i64>,
}

/// Simulates forward day-by-day rest (TSS=0) using the same Banister EWMA
/// as `recompute_daily_load`, and returns the number of days until CTL-ATL
/// reaches `target`. Rest doesn't push TSB up forever — ATL decays faster
/// than CTL at first (raising TSB) but both eventually fade toward 0, so a
/// high target may never be reached; capped at 180 days, returning `None`
/// rather than a misleading huge number in that case.
pub fn days_of_rest_to_reach(mut ctl: f64, mut atl: f64, target: f64) -> Option<i64> {
    if ctl - atl >= target {
        return Some(0);
    }
    for day in 1..=180 {
        ctl += -ctl / 42.0;
        atl += -atl / 7.0;
        if ctl - atl >= target {
            return Some(day);
        }
    }
    None
}

/// Rest days until TSB leaves its current band for the next one up; `None`
/// in the top band (nothing above it) or when it can't be reached.
fn rest_days_to_next_band(ctl: f64, atl: f64, tsb: f64) -> Option<i64> {
    let next_upper = TSB_BAND_UPPER_BOUNDS.iter().copied().find(|upper| tsb < *upper)?;
    days_of_rest_to_reach(ctl, atl, next_upper)
}

/// Rebuilds the single `dashboard_summary` row from `daily_load`/`activities`
/// (after topping `daily_load` up to today, so rest days since the last sync
/// are reflected). Cheap enough to run on its own, but the point is that the
/// dashboard doesn't have to: it reads the row and only calls this when the
/// stored date is no longer today.
pub async fn refresh_dashboard_summary(pool: &SqlitePool) -> anyhow::Result<()> {
    extend_daily_load_to_today(pool).await?;

    let today = chrono::Utc::now().date_naive();
    let window_start = (today - chrono::Duration::days(LOAD_WINDOW_DAYS)).to_string();

    let latest: Option<(f64, f64, f64)> =
        sqlx::query_as("SELECT ctl, atl, tsb FROM daily_load ORDER BY date DESC LIMIT 1")
            .fetch_optional(pool)
            .await?;
    let (ctl, atl, tsb) = latest.unwrap_or((0.0, 0.0, 0.0));

    let (tss_period,): (f64,) =
        sqlx::query_as("SELECT COALESCE(SUM(tss), 0.0) FROM daily_load WHERE date >= ?")
            .bind(&window_start)
            .fetch_one(pool)
            .await?;

    // started_at is ISO-8601, so a plain string comparison against the
    // window's first day is exact and lets idx_activities_started_at apply.
    let z: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT COALESCE(SUM(zone1_s),0), COALESCE(SUM(zone2_s),0), COALESCE(SUM(zone3_s),0),
               COALESCE(SUM(zone4_s),0), COALESCE(SUM(zone5_s),0), COALESCE(SUM(zone6_s),0), COALESCE(SUM(zone7_s),0)
        FROM activities WHERE started_at >= ?
        "#,
    )
    .bind(&window_start)
    .fetch_one(pool)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO dashboard_summary
            (id, computed_for_date, tss_period, ctl, atl, tsb,
             zone1_s, zone2_s, zone3_s, zone4_s, zone5_s, zone6_s, zone7_s, rest_days_to_next_band)
        VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (id) DO UPDATE SET
            computed_for_date = excluded.computed_for_date, tss_period = excluded.tss_period,
            ctl = excluded.ctl, atl = excluded.atl, tsb = excluded.tsb,
            zone1_s = excluded.zone1_s, zone2_s = excluded.zone2_s, zone3_s = excluded.zone3_s,
            zone4_s = excluded.zone4_s, zone5_s = excluded.zone5_s, zone6_s = excluded.zone6_s,
            zone7_s = excluded.zone7_s, rest_days_to_next_band = excluded.rest_days_to_next_band
        "#,
    )
    .bind(today.to_string())
    .bind(tss_period)
    .bind(ctl)
    .bind(atl)
    .bind(tsb)
    .bind(z.0)
    .bind(z.1)
    .bind(z.2)
    .bind(z.3)
    .bind(z.4)
    .bind(z.5)
    .bind(z.6)
    .bind(rest_days_to_next_band(ctl, atl, tsb))
    .execute(pool)
    .await?;
    Ok(())
}

/// What the dashboard calls on every page load: a single-row read, with a
/// refresh only when the stored row was computed on an earlier day (or
/// doesn't exist yet, e.g. on a fresh database).
pub async fn dashboard_summary(pool: &SqlitePool) -> anyhow::Result<DashboardSummary> {
    let today = chrono::Utc::now().date_naive().to_string();
    for attempt in 0..2 {
        let row: Option<(String, f64, f64, f64, f64, i64, i64, i64, i64, i64, i64, i64, Option<i64>)> =
            sqlx::query_as(
                "SELECT computed_for_date, tss_period, ctl, atl, tsb, \
                 zone1_s, zone2_s, zone3_s, zone4_s, zone5_s, zone6_s, zone7_s, rest_days_to_next_band \
                 FROM dashboard_summary WHERE id = 1",
            )
            .fetch_optional(pool)
            .await?;
        if let Some(r) = row.filter(|r| r.0 == today || attempt == 1) {
            return Ok(DashboardSummary {
                tss_period: r.1,
                ctl: r.2,
                atl: r.3,
                tsb: r.4,
                zone_secs: [r.5, r.6, r.7, r.8, r.9, r.10, r.11],
                rest_days_to_next_band: r.12,
            });
        }
        refresh_dashboard_summary(pool).await?;
    }
    unreachable!("second attempt always returns")
}

pub async fn get_ftp(pool: &SqlitePool) -> anyhow::Result<f64> {
    let (ftp,): (i64,) = sqlx::query_as("SELECT ftp_watts FROM athlete_config WHERE id = 1")
        .fetch_one(pool)
        .await?;
    Ok(ftp as f64)
}

/// hrTSS for every run, written to `activities.tss` (source `'hr'`) so it
/// accumulates with the rides' power TSS in `daily_load`. Uses the HR stream
/// when there is one, the average HR otherwise; a run with no HR data at all
/// has no TSS (and contributes nothing, rather than breaking anything).
async fn update_run_hr_tss(pool: &SqlitePool, only: Option<&[i64]>) -> anyhow::Result<()> {
    let cfg = get_hr_config(pool).await?;
    let runs: Vec<(i64, i64, Option<f64>)> = sqlx::query_as(
        "SELECT id, duration_s, avg_hr FROM activities WHERE sport_type IN ('Run', 'TrailRun')",
    )
    .fetch_all(pool)
    .await?;

    for (id, duration_s, avg_hr) in runs {
        if only.is_some_and(|ids| !ids.contains(&id)) {
            continue;
        }
        let streams_json: Option<String> =
            sqlx::query_scalar("SELECT streams_json FROM activity_streams WHERE activity_id = ?")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        let from_stream = streams_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| {
                let series = |key: &str| -> Option<Vec<f64>> {
                    v.get(key)?.get("data")?.as_array()?.iter().map(|x| x.as_f64()).collect()
                };
                hr_tss(&series("heartrate")?, &series("time")?, &cfg)
            });
        let tss = from_stream.or_else(|| avg_hr.and_then(|hr| hr_tss_from_average(hr, duration_s, &cfg)));
        sqlx::query("UPDATE activities SET tss = ?, tss_source = ? WHERE id = ?")
            .bind(tss)
            .bind(tss.map(|_| "hr"))
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Standard TSS/IF formulas using normalized power (Strava's
/// weighted_average_watts, captured only from real power-meter data —
/// see strava::models::SummaryActivity).
async fn update_tss_if(pool: &SqlitePool, ftp: f64) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE activities
        SET intensity_factor = normalized_power_w / ?1,
            tss = (duration_s * normalized_power_w * (normalized_power_w / ?1)) / (?1 * 3600.0) * 100.0,
            tss_source = 'power'
        WHERE normalized_power_w IS NOT NULL AND sport_type NOT IN ('Run', 'TrailRun')
        "#,
    )
    .bind(ftp)
    .execute(pool)
    .await?;
    Ok(())
}

/// Coggan's extended 7-zone model as % of FTP — a rider's strength/weakness
/// profile is read off this distribution (time spent Z5-Z7 vs sustained
/// Z2/Z3).
pub fn power_zone(watts: f64, ftp: f64) -> usize {
    let pct = watts / ftp;
    if pct < 0.55 {
        1 // Active Recovery
    } else if pct < 0.75 {
        2 // Endurance
    } else if pct < 0.90 {
        3 // Tempo
    } else if pct < 1.05 {
        4 // Threshold
    } else if pct < 1.20 {
        5 // VO2max
    } else if pct < 1.50 {
        6 // Anaerobic Capacity
    } else {
        7 // Neuromuscular Power
    }
}

async fn recompute_all_zones(pool: &SqlitePool, ftp: f64, only: Option<&[i64]>) -> anyhow::Result<()> {
    let ids: Vec<(i64,)> = sqlx::query_as(
        "SELECT s.activity_id FROM activity_streams s JOIN activities a ON a.id = s.activity_id \n         WHERE a.sport_type NOT IN ('Run', 'TrailRun')",
    )
        .fetch_all(pool)
        .await?;
    for (activity_id,) in ids {
        if only.is_some_and(|only| !only.contains(&activity_id)) {
            continue;
        }
        compute_zones_for_activity(pool, activity_id, ftp).await?;
    }
    Ok(())
}

async fn compute_zones_for_activity(
    pool: &SqlitePool,
    activity_id: i64,
    ftp: f64,
) -> anyhow::Result<()> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT streams_json FROM activity_streams WHERE activity_id = ?")
            .bind(activity_id)
            .fetch_optional(pool)
            .await?;
    let Some((streams_json,)) = row else {
        return Ok(());
    };
    let parsed: serde_json::Value = serde_json::from_str(&streams_json)?;
    let watts = parsed.get("watts").and_then(|s| s.get("data")).and_then(|d| d.as_array());
    let time = parsed.get("time").and_then(|s| s.get("data")).and_then(|d| d.as_array());
    let (Some(watts), Some(time)) = (watts, time) else {
        return Ok(());
    };

    let mut zone_secs = [0i64; 7];
    let n = time.len().min(watts.len());
    for i in 1..n {
        let t0 = time[i - 1].as_f64().unwrap_or(0.0);
        let t1 = time[i].as_f64().unwrap_or(0.0);
        let dt = (t1 - t0).max(0.0) as i64;
        let w = watts[i].as_f64().unwrap_or(0.0);
        zone_secs[power_zone(w, ftp) - 1] += dt;
    }

    sqlx::query(
        "UPDATE activities SET zone1_s=?, zone2_s=?, zone3_s=?, zone4_s=?, zone5_s=?, zone6_s=?, zone7_s=? WHERE id=?",
    )
    .bind(zone_secs[0])
    .bind(zone_secs[1])
    .bind(zone_secs[2])
    .bind(zone_secs[3])
    .bind(zone_secs[4])
    .bind(zone_secs[5])
    .bind(zone_secs[6])
    .bind(activity_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Banister impulse-response model: CTL (42-day EWMA of TSS, "fitness"),
/// ATL (7-day EWMA, "fatigue"), TSB = CTL - ATL ("form"), all three taken
/// from the same day's post-session values so a row's numbers are always
/// internally consistent (TSB literally equals that row's CTL minus ATL,
/// matching the dashboard glossary). This recursion doesn't translate to a
/// single SQL query, hence Rust.
///
/// `pub` (not just called from `run`) so the dashboard can re-run it on
/// every page load, not just at sync time — with no new activity, TSS is 0
/// but CTL/ATL still decay day by day, so a rest streak should visibly
/// raise TSB even between syncs.
pub async fn recompute_daily_load(pool: &SqlitePool) -> anyhow::Result<()> {
    // Full rebuild, not incremental: if the earliest activity changes (e.g.
    // after purging misclassified activities), stale rows before the new
    // first_day would otherwise linger forever since the loop below only
    // touches [first_day, today].
    // One transaction for the whole rebuild: it inserts a row per day since
    // the first activity, and a commit (fsync) per row is what made the sync
    // crawl on the NAS's disk.
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM daily_load").execute(&mut *tx).await?;

    // hrTSS (runs) enters the load weighted by the configured coefficient.
    let coef = get_hr_config(pool).await?.tss_coef;
    let rows: Vec<(String, f64)> = sqlx::query_as(&format!(
        "SELECT date(started_at) as day, SUM(COALESCE({}, 0.0)) FROM activities GROUP BY day",
        load_tss_sql(1)
    ))
    .bind(coef)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        tx.commit().await?;
        return Ok(());
    }

    let mut tss_by_day: std::collections::HashMap<NaiveDate, f64> = std::collections::HashMap::new();
    for (day_str, tss) in &rows {
        if let Ok(day) = NaiveDate::parse_from_str(day_str, "%Y-%m-%d") {
            tss_by_day.insert(day, *tss);
        }
    }

    let first_day = *tss_by_day.keys().min().unwrap();
    let today = chrono::Utc::now().date_naive();

    let mut ctl = 0.0;
    let mut atl = 0.0;
    let mut day = first_day;
    while day <= today {
        let tss_today = tss_by_day.get(&day).copied().unwrap_or(0.0);
        ctl += (tss_today - ctl) / 42.0;
        atl += (tss_today - atl) / 7.0;
        let tsb = ctl - atl;

        sqlx::query(
            r#"
            INSERT INTO daily_load (date, tss, ctl, atl, tsb)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT (date) DO UPDATE SET
                tss = excluded.tss, ctl = excluded.ctl, atl = excluded.atl, tsb = excluded.tsb
            "#,
        )
        .bind(day.to_string())
        .bind(tss_today)
        .bind(ctl)
        .bind(atl)
        .bind(tsb)
        .execute(&mut *tx)
        .await?;

        day += chrono::Duration::days(1);
    }

    tx.commit().await?;
    Ok(())
}

/// Cheap alternative to `recompute_daily_load` for the dashboard's per-page
/// -load refresh: extends `daily_load` forward from its last row to today
/// (a rest streak still needs to visibly decay CTL/ATL and raise TSB
/// between syncs) without re-walking the entire history. Between two syncs
/// no new activity can appear (only `sync::run` imports them, and it always
/// finishes with a full `recompute_daily_load`), so every day this walks is
/// guaranteed TSS=0 — cost is O(days since last load), not O(history).
/// A full rebuild still runs at sync time and on FTP edits, where
/// correctness requires re-dating the whole history.
pub async fn extend_daily_load_to_today(pool: &SqlitePool) -> anyhow::Result<()> {
    let last: Option<(String, f64, f64)> =
        sqlx::query_as("SELECT date, ctl, atl FROM daily_load ORDER BY date DESC LIMIT 1")
            .fetch_optional(pool)
            .await?;
    // No daily_load yet (no activities ever synced) — nothing to extend;
    // the first sync's full recompute will create it.
    let Some((last_date_str, mut ctl, mut atl)) = last else {
        return Ok(());
    };
    let Ok(last_date) = NaiveDate::parse_from_str(&last_date_str, "%Y-%m-%d") else {
        return Ok(());
    };
    let today = chrono::Utc::now().date_naive();

    let mut tx = pool.begin().await?;
    let mut day = last_date + chrono::Duration::days(1);
    while day <= today {
        ctl += -ctl / 42.0;
        atl += -atl / 7.0;
        let tsb = ctl - atl;
        sqlx::query(
            "INSERT INTO daily_load (date, tss, ctl, atl, tsb) VALUES (?, 0.0, ?, ?, ?)",
        )
        .bind(day.to_string())
        .bind(ctl)
        .bind(atl)
        .bind(tsb)
        .execute(&mut *tx)
        .await?;
        day += chrono::Duration::days(1);
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn tss_if_uses_current_ftp() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 200, 190, 50)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m, normalized_power_w) \
             VALUES (1, '2026-08-01T10:00:00Z', 3600, 30000, 300, 200)",
        )
        .execute(&pool)
        .await
        .unwrap();

        run(&pool).await.unwrap();

        let (tss, intensity_factor): (f64, f64) =
            sqlx::query_as("SELECT tss, intensity_factor FROM activities WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();

        // NP == FTP => IF = 1.0, one hour at IF 1.0 => TSS = 100
        assert!((intensity_factor - 1.0).abs() < 1e-9);
        assert!((tss - 100.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn runs_contribute_no_tss_zones_or_load_and_do_not_break_the_pipeline() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 200, 190, 50)")
            .execute(&pool)
            .await
            .unwrap();
        // A run carrying (foot-pod) power must still be ignored by TSS/zones.
        sqlx::query(
            "INSERT INTO activities (id, sport_type, started_at, duration_s, distance_m, elevation_gain_m, normalized_power_w)              VALUES (1, 'Run', '2026-08-01T10:00:00Z', 3000, 10000, 50, 250)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let streams = serde_json::json!({"time": {"data": [0, 10]}, "watts": {"data": [250, 250]}});
        sqlx::query("INSERT INTO activity_streams (activity_id, streams_json) VALUES (1, ?)")
            .bind(streams.to_string())
            .execute(&pool)
            .await
            .unwrap();

        run(&pool).await.unwrap();

        let (tss, z4): (Option<f64>, i64) =
            sqlx::query_as("SELECT tss, zone4_s + zone5_s + zone6_s FROM activities WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(tss, None);
        assert_eq!(z4, 0);
        let (load_tss,): (f64,) = sqlx::query_as("SELECT COALESCE(SUM(tss), 0.0) FROM daily_load")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(load_tss, 0.0);
        let summary = dashboard_summary(&pool).await.unwrap();
        assert!(summary.tsb.is_finite());
    }

    #[test]
    fn hr_tss_is_100_for_one_hour_at_threshold_and_scales_with_intensity() {
        let cfg = HrConfig { rest: 50.0, max: 190.0, lthr: 170.0, tss_coef: 1.0 };
        let time: Vec<f64> = (0..=3600).map(|t| t as f64).collect();
        let at_threshold = vec![170.0; time.len()];
        let tss = hr_tss(&at_threshold, &time, &cfg).unwrap();
        assert!((tss - 100.0).abs() < 0.1, "got {tss}");

        // Easier effort for the same hour is a lot less stress; resting HR is none.
        let easy = hr_tss(&vec![140.0; time.len()], &time, &cfg).unwrap();
        assert!(easy > 20.0 && easy < 60.0, "got {easy}");
        assert_eq!(hr_tss(&vec![50.0; time.len()], &time, &cfg).unwrap(), 0.0);
        // Half the duration at threshold is half the TSS.
        let half = hr_tss(&at_threshold[..1801], &time[..1801], &cfg).unwrap();
        assert!((half - 50.0).abs() < 0.1);
    }

    #[test]
    fn hr_tss_ignores_long_pauses_in_the_stream() {
        let cfg = HrConfig { rest: 50.0, max: 190.0, lthr: 170.0, tss_coef: 1.0 };
        // 10 min at threshold, a 1 h auto-pause gap, 10 more minutes.
        let mut time: Vec<f64> = (0..=600).map(|t| t as f64).collect();
        time.extend((0..=600).map(|t| 4200.0 + t as f64));
        let hr = vec![170.0; time.len()];
        let tss = hr_tss(&hr, &time, &cfg).unwrap();
        // ~20 min + at most one 30 s capped gap, not 80 min.
        assert!(tss > 32.0 && tss < 36.0, "got {tss}");
    }

    #[tokio::test]
    async fn run_hr_tss_accumulates_with_ride_tss_in_the_load() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest, lthr) VALUES (1, 200, 190, 50, 170)")
            .execute(&pool)
            .await
            .unwrap();
        // Same day: a power ride (TSS 100) and a 1 h run at threshold HR (hrTSS ~100).
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m, normalized_power_w) \
             VALUES (1, '2026-08-01T07:00:00Z', 3600, 30000, 300, 200)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO activities (id, sport_type, started_at, duration_s, distance_m, elevation_gain_m, avg_hr) \
             VALUES (2, 'Run', '2026-08-01T18:00:00Z', 3600, 10000, 50, 170)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let time: Vec<f64> = (0..=3600).map(|t| t as f64).collect();
        let streams = serde_json::json!({"time": {"data": time}, "heartrate": {"data": vec![170.0; 3601]}});
        sqlx::query("INSERT INTO activity_streams (activity_id, streams_json) VALUES (2, ?)")
            .bind(streams.to_string())
            .execute(&pool)
            .await
            .unwrap();
        // A second run with no HR at all: no TSS, nothing breaks.
        sqlx::query(
            "INSERT INTO activities (id, sport_type, started_at, duration_s, distance_m, elevation_gain_m) \
             VALUES (3, 'Run', '2026-08-02T18:00:00Z', 1800, 5000, 20)",
        )
        .execute(&pool)
        .await
        .unwrap();

        run(&pool).await.unwrap();

        let (ride_tss, ride_src): (f64, String) =
            sqlx::query_as("SELECT tss, tss_source FROM activities WHERE id = 1").fetch_one(&pool).await.unwrap();
        let (run_tss, run_src): (f64, String) =
            sqlx::query_as("SELECT tss, tss_source FROM activities WHERE id = 2").fetch_one(&pool).await.unwrap();
        let (no_hr_tss,): (Option<f64>,) =
            sqlx::query_as("SELECT tss FROM activities WHERE id = 3").fetch_one(&pool).await.unwrap();
        assert!((ride_tss - 100.0).abs() < 1e-6 && ride_src == "power");
        assert!((run_tss - 100.0).abs() < 0.5 && run_src == "hr", "run tss {run_tss}");
        assert_eq!(no_hr_tss, None);

        // Both land in the same day's load: the day's TSS is the sum.
        let (day_tss,): (f64,) = sqlx::query_as("SELECT tss FROM daily_load WHERE date = '2026-08-01'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!((day_tss - (ride_tss + run_tss)).abs() < 1e-6);
    }

    #[tokio::test]
    async fn hr_tss_coefficient_weights_runs_in_the_load_but_not_the_stored_tss() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest, lthr) VALUES (1, 200, 190, 50, 170)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m, normalized_power_w) \
             VALUES (1, '2026-08-01T07:00:00Z', 3600, 30000, 300, 200)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO activities (id, sport_type, started_at, duration_s, distance_m, elevation_gain_m, avg_hr) \
             VALUES (2, 'Run', '2026-08-01T18:00:00Z', 3600, 10000, 50, 170)",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query("UPDATE athlete_config SET hr_tss_coef = 0.5").execute(&pool).await.unwrap();
        run(&pool).await.unwrap();

        let (run_tss,): (f64,) = sqlx::query_as("SELECT tss FROM activities WHERE id = 2").fetch_one(&pool).await.unwrap();
        let (day_tss,): (f64,) = sqlx::query_as("SELECT tss FROM daily_load WHERE date = '2026-08-01'")
            .fetch_one(&pool)
            .await
            .unwrap();
        // Stored hrTSS is raw (~100); the day counts the ride in full (100) + half the run.
        assert!((run_tss - 100.0).abs() < 0.5, "stored hrTSS {run_tss}");
        assert!((day_tss - (100.0 + run_tss * 0.5)).abs() < 1e-6, "day load {day_tss}");

        // 0 = runs don't count towards load at all; the summary follows.
        sqlx::query("UPDATE athlete_config SET hr_tss_coef = 0").execute(&pool).await.unwrap();
        run(&pool).await.unwrap();
        let (day_tss,): (f64,) = sqlx::query_as("SELECT tss FROM daily_load WHERE date = '2026-08-01'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!((day_tss - 100.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn threshold_hr_defaults_from_hr_reserve_when_unset() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 200, 190, 50)")
            .execute(&pool)
            .await
            .unwrap();
        let cfg = get_hr_config(&pool).await.unwrap();
        assert!((cfg.lthr - (50.0 + 0.85 * 140.0)).abs() < 1e-9);
        sqlx::query("UPDATE athlete_config SET lthr = 167").execute(&pool).await.unwrap();
        assert_eq!(get_hr_config(&pool).await.unwrap().lthr, 167.0);
    }

    #[test]
    fn power_zone_boundaries() {
        let ftp = 200.0;
        assert_eq!(power_zone(100.0, ftp), 1); // 50%
        assert_eq!(power_zone(140.0, ftp), 2); // 70%
        assert_eq!(power_zone(170.0, ftp), 3); // 85%
        assert_eq!(power_zone(200.0, ftp), 4); // 100%
        assert_eq!(power_zone(220.0, ftp), 5); // 110%
        assert_eq!(power_zone(280.0, ftp), 6); // 140%
        assert_eq!(power_zone(320.0, ftp), 7); // 160%
    }

    #[tokio::test]
    async fn zone_binning_from_streams() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 200, 190, 50)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m) \
             VALUES (1, '2026-08-01T10:00:00Z', 20, 100, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // 10s at 100W (zone1, 50% FTP), 10s at 250W (zone6, 125% FTP)
        let streams = serde_json::json!({
            "time": {"data": [0, 10, 20]},
            "watts": {"data": [100, 100, 250]},
        });
        sqlx::query("INSERT INTO activity_streams (activity_id, streams_json) VALUES (1, ?)")
            .bind(streams.to_string())
            .execute(&pool)
            .await
            .unwrap();

        run(&pool).await.unwrap();

        let (z1, z6): (i64, i64) =
            sqlx::query_as("SELECT zone1_s, zone6_s FROM activities WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(z1, 10);
        assert_eq!(z6, 10);
    }

    #[tokio::test]
    async fn ctl_atl_ewma_recursion() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 200, 190, 50)")
            .execute(&pool)
            .await
            .unwrap();
        // A single 100-TSS day; everything else in [first_day, today] is 0.
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m, normalized_power_w) \
             VALUES (1, '2026-08-01T10:00:00Z', 3600, 30000, 300, 200)",
        )
        .execute(&pool)
        .await
        .unwrap();

        run(&pool).await.unwrap();

        let (ctl, atl, tsb): (f64, f64, f64) = sqlx::query_as(
            "SELECT ctl, atl, tsb FROM daily_load WHERE date = '2026-08-01'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        // First day, starting from ctl=atl=0: ctl = 0 + (100-0)/42, atl = 0 + (100-0)/7
        assert!((ctl - 100.0 / 42.0).abs() < 1e-9);
        assert!((atl - 100.0 / 7.0).abs() < 1e-9);
        // tsb must always equal that same row's ctl - atl
        assert!((tsb - (ctl - atl)).abs() < 1e-9);
    }

    #[tokio::test]
    async fn extend_top_up_decays_forward_to_today_without_touching_earlier_rows() {
        let pool = test_pool().await;
        let today = chrono::Utc::now().date_naive();
        let last_synced_day = today - chrono::Duration::days(3);

        sqlx::query("INSERT INTO daily_load (date, tss, ctl, atl, tsb) VALUES (?, 100.0, 50.0, 30.0, 20.0)")
            .bind(last_synced_day.to_string())
            .execute(&pool)
            .await
            .unwrap();

        extend_daily_load_to_today(&pool).await.unwrap();

        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM daily_load")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 4, "last_synced_day plus 3 topped-up days through today");

        // Original row untouched.
        let (tss, ctl, atl): (f64, f64, f64) =
            sqlx::query_as("SELECT tss, ctl, atl FROM daily_load WHERE date = ?")
                .bind(last_synced_day.to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(tss, 100.0);
        assert_eq!(ctl, 50.0);
        assert_eq!(atl, 30.0);

        // Rest decay: TSS=0 for every topped-up day, ATL (7-day) decaying
        // faster than CTL (42-day).
        let (last_date, last_tss, last_ctl, last_atl, last_tsb): (String, f64, f64, f64, f64) =
            sqlx::query_as("SELECT date, tss, ctl, atl, tsb FROM daily_load ORDER BY date DESC LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(last_date, today.to_string());
        assert_eq!(last_tss, 0.0);
        assert!(last_ctl < 50.0 && last_ctl > 0.0);
        assert!(last_atl < 30.0 && last_atl > 0.0);
        assert!((last_tsb - (last_ctl - last_atl)).abs() < 1e-9);
    }

    #[tokio::test]
    async fn extend_top_up_is_noop_when_already_current() {
        let pool = test_pool().await;
        let today = chrono::Utc::now().date_naive();
        sqlx::query("INSERT INTO daily_load (date, tss, ctl, atl, tsb) VALUES (?, 0.0, 10.0, 5.0, 5.0)")
            .bind(today.to_string())
            .execute(&pool)
            .await
            .unwrap();

        extend_daily_load_to_today(&pool).await.unwrap();

        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM daily_load")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "already up to date, nothing to add");
    }

    #[test]
    fn rest_simulation_reaches_target_and_reports_zero_when_already_there() {
        // Already past the target.
        assert_eq!(days_of_rest_to_reach(20.0, 5.0, 0.0), Some(0));
        // ATL (7-day) decays faster than CTL (42-day) under pure rest, so
        // TSB rises quickly at first — a couple of days to cross into Frais.
        let days = days_of_rest_to_reach(28.9, 37.4, 0.0).expect("reachable within cap");
        assert!((1..=5).contains(&days), "expected a small number of days, got {days}");
    }

    #[test]
    fn unreachable_target_returns_none_within_cap() {
        // With ctl=atl=0 (no history), TSB stays at 0 forever under rest —
        // it can never climb into the Optimal band.
        assert_eq!(days_of_rest_to_reach(0.0, 0.0, 10.0), None);
    }

    #[test]
    fn no_next_band_estimate_in_the_top_band() {
        assert_eq!(rest_days_to_next_band(10.0, -20.0, 30.0), None);
    }

    #[tokio::test]
    async fn dashboard_summary_is_computed_once_then_read_from_the_cache_row() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 200, 190, 50)")
            .execute(&pool)
            .await
            .unwrap();
        let yesterday = chrono::Utc::now().date_naive() - chrono::Duration::days(1);
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m, normalized_power_w, zone2_s) \
             VALUES (1, ?, 3600, 30000, 300, 200, 3600)",
        )
        .bind(format!("{yesterday}T10:00:00Z"))
        .execute(&pool)
        .await
        .unwrap();
        run(&pool).await.unwrap();

        // Cache row exists after run(); a later activity edit is not picked
        // up by a same-day read (nothing re-aggregates on page load).
        sqlx::query("UPDATE activities SET zone2_s = 0 WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        let summary = dashboard_summary(&pool).await.unwrap();
        assert_eq!(summary.zone_secs[1], 3600);
        assert!((summary.tss_period - 100.0).abs() < 1e-9);
        assert!((summary.tsb - (summary.ctl - summary.atl)).abs() < 1e-9);

        // A stale row (computed on an earlier day) is rebuilt transparently.
        sqlx::query("UPDATE dashboard_summary SET computed_for_date = '2000-01-01'")
            .execute(&pool)
            .await
            .unwrap();
        let summary = dashboard_summary(&pool).await.unwrap();
        assert_eq!(summary.zone_secs[1], 0);
    }

    #[tokio::test]
    async fn extend_top_up_is_noop_with_no_history() {
        let pool = test_pool().await;
        extend_daily_load_to_today(&pool).await.unwrap();
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM daily_load")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
