//! Per-window performance metrics computed from an activity's raw streams:
//! overall and per lap (avg/max/normalized power, HR, cadence, grade, zone
//! time...), best-power curve, variability index and power:HR decoupling.
//! Backs the detailed `/api/v1/activities/{id}` and `.../streams` endpoints.
//!
//! Streams are first resampled to one value per second (last value held
//! across gaps) so every window is a plain slice and every duration means
//! what it says, whatever the device's recording interval was.

use crate::workout::{hr_zone_of, Athlete};
use serde_json::Value;

/// One value per elapsed second since the activity start (index = second).
/// A channel is `None` when the activity doesn't carry it.
pub struct Series {
    pub len: usize,
    pub power: Option<Vec<f64>>,
    pub hr: Option<Vec<f64>>,
    pub cadence: Option<Vec<f64>>,
    pub altitude: Option<Vec<f64>>,
    pub distance_m: Option<Vec<f64>>,
}

fn channel(v: &Value, key: &str) -> Option<Vec<f64>> {
    v.get(key)?.get("data")?.as_array()?.iter().map(|x| x.as_f64()).collect()
}

/// Holds each sample's value until the next sample's timestamp.
fn resample_1hz(time: &[f64], values: &[f64], len: usize) -> Vec<f64> {
    let mut out = vec![0.0; len];
    let n = time.len().min(values.len());
    for i in 0..n {
        let start = time[i].max(0.0) as usize;
        let end = if i + 1 < n { (time[i + 1].max(0.0) as usize).max(start + 1) } else { start + 1 };
        for slot in out.iter_mut().take(end.min(len)).skip(start) {
            *slot = values[i];
        }
    }
    out
}

impl Series {
    /// `None` when there's no usable `time` stream. `with_power` is false for
    /// runs, whose power (if any) is ignored everywhere else in the app too.
    pub fn from_streams(streams: &Value, with_power: bool) -> Option<Series> {
        let time = channel(streams, "time")?;
        let last = *time.last()?;
        if time.len() < 2 || last < 1.0 {
            return None;
        }
        let len = last as usize + 1;
        let get = |key: &str| channel(streams, key).map(|v| resample_1hz(&time, &v, len));
        Some(Series {
            len,
            power: if with_power { get("watts") } else { None },
            hr: get("heartrate"),
            cadence: get("cadence"),
            altitude: get("altitude"),
            distance_m: get("distance"),
        })
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct WindowStats {
    pub duration_s: usize,
    pub distance_m: Option<f64>,
    pub avg_power_w: Option<f64>,
    pub max_power_w: Option<f64>,
    pub normalized_power_w: Option<f64>,
    pub avg_hr: Option<f64>,
    pub max_hr: Option<f64>,
    pub avg_cadence: Option<f64>,
    pub elevation_gain_m: Option<f64>,
    pub avg_grade_pct: Option<f64>,
    pub power_zone_s: Option<[i64; 7]>,
    pub hr_zone_s: Option<[i64; 5]>,
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

fn max(v: &[f64]) -> Option<f64> {
    v.iter().cloned().fold(None, |m, x| Some(m.map_or(x, |m: f64| m.max(x))))
}

/// Normalized power: 4th-power mean of the 30 s rolling average. Needs at
/// least 30 s of data, otherwise it's just not defined.
pub fn normalized_power(power: &[f64]) -> Option<f64> {
    const W: usize = 30;
    if power.len() < W {
        return None;
    }
    let mut sum: f64 = power[..W].iter().sum();
    let mut acc = (sum / W as f64).powi(4);
    let mut count = 1usize;
    for i in W..power.len() {
        sum += power[i] - power[i - W];
        acc += (sum / W as f64).powi(4);
        count += 1;
    }
    Some((acc / count as f64).powf(0.25))
}

/// Best average over any `window` consecutive seconds.
pub fn best_average(values: &[f64], window: usize) -> Option<f64> {
    if window == 0 || values.len() < window {
        return None;
    }
    let mut sum: f64 = values[..window].iter().sum();
    let mut best = sum;
    for i in window..values.len() {
        sum += values[i] - values[i - window];
        best = best.max(sum);
    }
    Some(best / window as f64)
}

/// Stats over `[start_s, end_s)` (clamped to the activity).
pub fn window_stats(s: &Series, start_s: usize, end_s: usize, athlete: &Athlete) -> WindowStats {
    let end = end_s.min(s.len);
    let start = start_s.min(end);
    if end <= start {
        return WindowStats::default();
    }
    fn slice_of(v: &Option<Vec<f64>>, a: usize, b: usize) -> Option<&[f64]> {
        v.as_ref().map(|v| &v[a..b])
    }

    let power = slice_of(&s.power, start, end);
    let hr = slice_of(&s.hr, start, end);
    let alt = slice_of(&s.altitude, start, end);
    let dist = s.distance_m.as_ref();

    let distance_m = dist.map(|d| d[end - 1] - d[start]);
    let elevation_gain_m = alt.map(|a| {
        // Sampled every 5 s: the altitude stream jitters by a metre or two
        // second to second, which would otherwise add up to phantom climbing.
        a.iter().step_by(5).collect::<Vec<_>>().windows(2).map(|w| (w[1] - w[0]).max(0.0)).sum()
    });
    let avg_grade_pct = match (alt, distance_m) {
        (Some(a), Some(d)) if d > 50.0 => Some((a[a.len() - 1] - a[0]) / d * 100.0),
        _ => None,
    };

    WindowStats {
        duration_s: end - start,
        distance_m,
        avg_power_w: power.and_then(mean),
        max_power_w: power.and_then(max),
        normalized_power_w: power.and_then(normalized_power),
        avg_hr: hr.and_then(mean),
        max_hr: hr.and_then(max),
        avg_cadence: slice_of(&s.cadence, start, end).and_then(mean),
        elevation_gain_m,
        avg_grade_pct,
        power_zone_s: power.map(|p| {
            let mut z = [0i64; 7];
            for w in p {
                z[crate::metrics::power_zone(*w, athlete.ftp_watts) - 1] += 1;
            }
            z
        }),
        hr_zone_s: hr.map(|h| {
            let mut z = [0i64; 5];
            for bpm in h {
                // Zero = no reading yet, not a real resting beat.
                if *bpm > 0.0 {
                    z[hr_zone_of(*bpm, athlete) as usize - 1] += 1;
                }
            }
            z
        }),
    }
}

/// Best average power for the usual benchmark durations (null when the
/// activity is shorter than the window).
pub fn power_curve(power: &[f64]) -> Vec<(&'static str, Option<f64>)> {
    [("5s", 5), ("15s", 15), ("30s", 30), ("1min", 60), ("5min", 300), ("10min", 600), ("20min", 1200), ("60min", 3600)]
        .into_iter()
        .map(|(label, secs)| (label, best_average(power, secs)))
        .collect()
}

/// Power:HR decoupling — how much the power/HR ratio drops from the first to
/// the second half (positive = HR drifting up for the same power: fatigue,
/// heat, dehydration). Only for efforts of at least 20 minutes.
pub fn decoupling_pct(s: &Series) -> Option<f64> {
    let (power, hr) = (s.power.as_ref()?, s.hr.as_ref()?);
    if s.len < 1200 {
        return None;
    }
    let half = s.len / 2;
    let efficiency = |a: usize, b: usize| -> Option<f64> {
        let np = normalized_power(&power[a..b])?;
        let avg_hr = mean(&hr[a..b])?;
        (avg_hr > 0.0).then(|| np / avg_hr)
    };
    let (first, second) = (efficiency(0, half)?, efficiency(half, s.len)?);
    (first > 0.0).then(|| (first - second) / first * 100.0)
}

/// Downsampled series for charts: the 1 Hz data averaged over `step_s`-second
/// buckets inside `[from_s, to_s)`. Speed comes from the distance covered in
/// each bucket.
pub struct Downsampled {
    pub time_s: Vec<f64>,
    pub distance_km: Option<Vec<f64>>,
    pub speed_kmh: Option<Vec<f64>>,
    pub power_w: Option<Vec<f64>>,
    pub hr_bpm: Option<Vec<f64>>,
    pub cadence: Option<Vec<f64>>,
    pub altitude_m: Option<Vec<f64>>,
}

pub fn downsample(s: &Series, from_s: usize, to_s: usize, step_s: usize) -> Downsampled {
    let step = step_s.max(1);
    let end = to_s.min(s.len);
    let mut out = Downsampled {
        time_s: Vec::new(),
        distance_km: s.distance_m.as_ref().map(|_| Vec::new()),
        speed_kmh: s.distance_m.as_ref().map(|_| Vec::new()),
        power_w: s.power.as_ref().map(|_| Vec::new()),
        hr_bpm: s.hr.as_ref().map(|_| Vec::new()),
        cadence: s.cadence.as_ref().map(|_| Vec::new()),
        altitude_m: s.altitude.as_ref().map(|_| Vec::new()),
    };
    let mut a = from_s.min(end);
    while a < end {
        let b = (a + step).min(end);
        out.time_s.push(a as f64);
        let avg = |v: &Option<Vec<f64>>, dst: &mut Option<Vec<f64>>| {
            if let (Some(v), Some(dst)) = (v.as_ref(), dst.as_mut()) {
                dst.push(mean(&v[a..b]).unwrap_or(0.0));
            }
        };
        avg(&s.power, &mut out.power_w);
        avg(&s.hr, &mut out.hr_bpm);
        avg(&s.cadence, &mut out.cadence);
        avg(&s.altitude, &mut out.altitude_m);
        if let (Some(d), Some(km), Some(sp)) = (s.distance_m.as_ref(), out.distance_km.as_mut(), out.speed_kmh.as_mut()) {
            km.push(d[a] / 1000.0);
            let dt = (b - a) as f64;
            let prev = if a > 0 { d[a - 1] } else { d[a] };
            sp.push(((d[b - 1] - prev).max(0.0) / dt.max(1.0)) * 3.6);
        }
        a = b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn athlete() -> Athlete {
        Athlete { ftp_watts: 200.0, hr_rest: 50.0, hr_max: 190.0, run_threshold_pace_s: 0.0 }
    }

    /// 10 minutes at 1 Hz: first 5 min at 100 W / 120 bpm, last 5 at 300 W / 160 bpm,
    /// 10 m/s of distance per second, climbing 0.05 m per metre.
    fn two_block_streams() -> Value {
        let n = 600;
        let time: Vec<usize> = (0..n).collect();
        let watts: Vec<f64> = (0..n).map(|t| if t < 300 { 100.0 } else { 300.0 }).collect();
        let hr: Vec<f64> = (0..n).map(|t| if t < 300 { 120.0 } else { 160.0 }).collect();
        let dist: Vec<f64> = (0..n).map(|t| t as f64 * 10.0).collect();
        let alt: Vec<f64> = (0..n).map(|t| 100.0 + t as f64 * 0.5).collect();
        json!({"time": {"data": time}, "watts": {"data": watts}, "heartrate": {"data": hr},
               "distance": {"data": dist}, "altitude": {"data": alt}})
    }

    #[test]
    fn lap_windows_split_power_hr_and_climb_correctly() {
        let s = Series::from_streams(&two_block_streams(), true).unwrap();
        let first = window_stats(&s, 0, 300, &athlete());
        let second = window_stats(&s, 300, 600, &athlete());

        assert_eq!(first.avg_power_w, Some(100.0));
        assert_eq!(second.avg_power_w, Some(300.0));
        assert_eq!(second.max_power_w, Some(300.0));
        assert_eq!(first.avg_hr, Some(120.0));
        assert_eq!(second.max_hr, Some(160.0));
        // Constant effort: NP equals the average.
        assert!((second.normalized_power_w.unwrap() - 300.0).abs() < 1e-9);
        // 5 % grade (0.5 m per 10 m), ~150 m climbed per 300 s.
        assert!((first.avg_grade_pct.unwrap() - 5.0).abs() < 0.2);
        assert!((first.elevation_gain_m.unwrap() - 150.0).abs() < 3.0);
        assert!((first.distance_m.unwrap() - 2990.0).abs() < 1.0);
        // Zone time: 100 W = Z1 (<55 %... of 200 W = 110), 300 W = Z7.
        assert_eq!(first.power_zone_s.unwrap()[0], 300);
        assert_eq!(second.power_zone_s.unwrap()[6], 300);
        // 120 bpm = 50 % of reserve = Z1; 160 bpm = 79 % = Z3.
        assert_eq!(first.hr_zone_s.unwrap()[0], 300);
        assert_eq!(second.hr_zone_s.unwrap()[2], 300);
    }

    #[test]
    fn runs_ignore_power_channel() {
        let s = Series::from_streams(&two_block_streams(), false).unwrap();
        assert!(s.power.is_none());
        let w = window_stats(&s, 0, 300, &athlete());
        assert_eq!(w.avg_power_w, None);
        assert_eq!(w.power_zone_s, None);
        assert_eq!(w.avg_hr, Some(120.0));
    }

    #[test]
    fn normalized_power_weights_variability_above_the_average() {
        // Alternating 60 s at 100 W and 60 s at 300 W: avg 200, NP higher.
        let p: Vec<f64> = (0..1200).map(|t| if (t / 60) % 2 == 0 { 100.0 } else { 300.0 }).collect();
        let np = normalized_power(&p).unwrap();
        assert!(np > 215.0 && np < 260.0, "got {np}");
        assert_eq!(normalized_power(&p[..10]), None);
    }

    #[test]
    fn power_curve_finds_the_best_window() {
        let mut p = vec![100.0; 1000];
        for v in p.iter_mut().skip(400).take(60) {
            *v = 400.0;
        }
        assert_eq!(best_average(&p, 60), Some(400.0));
        let curve = power_curve(&p);
        assert_eq!(curve.iter().find(|(l, _)| *l == "1min").unwrap().1, Some(400.0));
        assert_eq!(curve.iter().find(|(l, _)| *l == "5s").unwrap().1, Some(400.0));
        // Longer than the activity: not available.
        assert_eq!(curve.iter().find(|(l, _)| *l == "20min").unwrap().1, None);
    }

    #[test]
    fn decoupling_is_positive_when_hr_drifts_up_at_constant_power() {
        let n = 3600;
        let time: Vec<usize> = (0..n).collect();
        let watts = vec![200.0; n];
        let hr: Vec<f64> = (0..n).map(|t| if t < n / 2 { 140.0 } else { 154.0 }).collect();
        let s = Series::from_streams(&json!({"time": {"data": time}, "watts": {"data": watts}, "heartrate": {"data": hr}}), true).unwrap();
        let d = decoupling_pct(&s).unwrap();
        assert!((d - (1.0 - 140.0 / 154.0) * 100.0).abs() < 0.1, "got {d}");

        // Too short to be meaningful.
        let short = Series::from_streams(&json!({"time": {"data": [0, 1, 2]}, "watts": {"data": [1, 1, 1]}, "heartrate": {"data": [1, 1, 1]}}), true).unwrap();
        assert_eq!(decoupling_pct(&short), None);
    }

    #[test]
    fn resampling_holds_values_across_recording_gaps() {
        // Samples every 5 s: 0 s -> 100 W, 5 s -> 200 W, 10 s -> 300 W.
        let streams = json!({"time": {"data": [0, 5, 10]}, "watts": {"data": [100, 200, 300]}});
        let s = Series::from_streams(&streams, true).unwrap();
        assert_eq!(s.len, 11);
        let p = s.power.unwrap();
        assert_eq!(&p[0..5], &[100.0; 5]);
        assert_eq!(&p[5..10], &[200.0; 5]);
        assert_eq!(p[10], 300.0);
    }

    #[test]
    fn downsample_buckets_and_derives_speed() {
        let s = Series::from_streams(&two_block_streams(), true).unwrap();
        let d = downsample(&s, 0, 600, 60);
        assert_eq!(d.time_s.len(), 10);
        let power = d.power_w.unwrap();
        assert_eq!(power[0], 100.0);
        assert_eq!(power[9], 300.0);
        // 10 m/s = 36 km/h.
        let speed = d.speed_kmh.unwrap();
        assert!((speed[3] - 36.0).abs() < 0.5, "got {}", speed[3]);
        // A window inside the activity.
        let w = downsample(&s, 300, 360, 30);
        assert_eq!(w.time_s, vec![300.0, 330.0]);
    }
}
