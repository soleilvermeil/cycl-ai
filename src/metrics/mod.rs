use chrono::NaiveDate;
use sqlx::SqlitePool;

/// Runs all deterministic post-sync calculations: TSS/IF, power zones,
/// CTL/ATL/TSB. Recomputes for *all* activities every time (not just new
/// ones) so that editing the FTP in the dashboard retroactively re-dates
/// the whole history.
pub async fn run(pool: &SqlitePool) -> anyhow::Result<()> {
    let ftp = get_ftp(pool).await?;
    update_tss_if(pool, ftp).await?;
    recompute_all_zones(pool, ftp).await?;
    recompute_daily_load(pool).await?;
    Ok(())
}

pub async fn get_ftp(pool: &SqlitePool) -> anyhow::Result<f64> {
    let (ftp,): (i64,) = sqlx::query_as("SELECT ftp_watts FROM athlete_config WHERE id = 1")
        .fetch_one(pool)
        .await?;
    Ok(ftp as f64)
}

/// Standard TSS/IF formulas using normalized power (Strava's
/// weighted_average_watts, captured only from real power-meter data —
/// see strava::models::SummaryActivity).
async fn update_tss_if(pool: &SqlitePool, ftp: f64) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE activities
        SET intensity_factor = normalized_power_w / ?1,
            tss = (duration_s * normalized_power_w * (normalized_power_w / ?1)) / (?1 * 3600.0) * 100.0
        WHERE normalized_power_w IS NOT NULL
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
fn power_zone(watts: f64, ftp: f64) -> usize {
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

async fn recompute_all_zones(pool: &SqlitePool, ftp: f64) -> anyhow::Result<()> {
    let ids: Vec<(i64,)> = sqlx::query_as("SELECT activity_id FROM activity_streams")
        .fetch_all(pool)
        .await?;
    for (activity_id,) in ids {
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
    sqlx::query("DELETE FROM daily_load").execute(pool).await?;

    let rows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT date(started_at) as day, SUM(COALESCE(tss, 0.0)) FROM activities GROUP BY day",
    )
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
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
        .execute(pool)
        .await?;

        day += chrono::Duration::days(1);
    }

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
}
