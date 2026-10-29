//! Elevation profile and estimated gradient for a planned route (GPX file).
//!
//! Raw GPX elevation is noisy (GPS/barometer jitter of a few metres), and a
//! gradient computed point to point swings wildly on it — 3 m of noise over a
//! 10 m step reads as 30 %. So the route is resampled at an even spacing, the
//! elevation smoothed over ~120 m, and the gradient taken over a ~100 m
//! window of that smoothed series. Climbs/descents are summed on the smoothed
//! series too, so jitter on flat sections doesn't inflate D+.

pub struct RouteProfile {
    pub name: Option<String>,
    /// Resampled track, parallel to the arrays below (map polyline + chart).
    pub latlng: Vec<[f64; 2]>,
    pub dist_km: Vec<f64>,
    /// Empty when the file carries no elevation at all.
    pub ele_m: Vec<f64>,
    pub grade_pct: Vec<f64>,
    pub total_km: f64,
    pub ascent_m: f64,
    pub descent_m: f64,
    pub min_ele_m: f64,
    pub max_ele_m: f64,
    pub max_grade_pct: f64,
    pub min_grade_pct: f64,
}

impl RouteProfile {
    pub fn has_elevation(&self) -> bool {
        !self.ele_m.is_empty()
    }
}

const MAX_SAMPLES: f64 = 3000.0;
const MIN_STEP_M: f64 = 10.0;
const SMOOTH_WINDOW_M: f64 = 120.0;
const GRADE_WINDOW_M: f64 = 100.0;
const MAX_ABS_GRADE_PCT: f64 = 30.0;
const CLIMB_THRESHOLD_M: f64 = 2.0;

fn haversine_m(a: [f64; 2], b: [f64; 2]) -> f64 {
    const EARTH_RADIUS_M: f64 = 6_371_000.0;
    let dlat = (b[0] - a[0]).to_radians();
    let dlng = (b[1] - a[1]).to_radians();
    let h = (dlat / 2.0).sin().powi(2)
        + a[0].to_radians().cos() * b[0].to_radians().cos() * (dlng / 2.0).sin().powi(2);
    EARTH_RADIUS_M * 2.0 * h.sqrt().atan2((1.0 - h).sqrt())
}

/// Reads a GPX file: tracks first (what most exports contain), falling back
/// to `<rte>` routes (what planners such as Komoot/RideWithGPS can export).
pub fn parse_gpx(bytes: &[u8]) -> anyhow::Result<RouteProfile> {
    let gpx = gpx::read(bytes)?;
    let mut points: Vec<([f64; 2], Option<f64>)> = gpx
        .tracks
        .iter()
        .flat_map(|t| t.segments.iter())
        .flat_map(|s| s.points.iter())
        .map(|p| ([p.point().y(), p.point().x()], p.elevation))
        .collect();
    if points.len() < 2 {
        points = gpx
            .routes
            .iter()
            .flat_map(|r| r.points.iter())
            .map(|p| ([p.point().y(), p.point().x()], p.elevation))
            .collect();
    }
    anyhow::ensure!(
        points.len() >= 2,
        "le fichier GPX contient moins de 2 points de trace ou d'itinéraire"
    );

    let name = gpx
        .metadata
        .as_ref()
        .and_then(|m| m.name.clone())
        .or_else(|| gpx.tracks.iter().find_map(|t| t.name.clone()))
        .or_else(|| gpx.routes.iter().find_map(|r| r.name.clone()))
        .filter(|n| !n.trim().is_empty());

    let mut profile = build_profile(&points)?;
    profile.name = name;
    Ok(profile)
}

/// Fills elevation gaps (nearest known value) so a few untagged points don't
/// punch holes in the profile; `None` if no point has elevation at all.
fn fill_elevation(points: &[([f64; 2], Option<f64>)]) -> Option<Vec<f64>> {
    let first = points.iter().find_map(|p| p.1)?;
    let mut last = first;
    Some(
        points
            .iter()
            .map(|p| {
                last = p.1.unwrap_or(last);
                last
            })
            .collect(),
    )
}

pub fn build_profile(points: &[([f64; 2], Option<f64>)]) -> anyhow::Result<RouteProfile> {
    // Cumulative distance, dropping points that don't move (duplicates would
    // make the interpolation below divide by zero).
    let mut kept: Vec<usize> = vec![0];
    let mut cum: Vec<f64> = vec![0.0];
    for i in 1..points.len() {
        let prev = *kept.last().unwrap();
        let d = haversine_m(points[prev].0, points[i].0);
        if d > 0.5 {
            kept.push(i);
            cum.push(cum.last().unwrap() + d);
        }
    }
    anyhow::ensure!(kept.len() >= 2, "le tracé ne contient aucun déplacement");
    let total_m = *cum.last().unwrap();

    let elevations = fill_elevation(points);
    let ll: Vec<[f64; 2]> = kept.iter().map(|&i| points[i].0).collect();
    let el: Option<Vec<f64>> = elevations.map(|e| kept.iter().map(|&i| e[i]).collect());

    let step = (total_m / MAX_SAMPLES).max(MIN_STEP_M);
    let n = (total_m / step).floor() as usize + 1;
    let mut dist_m = Vec::with_capacity(n + 1);
    for i in 0..n {
        dist_m.push(i as f64 * step);
    }
    if total_m - dist_m.last().unwrap() > 1.0 {
        dist_m.push(total_m);
    }

    // Linear interpolation of position/elevation at each sample distance.
    let mut latlng = Vec::with_capacity(dist_m.len());
    let mut ele_raw = Vec::with_capacity(dist_m.len());
    let mut seg = 0usize;
    for &d in &dist_m {
        while seg + 2 < cum.len() && cum[seg + 1] < d {
            seg += 1;
        }
        let span = cum[seg + 1] - cum[seg];
        let t = if span > 0.0 {
            ((d - cum[seg]) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        latlng.push([
            ll[seg][0] + (ll[seg + 1][0] - ll[seg][0]) * t,
            ll[seg][1] + (ll[seg + 1][1] - ll[seg][1]) * t,
        ]);
        if let Some(el) = &el {
            ele_raw.push(el[seg] + (el[seg + 1] - el[seg]) * t);
        }
    }

    let dist_km: Vec<f64> = dist_m.iter().map(|d| d / 1000.0).collect();
    let mut profile = RouteProfile {
        name: None,
        latlng,
        dist_km,
        ele_m: Vec::new(),
        grade_pct: Vec::new(),
        total_km: total_m / 1000.0,
        ascent_m: 0.0,
        descent_m: 0.0,
        min_ele_m: 0.0,
        max_ele_m: 0.0,
        max_grade_pct: 0.0,
        min_grade_pct: 0.0,
    };
    if el.is_none() {
        return Ok(profile);
    }

    let half_smooth = ((SMOOTH_WINDOW_M / step / 2.0).round() as usize).max(1);
    let ele = moving_average(&ele_raw, half_smooth);

    let half_grade = ((GRADE_WINDOW_M / step / 2.0).round() as usize).max(1);
    let last = ele.len() - 1;
    let grade: Vec<f64> = (0..ele.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(half_grade), (i + half_grade).min(last));
            let run = dist_m[b] - dist_m[a];
            if run > 0.0 {
                ((ele[b] - ele[a]) / run * 100.0).clamp(-MAX_ABS_GRADE_PCT, MAX_ABS_GRADE_PCT)
            } else {
                0.0
            }
        })
        .collect();

    // Climb/descent with hysteresis: a change only counts once elevation has
    // moved CLIMB_THRESHOLD_M away from the last counted level, so residual
    // ripple after smoothing never accumulates into phantom D+.
    let mut anchor = ele[0];
    for &e in &ele {
        if e - anchor >= CLIMB_THRESHOLD_M {
            profile.ascent_m += e - anchor;
            anchor = e;
        } else if anchor - e >= CLIMB_THRESHOLD_M {
            profile.descent_m += anchor - e;
            anchor = e;
        }
    }
    profile.min_ele_m = ele.iter().cloned().fold(f64::INFINITY, f64::min);
    profile.max_ele_m = ele.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    profile.max_grade_pct = grade.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    profile.min_grade_pct = grade.iter().cloned().fold(f64::INFINITY, f64::min);
    profile.ele_m = ele;
    profile.grade_pct = grade;
    Ok(profile)
}

/// Centered moving average with the window shrinking at the ends (so the
/// first/last samples keep their real values' neighbourhood instead of being
/// dragged toward zero).
fn moving_average(values: &[f64], half: usize) -> Vec<f64> {
    (0..values.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half).min(values.len() - 1);
            values[lo..=hi].iter().sum::<f64>() / (hi - lo + 1) as f64
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A straight line due north at ~111.2 km/degree with elevation rising by
    /// `slope` metres per metre of distance.
    fn line(total_m: f64, slope: f64, with_ele: bool) -> Vec<([f64; 2], Option<f64>)> {
        let n = 400;
        (0..=n)
            .map(|i| {
                let d = total_m * i as f64 / n as f64;
                (
                    [45.0 + d / 111_195.0, 6.0],
                    with_ele.then_some(500.0 + slope * d),
                )
            })
            .collect()
    }

    #[test]
    fn constant_slope_gives_that_gradient_and_matching_climb() {
        let p = build_profile(&line(10_000.0, 0.05, true)).unwrap();
        assert!((p.total_km - 10.0).abs() < 0.05);
        // Interior samples sit exactly on the 5 % slope.
        let mid = p.grade_pct[p.grade_pct.len() / 2];
        assert!((mid - 5.0).abs() < 0.1, "got {mid}");
        assert!((p.ascent_m - 500.0).abs() < 5.0, "ascent {}", p.ascent_m);
        assert!(p.descent_m < 1.0);
        assert!((p.max_ele_m - 1000.0).abs() < 5.0);
    }

    #[test]
    fn descent_is_negative_gradient() {
        let p = build_profile(&line(5_000.0, -0.08, true)).unwrap();
        let mid = p.grade_pct[p.grade_pct.len() / 2];
        assert!((mid + 8.0).abs() < 0.1, "got {mid}");
        assert!(p.ascent_m < 1.0 && (p.descent_m - 400.0).abs() < 5.0);
    }

    #[test]
    fn gps_elevation_noise_on_flat_ground_does_not_become_steep_gradient() {
        let mut pts = line(10_000.0, 0.0, true);
        for (i, p) in pts.iter_mut().enumerate() {
            // +/-2 m alternating jitter every ~25 m.
            p.1 = p.1.map(|e| e + if i % 2 == 0 { 2.0 } else { -2.0 });
        }
        let p = build_profile(&pts).unwrap();
        assert!(
            p.max_grade_pct < 1.5 && p.min_grade_pct > -1.5,
            "{} {}",
            p.min_grade_pct,
            p.max_grade_pct
        );
        assert!(
            p.ascent_m < 60.0,
            "smoothing should keep D+ small, got {}",
            p.ascent_m
        );
    }

    #[test]
    fn missing_elevation_yields_a_map_only_profile() {
        let p = build_profile(&line(3_000.0, 0.0, false)).unwrap();
        assert!(!p.has_elevation());
        assert!(p.grade_pct.is_empty());
        assert!(p.latlng.len() > 2 && p.latlng.len() == p.dist_km.len());
    }

    #[test]
    fn stationary_track_is_rejected() {
        let pts = vec![([45.0, 6.0], Some(100.0)), ([45.0, 6.0], Some(100.0))];
        assert!(build_profile(&pts).is_err());
    }

    #[test]
    fn parses_gpx_routes_when_there_is_no_track() {
        let gpx = r#"<?xml version="1.0"?>
<gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
  <rte><name>Col test</name>
    <rtept lat="45.000" lon="6.000"><ele>500</ele></rtept>
    <rtept lat="45.010" lon="6.000"><ele>600</ele></rtept>
    <rtept lat="45.020" lon="6.000"><ele>700</ele></rtept>
  </rte>
</gpx>"#;
        let p = parse_gpx(gpx.as_bytes()).unwrap();
        assert_eq!(p.name.as_deref(), Some("Col test"));
        assert!((p.total_km - 2.22).abs() < 0.02);
        assert!(p.has_elevation());
        // ~100 m per 1.11 km = ~9 %.
        assert!((p.grade_pct[p.grade_pct.len() / 2] - 9.0).abs() < 0.5);
    }
}
