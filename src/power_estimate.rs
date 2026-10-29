//! Crank-power estimate for rides that have speed but no power meter.
//!
//! The model is the usual road balance (rolling, air, gravity, acceleration),
//! divided by a drivetrain efficiency so the result is comparable to a crank
//! or hub meter. It never overwrites a recorded power stream. Disabled until
//! `/settings` turns it on and the rider has filled weight, CdA and Crr.

use serde_json::{Value, json};
use sqlx::SqlitePool;

const G: f64 = 9.80665;
/// Sea-level air density at about 15 °C. Wind is unknown and taken as zero.
const RHO: f64 = 1.226;
/// Power at the wheel / power at the crank.
const DRIVETRAIN: f64 = 0.975;

#[derive(Debug, Clone, Copy)]
pub struct Model {
    pub rider_kg: f64,
    pub bike_kg: f64,
    pub cda: f64,
    pub crr: f64,
}

impl Model {
    /// `None` when the four inputs can drive the model. Otherwise a sentence
    /// for the settings page.
    pub fn reject_reason(self) -> Option<&'static str> {
        if !(35.0..=200.0).contains(&self.rider_kg) {
            return Some("le poids du cycliste doit être entre 35 et 200 kg");
        }
        if !(4.0..=40.0).contains(&self.bike_kg) {
            return Some("le poids du vélo doit être entre 4 et 40 kg");
        }
        if !(0.15..=0.80).contains(&self.cda) {
            return Some("le CdA doit être entre 0,15 et 0,80 m² (typiquement 0,32 en haut de cintre, 0,28 en bas)");
        }
        if !(0.0015..=0.02).contains(&self.crr) {
            return Some("le Crr doit être entre 0,0015 et 0,020 (typiquement 0,004 en pneu course, 0,005 en entraînement)");
        }
        None
    }
}

/// One watt sample per `time` entry. `None` when speed cannot be derived.
/// Altitude may be absent: the grade is then zero (flat).
pub fn estimate_watts(
    time: &[f64],
    distance: &[f64],
    altitude: Option<&[f64]>,
    model: Model,
) -> Option<Vec<f64>> {
    let n = time.len();
    if n < 2 || distance.len() != n || model.reject_reason().is_some() {
        return None;
    }
    let altitude = altitude.filter(|a| a.len() == n);
    let mass = model.rider_kg + model.bike_kg;
    let mut watts = vec![0.0; n];
    let mut prev_v = None;
    for i in 1..n {
        let dt = time[i] - time[i - 1];
        if !(0.2..=30.0).contains(&dt) {
            watts[i] = watts[i - 1];
            continue;
        }
        let mut v = ((distance[i] - distance[i - 1]) / dt).clamp(0.0, 25.0);
        if v < 1.0 {
            prev_v = Some(0.0);
            watts[i] = 0.0;
            continue;
        }
        let v_prev = prev_v.unwrap_or(v);
        let accel = ((v - v_prev) / dt).clamp(-1.5, 1.5);
        // Kill a one-sample GPS spike by not letting speed jump faster than
        // the clamped acceleration allows.
        v = (v_prev + accel * dt).clamp(0.0, 25.0);
        prev_v = Some(v);
        let grade = altitude
            .map(|alt| grade_at(distance, alt, i))
            .unwrap_or(0.0);
        let rolling = model.crr * mass * G * v;
        let air = 0.5 * model.cda * RHO * v * v * v;
        let gravity = mass * G * grade * v;
        let inertia = mass * accel * v;
        let crank = (rolling + air + gravity + inertia) / DRIVETRAIN;
        watts[i] = crank.clamp(0.0, 2000.0);
    }
    watts[0] = watts[1];
    Some(watts)
}

/// Grade over the nearest samples covering about 40 m each side, clamped so
/// a single bad altitude point cannot invent a wall.
fn grade_at(distance: &[f64], altitude: &[f64], i: usize) -> f64 {
    let mut j = i;
    while j > 0 && distance[i] - distance[j] < 40.0 {
        j -= 1;
    }
    let mut k = i;
    while k + 1 < distance.len() && distance[k] - distance[i] < 40.0 {
        k += 1;
    }
    let dd = distance[k] - distance[j];
    if dd < 20.0 {
        return 0.0;
    }
    ((altitude[k] - altitude[j]) / dd).clamp(-0.25, 0.25)
}

fn channel(v: &Value, key: &str) -> Option<Vec<f64>> {
    v.get(key)?
        .get("data")?
        .as_array()?
        .iter()
        .map(|x| x.as_f64())
        .collect()
}

fn has_measured_watts(v: &Value) -> bool {
    channel(v, "watts").is_some_and(|w| w.iter().any(|x| *x > 1.0))
}

pub async fn load_model(pool: &SqlitePool) -> anyhow::Result<Model> {
    let (rider_kg, bike_kg, cda, crr): (f64, f64, f64, f64) = sqlx::query_as(
        "SELECT rider_weight_kg, bike_weight_kg, cda, crr FROM athlete_config WHERE id = 1",
    )
    .fetch_one(pool)
    .await?;
    Ok(Model {
        rider_kg,
        bike_kg,
        cda,
        crr,
    })
}

/// Writes an estimated `watts` stream on the given rides when they have speed
/// and no measured power. Rides already marked `power_source = 'estimate'`
/// are recomputed. Runs are left alone. Returns how many activities changed.
pub async fn apply(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let model = load_model(pool).await?;
    if let Some(reason) = model.reject_reason() {
        tracing::warn!(reason, "power estimate skipped");
        return Ok(0);
    }
    let mut updated = 0usize;
    for &id in ids {
        let row: Option<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT a.sport_type, a.power_source, s.streams_json \
             FROM activities a JOIN activity_streams s ON s.activity_id = a.id \
             WHERE a.id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        let Some((sport, power_source, streams_json)) = row else {
            continue;
        };
        if !crate::metrics::is_ride(&sport) {
            continue;
        }
        let Ok(mut streams) = serde_json::from_str::<Value>(&streams_json) else {
            continue;
        };
        let estimated_already = power_source.as_deref() == Some("estimate");
        if has_measured_watts(&streams) && !estimated_already {
            continue;
        }
        let Some(time) = channel(&streams, "time") else {
            continue;
        };
        let Some(distance) = channel(&streams, "distance") else {
            continue;
        };
        let altitude = channel(&streams, "altitude");
        let Some(watts) = estimate_watts(&time, &distance, altitude.as_deref(), model) else {
            continue;
        };
        if let Some(obj) = streams.as_object_mut() {
            obj.insert("watts".to_string(), json!({ "data": watts }));
        }
        let (avg, np) = summary_power(&streams);
        sqlx::query(
            "UPDATE activities SET avg_power_w = ?, normalized_power_w = ?, power_source = 'estimate' WHERE id = ?",
        )
        .bind(avg)
        .bind(np)
        .bind(id)
        .execute(pool)
        .await?;
        sqlx::query("UPDATE activity_streams SET streams_json = ? WHERE activity_id = ?")
            .bind(streams.to_string())
            .bind(id)
            .execute(pool)
            .await?;
        crate::bests::store(pool, id, false, &streams).await?;
        updated += 1;
    }
    Ok(updated)
}

/// Every outdoor or indoor ride. Used by the settings « Recalculer » button.
pub async fn apply_all(pool: &SqlitePool) -> anyhow::Result<usize> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM activities WHERE sport_type IN ('Ride', 'VirtualRide')",
    )
    .fetch_all(pool)
    .await?;
    apply(pool, &ids).await
}

fn summary_power(streams: &Value) -> (Option<f64>, Option<f64>) {
    let Some(series) = crate::performance::Series::from_streams(streams, true) else {
        return (None, None);
    };
    let Some(power) = series.power.as_ref() else {
        return (None, None);
    };
    if power.is_empty() {
        return (None, None);
    }
    let avg = power.iter().sum::<f64>() / power.len() as f64;
    let np = crate::performance::normalized_power(power).or(Some(avg));
    (Some(avg), np)
}

/// Removes estimated watts and the TSS that was derived from them. Measured
/// rides are untouched. The caller recomputes metrics and segment efforts.
pub async fn clear(pool: &SqlitePool) -> anyhow::Result<usize> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT a.id, s.streams_json FROM activities a \
         JOIN activity_streams s ON s.activity_id = a.id \
         WHERE a.power_source = 'estimate'",
    )
    .fetch_all(pool)
    .await?;
    let n = rows.len();
    for (id, streams_json) in rows {
        let mut streams: Value = serde_json::from_str(&streams_json).unwrap_or(json!({}));
        if let Some(obj) = streams.as_object_mut() {
            obj.remove("watts");
        }
        sqlx::query("UPDATE activity_streams SET streams_json = ? WHERE activity_id = ?")
            .bind(streams.to_string())
            .bind(id)
            .execute(pool)
            .await?;
        sqlx::query(
            "UPDATE activities SET avg_power_w = NULL, normalized_power_w = NULL, tss = NULL, \
             tss_source = NULL, intensity_factor = NULL, power_source = NULL, \
             zone1_s = 0, zone2_s = 0, zone3_s = 0, zone4_s = 0, zone5_s = 0, zone6_s = 0, zone7_s = 0 \
             WHERE id = ?",
        )
        .bind(id)
        .execute(pool)
        .await?;
        crate::bests::store(pool, id, false, &streams).await?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn road() -> Model {
        Model {
            rider_kg: 75.0,
            bike_kg: 8.0,
            cda: 0.32,
            crr: 0.005,
        }
    }

    fn steady(seconds: usize, speed_ms: f64, grade: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let time: Vec<f64> = (0..seconds).map(|s| s as f64).collect();
        let mut distance = Vec::with_capacity(seconds);
        let mut altitude = Vec::with_capacity(seconds);
        let mut d = 0.0;
        let mut h = 100.0;
        for _ in 0..seconds {
            distance.push(d);
            altitude.push(h);
            d += speed_ms;
            h += speed_ms * grade;
        }
        (time, distance, altitude)
    }

    #[test]
    fn flat_30_kmh_is_about_150_watts() {
        let (time, distance, altitude) = steady(90, 30.0 / 3.6, 0.0);
        let watts = estimate_watts(&time, &distance, Some(&altitude), road()).unwrap();
        let mid = watts[60];
        assert!(
            (140.0..=175.0).contains(&mid),
            "expected ~151 W at 30 km/h on the flat, got {mid}"
        );
    }

    #[test]
    fn a_five_percent_climb_costs_far_more_than_the_flat() {
        let (time, distance, altitude) = steady(90, 5.0, 0.05);
        let watts = estimate_watts(&time, &distance, Some(&altitude), road()).unwrap();
        assert!(
            watts[60] > 240.0,
            "climb power should be well above flat riding, got {}",
            watts[60]
        );
    }

    #[test]
    fn stopped_samples_are_zero_and_missing_speed_is_refused() {
        let (mut time, mut distance, altitude) = steady(30, 8.0, 0.0);
        for i in 10..20 {
            distance[i] = distance[10];
        }
        let watts = estimate_watts(&time, &distance, Some(&altitude), road()).unwrap();
        assert_eq!(watts[15], 0.0);
        time.pop();
        assert!(estimate_watts(&time, &distance, None, road()).is_none());
    }

    #[test]
    fn rejects_a_model_without_the_required_fields() {
        let blank = Model {
            rider_kg: 0.0,
            bike_kg: 0.0,
            cda: 0.0,
            crr: 0.0,
        };
        assert!(blank.reject_reason().is_some());
        assert!(road().reject_reason().is_none());
    }
}
