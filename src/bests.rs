//! Per-activity records feeding the Statistiques page: best average power
//! over benchmark durations and best time over benchmark distances. Computed
//! once from the raw streams (at sync / import, and by a boot backfill for
//! older rows) into `activity_bests`, so the page never parses streams.

use crate::performance::{best_average, Series};
use serde_json::Value;
use sqlx::SqlitePool;

/// Durations (s) of the power radar — same set as `performance::power_curve`.
pub const POWER_DURATIONS: [usize; 8] = [5, 15, 30, 60, 300, 600, 1200, 3600];
/// Benchmark distances (m) for rides.
pub const RIDE_DISTANCES: [u32; 6] = [1_000, 5_000, 10_000, 20_000, 40_000, 80_000];
/// Benchmark distances (m) for runs.
pub const RUN_DISTANCES: [u32; 5] = [1_000, 5_000, 10_000, 21_097, 42_195];

/// Shortest time (s) to cover `target_m` metres, from a 1 Hz cumulative
/// distance series (index = second). `None` if the activity is shorter.
pub fn best_time_for_distance(dist: &[f64], target_m: f64) -> Option<f64> {
    if dist.len() < 2 || *dist.last()? < target_m {
        return None;
    }
    // Guard against GPS glitches making the cumulative distance go backwards.
    let mut mono = Vec::with_capacity(dist.len());
    let mut max = 0.0f64;
    for d in dist {
        max = max.max(*d);
        mono.push(max);
    }
    let mut best: Option<usize> = None;
    let mut i = 0usize;
    for j in 0..mono.len() {
        // Latest start index whose window [i, j] still covers the target.
        while i < j && mono[j] - mono[i + 1] >= target_m {
            i += 1;
        }
        if mono[j] - mono[i] >= target_m {
            let t = j - i;
            best = Some(best.map_or(t, |b: usize| b.min(t)));
        }
    }
    best.map(|t| t as f64)
}

/// (kind, key, value) rows for one activity.
pub fn compute(streams: &Value, is_run: bool) -> Vec<(&'static str, i64, f64)> {
    let mut rows = Vec::new();
    let Some(series) = Series::from_streams(streams, !is_run) else {
        return rows;
    };
    if let Some(power) = &series.power {
        for secs in POWER_DURATIONS {
            if let Some(w) = best_average(power, secs).filter(|w| *w > 0.0) {
                rows.push(("power", secs as i64, w));
            }
        }
    }
    if let Some(dist) = &series.distance_m {
        let targets: &[u32] = if is_run { &RUN_DISTANCES } else { &RIDE_DISTANCES };
        for t in targets {
            if let Some(secs) = best_time_for_distance(dist, *t as f64) {
                rows.push(("dist", *t as i64, secs));
            }
        }
    }
    rows
}

pub async fn store(pool: &SqlitePool, activity_id: i64, is_run: bool, streams: &Value) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM activity_bests WHERE activity_id = ?").bind(activity_id).execute(pool).await?;
    for (kind, key, value) in compute(streams, is_run) {
        sqlx::query("INSERT INTO activity_bests (activity_id, kind, key, value) VALUES (?, ?, ?, ?)")
            .bind(activity_id)
            .bind(kind)
            .bind(key)
            .bind(value)
            .execute(pool)
            .await?;
    }
    sqlx::query("UPDATE activities SET bests_computed = 1 WHERE id = ?").bind(activity_id).execute(pool).await?;
    Ok(())
}

/// Computes the records of every activity stored before this feature
/// existed. A no-op once all rows are flagged `bests_computed`.
pub async fn backfill(pool: &SqlitePool) -> anyhow::Result<()> {
    let ids: Vec<(i64, String)> = sqlx::query_as("SELECT id, sport_type FROM activities WHERE bests_computed = 0")
        .fetch_all(pool)
        .await?;
    if ids.is_empty() {
        return Ok(());
    }
    tracing::info!(count = ids.len(), "computing activity records");
    for (id, sport) in ids {
        let streams: Option<String> = sqlx::query_scalar("SELECT streams_json FROM activity_streams WHERE activity_id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
        let parsed = streams.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
        store(pool, id, crate::metrics::is_run(&sport), &parsed).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_time_finds_the_fastest_window() {
        // 10 m/s for 100 s, then 5 m/s for 100 s: fastest 500 m is 50 s.
        let mut dist = Vec::new();
        for s in 0..=100 {
            dist.push(s as f64 * 10.0);
        }
        for s in 1..=100 {
            dist.push(1000.0 + s as f64 * 5.0);
        }
        assert_eq!(best_time_for_distance(&dist, 500.0), Some(50.0));
        assert_eq!(best_time_for_distance(&dist, 1000.0), Some(100.0));
        assert_eq!(best_time_for_distance(&dist, 5000.0), None);
    }

    #[test]
    fn compute_returns_power_and_distance_rows() {
        let n = 400usize;
        let time: Vec<f64> = (0..n).map(|s| s as f64).collect();
        let watts: Vec<f64> = (0..n).map(|s| if s < 100 { 300.0 } else { 150.0 }).collect();
        let dist: Vec<f64> = (0..n).map(|s| s as f64 * 8.0).collect();
        let streams = serde_json::json!({
            "time": {"data": time}, "watts": {"data": watts}, "distance": {"data": dist},
        });
        let rows = compute(&streams, false);
        assert!(rows.contains(&("power", 5, 300.0)));
        assert!(rows.iter().any(|r| r.0 == "power" && r.1 == 300));
        assert!(rows.contains(&("dist", 1000, 125.0)));
        // A run ignores power.
        assert!(compute(&streams, true).iter().all(|r| r.0 == "dist"));
    }
}
