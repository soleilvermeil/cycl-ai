//! Readings computed from streams already in the database: repeated climbs
//! turned into local segments, interval blocks, lap kind (auto-split versus
//! a structured workout), and a comparison with recent similar rides.
//!
//! Nothing here is sent to the model unless `/settings` has enabled the
//! feature. Starred Strava segments are never modified.

use serde_json::Value;
use sqlx::SqlitePool;
use std::collections::HashSet;

const CLIMB_GRADE: f64 = 0.03;
const CLIMB_MIN_M: f64 = 300.0;
const CLIMB_MIN_GAIN_M: f64 = 20.0;
/// Two climbs are the same hill when both ends fall inside this radius.
const CLUSTER_M: f64 = 80.0;
/// Auto endpoints are estimated, so matching is looser than a starred pin.
pub const AUTO_MATCH_M: f64 = 50.0;

#[derive(Debug, Clone, PartialEq)]
pub struct ClimbHit {
    pub start_lat: f64,
    pub start_lng: f64,
    pub end_lat: f64,
    pub end_lng: f64,
    pub distance_m: f64,
    pub gain_m: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LapRead {
    /// Equal distance or time splits (the watch's auto-lap), not a workout.
    AutoSplit,
    /// Manual laps or a FIT workout's warmup / work / rest intensities.
    Structured,
    Ordinary,
}

#[derive(Debug, Clone)]
pub struct LapSample {
    pub distance_m: f64,
    pub trigger: Option<String>,
    pub intensity: Option<String>,
}

pub struct Effort {
    pub duration_s: f64,
    pub avg_w: f64,
}

pub fn classify_laps(laps: &[LapSample]) -> LapRead {
    if laps.len() < 2 {
        return LapRead::Ordinary;
    }
    let triggers: Vec<&str> = laps.iter().filter_map(|l| l.trigger.as_deref()).collect();
    if triggers.len() >= 4 {
        let auto = triggers
            .iter()
            .filter(|t| **t == "distance" || **t == "time")
            .count();
        if auto * 2 >= triggers.len() {
            return LapRead::AutoSplit;
        }
    }
    let intensities: Vec<&str> = laps.iter().filter_map(|l| l.intensity.as_deref()).collect();
    let work = intensities
        .iter()
        .any(|s| *s == "active" || *s == "interval");
    let rest = intensities.iter().any(|s| {
        matches!(*s, "rest" | "recovery" | "warmup" | "cooldown")
    });
    if work && rest && laps.len() >= 3 {
        return LapRead::Structured;
    }
    if triggers.len() >= 3 {
        let manual = triggers.iter().filter(|t| **t == "manual").count();
        if manual * 2 >= triggers.len() {
            return LapRead::Structured;
        }
    }
    let mut dists: Vec<f64> = laps
        .iter()
        .map(|l| l.distance_m)
        .filter(|d| *d > 50.0)
        .collect();
    if dists.len() >= 4 {
        dists.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = dists[dists.len() / 2];
        if (400.0..=5000.0).contains(&median) {
            let close = dists
                .iter()
                .filter(|d| (**d - median).abs() / median < 0.12)
                .count();
            if close >= 4 && close + 1 >= dists.len() {
                return LapRead::AutoSplit;
            }
        }
    }
    LapRead::Ordinary
}

pub fn lap_note(kind: LapRead) -> Option<&'static str> {
    match kind {
        LapRead::AutoSplit => Some(
            "Laps : splits automatiques (distance ou temps régulier), pas une séance structurée.",
        ),
        LapRead::Structured => Some(
            "Laps : séance structurée (déclenchement manuel ou intensités d'un entraînement).",
        ),
        LapRead::Ordinary => None,
    }
}

/// Sustained blocks at or above 90 % of FTP, ended by 15 s easier than 75 %.
pub fn detect_efforts(time: &[f64], watts: &[f64], ftp: f64) -> Vec<Effort> {
    if ftp <= 0.0 {
        return Vec::new();
    }
    let hard = 0.90 * ftp;
    let easy = 0.75 * ftp;
    let n = time.len().min(watts.len());
    let mut efforts = Vec::new();
    let mut i = 1usize;
    while i < n {
        if watts[i] < hard {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        let mut easy_for = 0.0;
        i += 1;
        while i < n {
            let dt = (time[i] - time[i - 1]).clamp(0.0, 30.0);
            if watts[i] >= easy {
                easy_for = 0.0;
                end = i;
            } else {
                easy_for += dt;
                if easy_for >= 15.0 {
                    break;
                }
            }
            i += 1;
        }
        let duration = time[end] - time[start];
        if duration >= 20.0 {
            let avg = time_avg(&time[start..=end], &watts[start..=end]);
            efforts.push(Effort {
                duration_s: duration,
                avg_w: avg,
            });
        }
    }
    efforts
}

fn time_avg(time: &[f64], watts: &[f64]) -> f64 {
    if time.len() < 2 {
        return watts.first().copied().unwrap_or(0.0);
    }
    let mut acc = 0.0;
    let mut dt_sum = 0.0;
    for i in 1..time.len() {
        let dt = (time[i] - time[i - 1]).clamp(0.0, 30.0);
        acc += watts[i] * dt;
        dt_sum += dt;
    }
    if dt_sum > 0.0 { acc / dt_sum } else { 0.0 }
}

pub fn summarize_efforts(efforts: &[Effort]) -> Option<String> {
    if efforts.is_empty() {
        return None;
    }
    let similar = efforts.len() >= 2 && {
        let durs: Vec<f64> = efforts.iter().map(|e| e.duration_s).collect();
        let pwrs: Vec<f64> = efforts.iter().map(|e| e.avg_w).collect();
        let dm = median(&durs);
        let pm = median(&pwrs);
        dm > 0.0
            && pm > 0.0
            && durs.iter().all(|d| (d - dm).abs() / dm <= 0.25)
            && pwrs.iter().all(|p| (p - pm).abs() / pm <= 0.15)
    };
    if similar {
        let dm = median(&efforts.iter().map(|e| e.duration_s).collect::<Vec<_>>());
        let pm = median(&efforts.iter().map(|e| e.avg_w).collect::<Vec<_>>());
        return Some(format!(
            "Efforts détectés : {} × {} autour de {:.0} W.",
            efforts.len(),
            fmt_dur(dm),
            pm
        ));
    }
    let shown: Vec<String> = efforts
        .iter()
        .take(5)
        .map(|e| format!("{} à {:.0} W", fmt_dur(e.duration_s), e.avg_w))
        .collect();
    let extra = efforts.len().saturating_sub(5);
    let mut line = format!("Efforts détectés : {}.", shown.join(", "));
    if extra > 0 {
        line.push_str(&format!(" Et {extra} autre{}.", if extra > 1 { "s" } else { "" }));
    }
    Some(line)
}

pub fn peer_power_line(current_w: f64, previous: &[f64]) -> Option<String> {
    if previous.len() < 3 || current_w <= 0.0 {
        return None;
    }
    let med = median(previous);
    if med <= 0.0 {
        return None;
    }
    let pct = (current_w - med) / med * 100.0;
    Some(format!(
        "NP {:.0} W, médiane {:.0} W sur {} sorties précédentes de durée proche ({pct:+.0} %).",
        current_w,
        med,
        previous.len()
    ))
}

pub fn peer_pace_line(current_s_per_km: f64, previous: &[f64]) -> Option<String> {
    if previous.len() < 3 || current_s_per_km <= 0.0 {
        return None;
    }
    let med = median(previous);
    if med <= 0.0 {
        return None;
    }
    Some(format!(
        "Allure {}/km, médiane {}/km sur {} courses précédentes de distance proche.",
        fmt_pace(current_s_per_km),
        fmt_pace(med),
        previous.len()
    ))
}

pub fn decoupling_line(pct: f64) -> String {
    format!(
        "Découplage puissance/FC : {pct:+.1} % (positif : la FC monte à puissance égale)."
    )
}

pub fn pace_drift_line(time: &[f64], distance: &[f64]) -> Option<String> {
    let n = time.len().min(distance.len());
    if n < 2 {
        return None;
    }
    let total = time[n - 1] - time[0];
    if total < 1200.0 {
        return None;
    }
    let mid_t = time[0] + total / 2.0;
    let mid = (0..n).position(|i| time[i] >= mid_t)?;
    let pace = |a: usize, b: usize| {
        let dd = distance[b] - distance[a];
        let dt = time[b] - time[a];
        (dd > 100.0 && dt > 0.0).then(|| dt / dd * 1000.0)
    };
    let first = pace(0, mid)?;
    let second = pace(mid, n - 1)?;
    if first <= 0.0 {
        return None;
    }
    let pct = (second - first) / first * 100.0;
    Some(format!(
        "Dérive d'allure : {pct:+.0} % (positif : la seconde moitié est plus lente)."
    ))
}

pub fn detect_climbs(latlng: &[[f64; 2]], distance: &[f64], altitude: &[f64]) -> Vec<ClimbHit> {
    let n = latlng.len().min(distance.len()).min(altitude.len());
    if n < 2 || latlng.len() != distance.len() || distance.len() != altitude.len() {
        return Vec::new();
    }
    let grade: Vec<f64> = (0..n).map(|i| grade_over(distance, altitude, i)).collect();
    let mut hits = Vec::new();
    let mut i = 0;
    while i < n {
        if grade[i] < CLIMB_GRADE {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && grade[i] >= CLIMB_GRADE - 0.005 {
            i += 1;
        }
        let end = i.saturating_sub(1);
        if end <= start {
            continue;
        }
        let dist = distance[end] - distance[start];
        let gain = altitude[end] - altitude[start];
        if dist >= CLIMB_MIN_M && gain >= CLIMB_MIN_GAIN_M {
            hits.push(ClimbHit {
                start_lat: latlng[start][0],
                start_lng: latlng[start][1],
                end_lat: latlng[end][0],
                end_lng: latlng[end][1],
                distance_m: dist,
                gain_m: gain,
            });
        }
    }
    hits
}

fn grade_over(distance: &[f64], altitude: &[f64], i: usize) -> f64 {
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
        0.0
    } else {
        (altitude[k] - altitude[j]) / dd
    }
}

/// One representative per hill that was climbed at least twice.
pub fn repeated_climbs(hits: &[ClimbHit]) -> Vec<ClimbHit> {
    let mut clusters: Vec<Vec<&ClimbHit>> = Vec::new();
    for hit in hits {
        if let Some(cluster) = clusters.iter_mut().find(|c| same_climb(c[0], hit)) {
            cluster.push(hit);
        } else {
            clusters.push(vec![hit]);
        }
    }
    clusters
        .into_iter()
        .filter(|c| c.len() >= 2)
        .map(|c| {
            let n = c.len() as f64;
            ClimbHit {
                start_lat: c.iter().map(|h| h.start_lat).sum::<f64>() / n,
                start_lng: c.iter().map(|h| h.start_lng).sum::<f64>() / n,
                end_lat: c.iter().map(|h| h.end_lat).sum::<f64>() / n,
                end_lng: c.iter().map(|h| h.end_lng).sum::<f64>() / n,
                distance_m: c.iter().map(|h| h.distance_m).sum::<f64>() / n,
                gain_m: c.iter().map(|h| h.gain_m).sum::<f64>() / n,
            }
        })
        .collect()
}

fn same_climb(a: &ClimbHit, b: &ClimbHit) -> bool {
    haversine_m(a.start_lat, a.start_lng, b.start_lat, b.start_lng) < CLUSTER_M
        && haversine_m(a.end_lat, a.end_lng, b.end_lat, b.end_lng) < CLUSTER_M
        && a.distance_m > 0.0
        && (a.distance_m - b.distance_m).abs() / a.distance_m < 0.35
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

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = v.len() / 2;
    if v.len().is_multiple_of(2) {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    }
}

fn fmt_dur(secs: f64) -> String {
    let s = secs.round().max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn fmt_pace(sec_per_km: f64) -> String {
    let s = sec_per_km.round().max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[derive(sqlx::FromRow)]
struct RideRow {
    id: i64,
    sport_type: String,
    duration_s: i64,
    distance_m: f64,
    normalized_power_w: Option<f64>,
    workout_name: Option<String>,
}

pub struct DetectStats {
    pub segments: i64,
    pub insights: i64,
}

/// Rebuilds auto segments and per-activity readings from the stored streams,
/// then rematches every segment (starred and auto).
pub async fn recompute(pool: &SqlitePool) -> anyhow::Result<DetectStats> {
    let ftp = crate::metrics::get_ftp(pool).await?;
    let rides: Vec<RideRow> = sqlx::query_as(
        "SELECT id, sport_type, duration_s, distance_m, normalized_power_w, workout_name \
         FROM activities ORDER BY started_at ASC",
    )
    .fetch_all(pool)
    .await?;

    let mut climb_hits = Vec::new();
    let mut insights: Vec<(i64, String)> = Vec::new();
    let mut past_power: Vec<(i64, f64)> = Vec::new();
    let mut past_pace: Vec<(f64, f64)> = Vec::new();

    for ride in &rides {
        let streams_json: Option<String> =
            sqlx::query_scalar("SELECT streams_json FROM activity_streams WHERE activity_id = ?")
                .bind(ride.id)
                .fetch_optional(pool)
                .await?;
        let streams = streams_json.and_then(|s| serde_json::from_str::<Value>(&s).ok());
        let is_run = crate::metrics::is_run(&ride.sport_type);
        let is_ride = crate::metrics::is_ride(&ride.sport_type);

        if ride.sport_type == "Ride" {
            if let Some(v) = &streams {
                let latlng = crate::segments::latlng_array(v).unwrap_or_default();
                let distance = crate::segments::f64_array(v, "distance").unwrap_or_default();
                let altitude = crate::segments::f64_array(v, "altitude").unwrap_or_default();
                climb_hits.extend(detect_climbs(&latlng, &distance, &altitude));
            }
        }

        let mut lines = Vec::new();
        if let Some(name) = ride.workout_name.as_deref().filter(|s| !s.trim().is_empty()) {
            lines.push(format!("Séance enregistrée dans le fichier : {name}."));
        }
        if is_ride {
            if let Some(v) = &streams {
                if let (Some(time), Some(watts)) = (
                    crate::segments::f64_array(v, "time"),
                    crate::segments::f64_array(v, "watts"),
                ) {
                    if let Some(line) = summarize_efforts(&detect_efforts(&time, &watts, ftp)) {
                        lines.push(line);
                    }
                }
                if let Some(series) = crate::performance::Series::from_streams(v, true) {
                    if let Some(pct) = crate::performance::decoupling_pct(&series) {
                        lines.push(decoupling_line(pct));
                    }
                }
            }
            if let Some(np) = ride.normalized_power_w {
                let prev: Vec<f64> = past_power
                    .iter()
                    .rev()
                    .filter(|(dur, _)| {
                        let lo = (ride.duration_s as f64 * 0.75) as i64;
                        let hi = (ride.duration_s as f64 * 1.25) as i64;
                        *dur >= lo && *dur <= hi
                    })
                    .take(8)
                    .map(|(_, w)| *w)
                    .collect();
                if let Some(line) = peer_power_line(np, &prev) {
                    lines.push(line);
                }
                past_power.push((ride.duration_s, np));
            }
        }
        if is_run {
            if let Some(v) = &streams {
                if let (Some(time), Some(distance)) = (
                    crate::segments::f64_array(v, "time"),
                    crate::segments::f64_array(v, "distance"),
                ) {
                    if let Some(line) = pace_drift_line(&time, &distance) {
                        lines.push(line);
                    }
                }
            }
            if ride.distance_m > 0.0 && ride.duration_s > 0 {
                let pace = ride.duration_s as f64 / ride.distance_m * 1000.0;
                let prev: Vec<f64> = past_pace
                    .iter()
                    .rev()
                    .filter(|(dist, _)| {
                        let lo = ride.distance_m * 0.75;
                        let hi = ride.distance_m * 1.25;
                        *dist >= lo && *dist <= hi
                    })
                    .take(8)
                    .map(|(_, p)| *p)
                    .collect();
                if let Some(line) = peer_pace_line(pace, &prev) {
                    lines.push(line);
                }
                past_pace.push((ride.distance_m, pace));
            }
        }
        if !lines.is_empty() {
            insights.push((ride.id, lines.join("\n")));
        }
    }

    let climbs = repeated_climbs(&climb_hits);
    replace_detected(pool, &climbs, &insights).await?;
    crate::segments::run(pool).await?;

    let segments: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reference_segments WHERE source = 'auto'")
            .fetch_one(pool)
            .await?;
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM activity_insights")
        .fetch_one(pool)
        .await?;
    Ok(DetectStats {
        segments,
        insights: stored,
    })
}

async fn replace_detected(
    pool: &SqlitePool,
    climbs: &[ClimbHit],
    insights: &[(i64, String)],
) -> anyhow::Result<()> {
    delete_detected_rows(pool).await?;
    let existing: Vec<String> = sqlx::query_scalar("SELECT name FROM reference_segments")
        .fetch_all(pool)
        .await?;
    let mut taken: HashSet<String> = existing.into_iter().collect();
    for climb in climbs {
        let name = unique_name(climb, &mut taken);
        sqlx::query(
            "INSERT INTO reference_segments \
             (name, start_lat, start_lng, end_lat, end_lng, distance_m, elevation_gain_m, source) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'auto')",
        )
        .bind(&name)
        .bind(climb.start_lat)
        .bind(climb.start_lng)
        .bind(climb.end_lat)
        .bind(climb.end_lng)
        .bind(climb.distance_m)
        .bind(climb.gain_m)
        .execute(pool)
        .await?;
    }
    for (id, summary) in insights {
        sqlx::query("INSERT INTO activity_insights (activity_id, summary) VALUES (?, ?)")
            .bind(id)
            .bind(summary)
            .execute(pool)
            .await?;
    }
    Ok(())
}

fn unique_name(climb: &ClimbHit, taken: &mut HashSet<String>) -> String {
    let base = format!(
        "Côte {:.0} m · {:.1} km",
        climb.gain_m,
        climb.distance_m / 1000.0
    );
    if taken.insert(base.clone()) {
        return base;
    }
    let mut n = 2;
    loop {
        let name = format!("{base} ({n})");
        if taken.insert(name.clone()) {
            return name;
        }
        n += 1;
    }
}

async fn delete_detected_rows(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "DELETE FROM segment_efforts WHERE reference_segment_id IN \
         (SELECT id FROM reference_segments WHERE source = 'auto')",
    )
    .execute(pool)
    .await?;
    sqlx::query("DELETE FROM reference_segments WHERE source = 'auto'")
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM activity_insights")
        .execute(pool)
        .await?;
    Ok(())
}

/// Drops auto segments, their efforts and the stored readings, then rematches
/// whatever starred segments remain.
pub async fn clear(pool: &SqlitePool) -> anyhow::Result<()> {
    delete_detected_rows(pool).await?;
    crate::segments::run(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn km_laps(n: usize, meters: f64, trigger: &str) -> Vec<LapSample> {
        (0..n)
            .map(|_| LapSample {
                distance_m: meters,
                trigger: Some(trigger.into()),
                intensity: None,
            })
            .collect()
    }

    #[test]
    fn distance_auto_laps_are_not_a_workout() {
        assert_eq!(classify_laps(&km_laps(8, 1000.0, "distance")), LapRead::AutoSplit);
        // No trigger stored (typical of a file imported before this column):
        // eight equal kilometres still read as auto-splits.
        let plain = km_laps(8, 1000.0, "distance");
        let plain: Vec<_> = plain
            .into_iter()
            .map(|mut l| {
                l.trigger = None;
                l
            })
            .collect();
        assert_eq!(classify_laps(&plain), LapRead::AutoSplit);
    }

    #[test]
    fn manual_or_workout_laps_stay_structured() {
        let laps: Vec<_> = ["warmup", "active", "rest", "active", "cooldown"]
            .into_iter()
            .map(|intensity| LapSample {
                distance_m: 800.0,
                trigger: Some("manual".into()),
                intensity: Some(intensity.into()),
            })
            .collect();
        assert_eq!(classify_laps(&laps), LapRead::Structured);
        // Three laps and no trigger (the bilan test's shape) stay an ordinary
        // breakdown, so the lap table is still shown.
        let plain = km_laps(3, 2500.0, "manual");
        let plain: Vec<_> = plain
            .into_iter()
            .map(|mut lap| {
                lap.trigger = None;
                lap
            })
            .collect();
        assert_eq!(classify_laps(&plain), LapRead::Ordinary);
    }

    #[test]
    fn repeated_blocks_above_ftp_collapse_to_one_line() {
        let mut time = Vec::new();
        let mut watts = Vec::new();
        let mut t = 0.0;
        for _ in 0..4 {
            for _ in 0..40 {
                time.push(t);
                watts.push(120.0);
                t += 1.0;
            }
            for _ in 0..30 {
                time.push(t);
                watts.push(260.0);
                t += 1.0;
            }
        }
        let efforts = detect_efforts(&time, &watts, 250.0);
        assert_eq!(efforts.len(), 4);
        let line = summarize_efforts(&efforts).unwrap();
        assert!(line.contains("4 ×"), "{line}");
        assert!(line.contains("260"), "{line}");
    }

    #[test]
    fn a_hill_ridden_once_is_dropped_and_twice_is_kept() {
        let (latlng, distance, altitude, _) = climb_track();
        let hits = detect_climbs(&latlng, &distance, &altitude);
        assert_eq!(hits.len(), 1, "the synthetic hill should be one climb");
        assert!(hits[0].gain_m >= 20.0);
        assert!(repeated_climbs(&hits).is_empty());
        let kept = repeated_climbs(&[hits[0].clone(), hits[0].clone()]);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn peer_lines_need_three_earlier_rides() {
        assert!(peer_power_line(200.0, &[180.0, 190.0]).is_none());
        let line = peer_power_line(200.0, &[180.0, 190.0, 170.0]).unwrap();
        assert!(line.contains("200"));
        assert!(line.contains("3 sorties"));
    }

    fn climb_track() -> (Vec<[f64; 2]>, Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut latlng = Vec::new();
        let mut distance = Vec::new();
        let mut altitude = Vec::new();
        let mut time = Vec::new();
        let mut d = 0.0;
        let mut h = 200.0;
        while d <= 1000.0 {
            let grade = if (100.0..900.0).contains(&d) { 0.06 } else { 0.0 };
            latlng.push([49.0 + d / 111_195.0, 6.0]);
            distance.push(d);
            altitude.push(h);
            time.push(d / 5.0);
            d += 10.0;
            h += 10.0 * grade;
        }
        (latlng, distance, altitude, time)
    }

    #[tokio::test]
    async fn recompute_stores_one_segment_for_two_passes_and_clear_removes_it() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 250, 190, 50)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings (id, autodetect_enabled) VALUES (1, 1)")
            .execute(&pool)
            .await
            .unwrap();
        let (latlng, distance, altitude, time) = climb_track();
        let streams = serde_json::json!({
            "time": {"data": time},
            "distance": {"data": distance},
            "altitude": {"data": altitude},
            "latlng": {"data": latlng},
        })
        .to_string();
        for (id, started) in [(1, "2026-01-01T10:00:00Z"), (2, "2026-01-08T10:00:00Z")] {
            sqlx::query(
                "INSERT INTO activities (id, started_at, sport_type, duration_s, distance_m, elevation_gain_m) \
                 VALUES (?, ?, 'Ride', 200, 1000, 48)",
            )
            .bind(id)
            .bind(started)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO activity_streams (activity_id, streams_json) VALUES (?, ?)")
                .bind(id)
                .bind(&streams)
                .execute(&pool)
                .await
                .unwrap();
        }
        let stats = recompute(&pool).await.unwrap();
        assert_eq!(stats.segments, 1);
        assert_eq!(stats.insights, 0);
        clear(&pool).await.unwrap();
        let left: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM reference_segments WHERE source = 'auto'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(left, 0);
    }
}
