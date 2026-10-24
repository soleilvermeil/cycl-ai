use crate::strava::StravaClient;
use sqlx::SqlitePool;

/// Distance under which a GPS point counts as "at" a segment endpoint.
/// Consumer cycling-computer GPS is accurate to a few meters in the open,
/// but tree cover / urban canyon can push errors past 20m — 15m is tight
/// enough to avoid cross-matching nearby segments.
const MATCH_THRESHOLD_M: f64 = 15.0;

/// Fetches the user's starred Strava segments and upserts their coordinates
/// into `reference_segments`. Starring the segment is a manual step on
/// Strava's side (see README) — this only pulls coordinates, never uses
/// Strava's own effort/segment_efforts calculations.
pub async fn import_starred(pool: &SqlitePool) -> anyhow::Result<()> {
    let settings = crate::settings::fetch(pool).await?;
    let refresh_token: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT strava_refresh_token FROM sync_state WHERE id = 1")
            .fetch_optional(pool)
            .await?
            .flatten();

    let client = StravaClient::authenticate(
        &settings.strava_client_id,
        &settings.strava_client_secret,
        &refresh_token.unwrap_or_default(),
    )
    .await?;

    let segments = client.list_starred_segments().await?;
    tracing::info!(count = segments.len(), "starred segments fetched");

    for seg in &segments {
        sqlx::query(
            r#"
            INSERT INTO reference_segments
                (name, start_lat, start_lng, end_lat, end_lng, distance_m, elevation_gain_m)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (name) DO UPDATE SET
                start_lat = excluded.start_lat, start_lng = excluded.start_lng,
                end_lat = excluded.end_lat, end_lng = excluded.end_lng,
                distance_m = excluded.distance_m, elevation_gain_m = excluded.elevation_gain_m
            "#,
        )
        .bind(&seg.name)
        .bind(seg.start_latlng[0])
        .bind(seg.start_latlng[1])
        .bind(seg.end_latlng[0])
        .bind(seg.end_latlng[1])
        .bind(seg.distance)
        .bind(seg.elevation_high - seg.elevation_low)
        .execute(pool)
        .await?;
    }

    // The sync skips matching when nothing new was imported, so match the
    // existing rides against the (possibly new) segments right away.
    run(pool).await?;

    Ok(())
}

fn haversine_m(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    const EARTH_RADIUS_M: f64 = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlng = (lng2 - lng1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlng / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    EARTH_RADIUS_M * c
}

struct RefSegment {
    id: i64,
    start_lat: f64,
    start_lng: f64,
    end_lat: f64,
    end_lng: f64,
}

/// Finds the [start_idx, end_idx] index range of `latlng` that covers this
/// segment: the first point within threshold of the segment start, then the
/// first *subsequent* point within threshold of the segment end. Returns
/// None if the activity never passed through the segment.
fn match_segment(latlng: &[[f64; 2]], seg: &RefSegment) -> Option<(usize, usize)> {
    let start_idx = latlng
        .iter()
        .position(|p| haversine_m(p[0], p[1], seg.start_lat, seg.start_lng) < MATCH_THRESHOLD_M)?;
    let end_idx = latlng[start_idx..]
        .iter()
        .position(|p| haversine_m(p[0], p[1], seg.end_lat, seg.end_lng) < MATCH_THRESHOLD_M)
        .map(|i| start_idx + i)?;
    if end_idx > start_idx {
        Some((start_idx, end_idx))
    } else {
        None
    }
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

struct Portion {
    avg_power_w: Option<f64>,
    avg_speed_ms: Option<f64>,
    avg_hr: Option<f64>,
}

fn extract_portion(
    start_idx: usize,
    end_idx: usize,
    distance: &[f64],
    time: &[f64],
    watts: Option<&[f64]>,
    heartrate: Option<&[f64]>,
) -> Portion {
    let slice_avg = |data: Option<&[f64]>| {
        data.and_then(|d| d.get(start_idx..=end_idx)).and_then(|s| mean(s))
    };
    let dt = time.get(end_idx).zip(time.get(start_idx)).map(|(e, s)| e - s);
    let dd = distance
        .get(end_idx)
        .zip(distance.get(start_idx))
        .map(|(e, s)| e - s);
    let avg_speed_ms = match (dd, dt) {
        (Some(dd), Some(dt)) if dt > 0.0 => Some(dd / dt),
        _ => None,
    };

    Portion {
        avg_power_w: slice_avg(watts),
        avg_speed_ms,
        avg_hr: slice_avg(heartrate),
    }
}

pub(crate) fn f64_array(v: &serde_json::Value, key: &str) -> Option<Vec<f64>> {
    v.get(key)?
        .get("data")?
        .as_array()?
        .iter()
        .map(|x| x.as_f64())
        .collect()
}

pub(crate) fn latlng_array(v: &serde_json::Value) -> Option<Vec<[f64; 2]>> {
    v.get("latlng")?
        .get("data")?
        .as_array()?
        .iter()
        .map(|pair| {
            let arr = pair.as_array()?;
            Some([arr.first()?.as_f64()?, arr.get(1)?.as_f64()?])
        })
        .collect()
}

/// Rebuilds `segment_efforts` from scratch, processing activities in
/// chronological order so each effort's PR/recent-avg/last-effort deltas
/// are computed against the history that existed *before* it — a replay,
/// not a batch aggregate, since those deltas depend on ordering.
pub async fn run(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM segment_efforts").execute(pool).await?;

    let ref_segments: Vec<(i64, f64, f64, f64, f64)> = sqlx::query_as(
        "SELECT id, start_lat, start_lng, end_lat, end_lng FROM reference_segments",
    )
    .fetch_all(pool)
    .await?;
    let ref_segments: Vec<RefSegment> = ref_segments
        .into_iter()
        .map(|(id, start_lat, start_lng, end_lat, end_lng)| RefSegment {
            id,
            start_lat,
            start_lng,
            end_lat,
            end_lng,
        })
        .collect();

    let activities: Vec<(i64, String)> = sqlx::query_as(
        r#"
        SELECT a.id, s.streams_json
        FROM activities a
        JOIN activity_streams s ON s.activity_id = a.id
        WHERE a.sport_type = 'Ride'
        ORDER BY a.started_at ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    for (activity_id, streams_json) in activities {
        let parsed: serde_json::Value = serde_json::from_str(&streams_json)?;
        let Some(latlng) = latlng_array(&parsed) else {
            continue;
        };
        let Some(distance) = f64_array(&parsed, "distance") else {
            continue;
        };
        let Some(time) = f64_array(&parsed, "time") else {
            continue;
        };
        let watts = f64_array(&parsed, "watts");
        let heartrate = f64_array(&parsed, "heartrate");

        for seg in &ref_segments {
            let Some((start_idx, end_idx)) = match_segment(&latlng, seg) else {
                continue;
            };
            let portion = extract_portion(
                start_idx,
                end_idx,
                &distance,
                &time,
                watts.as_deref(),
                heartrate.as_deref(),
            );
            let Some(power) = portion.avg_power_w else {
                // Deltas below are power-based; skip segments with no power data.
                continue;
            };

            let history: Vec<(i64, f64)> = sqlx::query_as(
                "SELECT activity_id, avg_power_w FROM segment_efforts \
                 WHERE reference_segment_id = ? AND avg_power_w IS NOT NULL \
                 ORDER BY id ASC",
            )
            .bind(seg.id)
            .fetch_all(pool)
            .await?;
            let powers: Vec<f64> = history.iter().map(|(_, p)| *p).collect();

            let pr = history.iter().fold(None::<(i64, f64)>, |acc, &(aid, p)| match acc {
                Some((_, best)) if best >= p => acc,
                _ => Some((aid, p)),
            });
            let recent_avg = mean(&powers[powers.len().saturating_sub(5)..]);
            let last = history.last().copied();

            let pct_delta = |reference: Option<f64>| {
                reference.map(|r| (power - r) / r * 100.0)
            };

            sqlx::query(
                r#"
                INSERT INTO segment_efforts
                    (activity_id, reference_segment_id, avg_power_w, avg_speed_ms, avg_hr,
                     delta_vs_pr_pct, delta_vs_recent_avg_pct, delta_vs_last_pct,
                     start_time_s, end_time_s, pr_activity_id, last_activity_id,
                     start_distance_m, end_distance_m)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(activity_id)
            .bind(seg.id)
            .bind(power)
            .bind(portion.avg_speed_ms)
            .bind(portion.avg_hr)
            .bind(pct_delta(pr.map(|(_, p)| p)))
            .bind(pct_delta(recent_avg))
            .bind(pct_delta(last.map(|(_, p)| p)))
            .bind(time.get(start_idx).copied())
            .bind(time.get(end_idx).copied())
            .bind(pr.map(|(aid, _)| aid))
            .bind(last.map(|(aid, _)| aid))
            .bind(distance.get(start_idx).copied())
            .bind(distance.get(end_idx).copied())
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_known_distance() {
        // Roughly 1 degree of longitude at the equator ~= 111.2 km.
        let d = haversine_m(0.0, 0.0, 0.0, 1.0);
        assert!((d - 111_195.0).abs() < 200.0);
    }

    #[test]
    fn matches_segment_when_gps_passes_through() {
        let latlng = vec![
            [49.0, 6.0],   // far away
            [49.1000, 6.2000], // near start
            [49.1005, 6.2005], // mid
            [49.1010, 6.2010], // near end
            [49.2, 6.3],   // far away after
        ];
        let seg = RefSegment {
            id: 1,
            start_lat: 49.1000,
            start_lng: 6.2000,
            end_lat: 49.1010,
            end_lng: 6.2010,
        };
        let m = match_segment(&latlng, &seg);
        assert_eq!(m, Some((1, 3)));
    }

    #[test]
    fn no_match_when_never_near_segment() {
        let latlng = vec![[0.0, 0.0], [0.001, 0.001]];
        let seg = RefSegment {
            id: 1,
            start_lat: 49.0,
            start_lng: 6.0,
            end_lat: 49.01,
            end_lng: 6.01,
        };
        assert_eq!(match_segment(&latlng, &seg), None);
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn segment_streams_json(latlng: [[f64; 2]; 3], watts: [f64; 3]) -> String {
        serde_json::json!({
            "latlng": {"data": latlng},
            "distance": {"data": [0.0, 50.0, 100.0]},
            "time": {"data": [0.0, 10.0, 20.0]},
            "watts": {"data": watts},
        })
        .to_string()
    }

    #[tokio::test]
    async fn full_pipeline_computes_deltas_chronologically() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO reference_segments (name, start_lat, start_lng, end_lat, end_lng, distance_m, elevation_gain_m) \
             VALUES ('crux', 49.1000, 6.2000, 49.1010, 6.2010, 2420, 131)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Two activities on the same segment: first at 200W, second (later) at 220W.
        let latlng = [[49.1000, 6.2000], [49.1005, 6.2005], [49.1010, 6.2010]];
        for (id, started_at, watts) in [
            (1i64, "2026-08-01T10:00:00Z", 200.0),
            (2i64, "2026-08-08T10:00:00Z", 220.0),
        ] {
            sqlx::query(
                "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m) VALUES (?, ?, 20, 100, 0)",
            )
            .bind(id)
            .bind(started_at)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO activity_streams (activity_id, streams_json) VALUES (?, ?)")
                .bind(id)
                .bind(segment_streams_json(latlng, [watts, watts, watts]))
                .execute(&pool)
                .await
                .unwrap();
        }

        run(&pool).await.unwrap();

        let rows: Vec<(i64, f64, Option<f64>, Option<f64>, Option<f64>)> = sqlx::query_as(
            "SELECT activity_id, avg_power_w, delta_vs_pr_pct, delta_vs_recent_avg_pct, delta_vs_last_pct \
             FROM segment_efforts ORDER BY activity_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(rows.len(), 2);
        // First effort has no history: all deltas NULL.
        assert_eq!(rows[0].0, 1);
        assert!((rows[0].1 - 200.0).abs() < 1e-9);
        assert!(rows[0].2.is_none());
        assert!(rows[0].3.is_none());
        assert!(rows[0].4.is_none());

        // Second effort: PR/recent-avg/last are all the first effort's 200W,
        // so delta = (220-200)/200*100 = 10%.
        assert_eq!(rows[1].0, 2);
        assert!((rows[1].1 - 220.0).abs() < 1e-9);
        assert!((rows[1].2.unwrap() - 10.0).abs() < 1e-9);
        assert!((rows[1].3.unwrap() - 10.0).abs() < 1e-9);
        assert!((rows[1].4.unwrap() - 10.0).abs() < 1e-9);
    }
}
