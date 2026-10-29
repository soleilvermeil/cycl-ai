//! Parses a manually-uploaded `.fit` or `.gpx` file into the same shape
//! `sync::run` builds from a Strava activity, so everything downstream
//! (metrics, zones, route thumbnail, detail-page charts) works unchanged
//! regardless of where the ride came from — see `web::import`.
//!
//! FIT is the richer source (native power/HR/cadence/laps, like a Strava
//! upload) and is preferred when available. GPX is a fallback: it only
//! carries GPS position/elevation/time, so power, heart rate, cadence and
//! laps are always absent from a GPX import — no equivalent field exists in
//! the format itself.

use serde_json::{Value as Json, json};

pub struct ParsedLap {
    pub lap_index: i64,
    pub elapsed_time_s: i64,
    pub distance_m: f64,
    pub avg_power_w: Option<f64>,
    pub avg_hr: Option<f64>,
    pub max_hr: Option<f64>,
    /// FIT `lap_trigger`: `manual`, `distance`, `time`, …
    pub lap_trigger: Option<String>,
    /// FIT `intensity`: `active`, `rest`, `warmup`, `cooldown`, …
    pub intensity: Option<String>,
}

pub struct ParsedActivity {
    pub started_at: String, // RFC3339, same convention as strava::models::SummaryActivity
    pub duration_s: i64,
    pub distance_m: f64,
    pub elevation_gain_m: f64,
    pub avg_power_w: Option<f64>,
    pub normalized_power_w: Option<f64>,
    pub avg_hr: Option<f64>,
    pub max_hr: Option<f64>,
    pub streams_json: Json,
    pub laps: Vec<ParsedLap>,
    /// FIT workout name, when the file carried a workout message.
    pub workout_name: Option<String>,
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

/// Builds the streams JSON in the same `{"key": {"data": [...]}}` shape
/// Strava's `get_streams` returns (see `strava::StravaClient::get_streams`)
/// — everything that reads `activity_streams.streams_json` downstream
/// expects exactly this shape. Only keys with data are included, same as
/// Strava omitting streams for sensors that weren't present.
fn build_streams_json(
    time: Vec<f64>,
    latlng: Vec<[f64; 2]>,
    distance: Vec<f64>,
    altitude: Vec<f64>,
    watts: Vec<f64>,
    heartrate: Vec<f64>,
    cadence: Vec<f64>,
) -> Json {
    let mut streams = serde_json::Map::new();
    streams.insert("time".into(), json!({"data": time}));
    if !latlng.is_empty() {
        streams.insert("latlng".into(), json!({"data": latlng}));
    }
    if !distance.is_empty() {
        streams.insert("distance".into(), json!({"data": distance}));
    }
    if !altitude.is_empty() {
        streams.insert("altitude".into(), json!({"data": altitude}));
    }
    if !watts.is_empty() {
        streams.insert("watts".into(), json!({"data": watts}));
    }
    if !heartrate.is_empty() {
        streams.insert("heartrate".into(), json!({"data": heartrate}));
    }
    if !cadence.is_empty() {
        streams.insert("cadence".into(), json!({"data": cadence}));
    }
    Json::Object(streams)
}

mod fit {
    use super::{ParsedActivity, ParsedLap};
    use fitparser::profile::MesgNum;
    use fitparser::{FitDataRecord, Value};

    /// FIT's scaled numeric field types all collapse to a plain f64 here —
    /// the crate already applies the FIT profile's scale/offset, so a
    /// "power" or "distance" field comes back in real units (W, m).
    fn value_f64(v: &Value) -> Option<f64> {
        use Value::*;
        match v {
            Byte(x) | UInt8(x) | UInt8z(x) => Some(*x as f64),
            Enum(x) => Some(*x as f64),
            SInt8(x) => Some(*x as f64),
            SInt16(x) => Some(*x as f64),
            UInt16(x) | UInt16z(x) => Some(*x as f64),
            SInt32(x) => Some(*x as f64),
            UInt32(x) | UInt32z(x) => Some(*x as f64),
            SInt64(x) => Some(*x as f64),
            UInt64(x) | UInt64z(x) => Some(*x as f64),
            Float32(x) => Some(*x as f64),
            Float64(x) => Some(*x),
            _ => None,
        }
    }

    fn field_f64(rec: &FitDataRecord, name: &str) -> Option<f64> {
        rec.fields()
            .iter()
            .find(|f| f.name() == name)
            .and_then(|f| value_f64(f.value()))
    }

    /// `field_f64`, trying each name in order — some fields go by different
    /// names across FIT profile versions/devices (e.g. normalized power).
    fn field_f64_any(rec: &FitDataRecord, names: &[&str]) -> Option<f64> {
        names.iter().find_map(|n| field_f64(rec, n))
    }

    fn field_timestamp_epoch(rec: &FitDataRecord, name: &str) -> Option<i64> {
        rec.fields()
            .iter()
            .find(|f| f.name() == name)
            .and_then(|f| match f.value() {
                Value::Timestamp(dt) => Some(dt.timestamp()),
                _ => None,
            })
    }

    fn field_u8(rec: &FitDataRecord, name: &str) -> Option<u8> {
        rec.fields()
            .iter()
            .find(|f| f.name() == name)
            .and_then(|f| match f.value() {
                Value::Enum(x) | Value::UInt8(x) | Value::Byte(x) | Value::UInt8z(x) => Some(*x),
                _ => None,
            })
    }

    fn field_string(rec: &FitDataRecord, name: &str) -> Option<String> {
        rec.fields()
            .iter()
            .find(|f| f.name() == name)
            .and_then(|f| match f.value() {
                Value::String(s) => {
                    let trimmed = s.trim().trim_matches('\0').to_string();
                    (!trimmed.is_empty()).then_some(trimmed)
                }
                _ => None,
            })
    }

    fn lap_trigger_name(value: u8) -> Option<&'static str> {
        Some(match value {
            0 => "manual",
            1 => "time",
            2 => "distance",
            3 => "position_start",
            4 => "position_lap",
            5 => "position_waypoint",
            6 => "session_end",
            7 => "fitness_equipment",
            _ => return None,
        })
    }

    fn intensity_name(value: u8) -> Option<&'static str> {
        Some(match value {
            0 => "active",
            1 => "rest",
            2 => "warmup",
            3 => "cooldown",
            4 => "recovery",
            5 => "interval",
            6 => "other",
            _ => return None,
        })
    }

    /// FIT stores lat/lng as semicircles (int32 range), not degrees.
    fn semicircle_to_deg(raw: f64) -> f64 {
        raw * (180.0 / 2f64.powi(31))
    }

    pub fn parse(bytes: &[u8]) -> anyhow::Result<ParsedActivity> {
        let records = fitparser::from_bytes(bytes)?;

        let mut time = Vec::new();
        let mut latlng = Vec::new();
        let mut distance = Vec::new();
        let mut altitude = Vec::new();
        let mut watts = Vec::new();
        let mut heartrate = Vec::new();
        let mut cadence = Vec::new();
        let (mut have_distance, mut have_altitude, mut have_watts, mut have_hr, mut have_cadence) =
            (false, false, false, false, false);

        let mut first_ts: Option<i64> = None;
        let mut laps = Vec::new();
        let mut workout_name: Option<String> = None;

        let mut session_start_epoch: Option<i64> = None;
        let mut session_duration_s: Option<i64> = None;
        let mut session_distance_m: Option<f64> = None;
        let mut session_ascent_m: Option<f64> = None;
        let mut session_avg_power: Option<f64> = None;
        let mut session_np: Option<f64> = None;
        let mut session_avg_hr: Option<f64> = None;
        let mut session_max_hr: Option<f64> = None;

        for rec in &records {
            match rec.kind() {
                MesgNum::Record => {
                    let Some(ts) = field_timestamp_epoch(rec, "timestamp") else {
                        continue;
                    };
                    let start = *first_ts.get_or_insert(ts);
                    time.push((ts - start) as f64);

                    if let (Some(lat), Some(lng)) = (
                        field_f64(rec, "position_lat"),
                        field_f64(rec, "position_long"),
                    ) {
                        latlng.push([semicircle_to_deg(lat), semicircle_to_deg(lng)]);
                    }

                    // Positionally aligned with `time` (0.0-filled on a rare
                    // single-sample sensor dropout) since metrics::run's
                    // zone computation walks `time`/`watts` by shared index
                    // — unlike `latlng` above, which only feeds map/
                    // thumbnail rendering and tolerates being shorter.
                    let d = field_f64_any(rec, &["distance"]);
                    have_distance |= d.is_some();
                    distance.push(d.unwrap_or(0.0));

                    let alt = field_f64_any(rec, &["enhanced_altitude", "altitude"]);
                    have_altitude |= alt.is_some();
                    altitude.push(alt.unwrap_or(0.0));

                    let w = field_f64(rec, "power");
                    have_watts |= w.is_some();
                    watts.push(w.unwrap_or(0.0));

                    let hr = field_f64(rec, "heart_rate");
                    have_hr |= hr.is_some();
                    heartrate.push(hr.unwrap_or(0.0));

                    let cad = field_f64(rec, "cadence");
                    have_cadence |= cad.is_some();
                    cadence.push(cad.unwrap_or(0.0));
                }
                MesgNum::Lap => {
                    let elapsed = field_f64(rec, "total_elapsed_time").unwrap_or(0.0);
                    laps.push(ParsedLap {
                        lap_index: laps.len() as i64 + 1,
                        elapsed_time_s: elapsed.round() as i64,
                        distance_m: field_f64(rec, "total_distance").unwrap_or(0.0),
                        avg_power_w: field_f64(rec, "avg_power"),
                        avg_hr: field_f64(rec, "avg_heart_rate"),
                        max_hr: field_f64(rec, "max_heart_rate"),
                        lap_trigger: field_u8(rec, "lap_trigger").and_then(lap_trigger_name).map(str::to_string),
                        intensity: field_u8(rec, "intensity").and_then(intensity_name).map(str::to_string),
                    });
                }
                MesgNum::Workout => {
                    workout_name = workout_name.or_else(|| {
                        field_string(rec, "wkt_name").or_else(|| field_string(rec, "name"))
                    });
                }
                MesgNum::Session => {
                    session_start_epoch =
                        session_start_epoch.or(field_timestamp_epoch(rec, "start_time"));
                    session_duration_s =
                        session_duration_s
                            .or(field_f64(rec, "total_elapsed_time").map(|v| v.round() as i64));
                    session_distance_m = session_distance_m.or(field_f64(rec, "total_distance"));
                    session_ascent_m = session_ascent_m.or(field_f64(rec, "total_ascent"));
                    session_avg_power = session_avg_power.or(field_f64(rec, "avg_power"));
                    session_np =
                        session_np.or(field_f64_any(rec, &["normalized_power", "norm_power"]));
                    session_avg_hr = session_avg_hr.or(field_f64(rec, "avg_heart_rate"));
                    session_max_hr = session_max_hr.or(field_f64(rec, "max_heart_rate"));
                }
                _ => {}
            }
        }

        anyhow::ensure!(
            !time.is_empty(),
            "no Record messages with a timestamp found in FIT file"
        );

        let started_epoch = session_start_epoch.or(first_ts).unwrap();
        let started_at = chrono::DateTime::from_timestamp(started_epoch, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid FIT start timestamp"))?
            .to_rfc3339();

        // Session summary fields are more accurate than anything we'd
        // derive from the record stream ourselves (esp. normalized power,
        // which needs a proper 30s rolling average) — prefer them, falling
        // back to the stream-derived values only if the session message
        // was missing or incomplete.
        let duration_s = session_duration_s.unwrap_or(*time.last().unwrap() as i64);
        let distance_m = session_distance_m
            .or_else(|| have_distance.then(|| distance.last().copied().unwrap_or(0.0)))
            .unwrap_or(0.0);
        let elevation_gain_m = session_ascent_m.unwrap_or_else(|| {
            if !have_altitude {
                return 0.0;
            }
            altitude.windows(2).map(|w| (w[1] - w[0]).max(0.0)).sum()
        });

        Ok(ParsedActivity {
            started_at,
            duration_s,
            distance_m,
            elevation_gain_m,
            avg_power_w: session_avg_power,
            normalized_power_w: session_np,
            avg_hr: session_avg_hr,
            max_hr: session_max_hr,
            streams_json: super::build_streams_json(
                time,
                latlng,
                if have_distance { distance } else { Vec::new() },
                if have_altitude { altitude } else { Vec::new() },
                if have_watts { watts } else { Vec::new() },
                if have_hr { heartrate } else { Vec::new() },
                if have_cadence { cadence } else { Vec::new() },
            ),
            laps,
            workout_name,
        })
    }
}

mod gpx_fmt {
    use super::{ParsedActivity, haversine_m};

    fn point_epoch(wp: &gpx::Waypoint) -> Option<i64> {
        let s = wp.time?.format().ok()?;
        chrono::DateTime::parse_from_rfc3339(&s)
            .ok()
            .map(|dt| dt.timestamp())
    }

    /// GPX only ever carries position, elevation and time — no power, heart
    /// rate, cadence or laps exist in the format, so those always come back
    /// `None`/empty from a GPX import (see the module doc comment).
    pub fn parse(bytes: &[u8]) -> anyhow::Result<ParsedActivity> {
        let gpx = gpx::read(bytes)?;
        let points: Vec<&gpx::Waypoint> = gpx
            .tracks
            .iter()
            .flat_map(|t| t.segments.iter())
            .flat_map(|s| s.points.iter())
            .collect();
        anyhow::ensure!(points.len() >= 2, "GPX file has fewer than 2 track points");

        let first_epoch = points.iter().find_map(|p| point_epoch(p));

        let mut time = Vec::new();
        let mut latlng = Vec::new();
        let mut altitude = Vec::new();
        let mut have_altitude = false;
        let mut distance_m = 0.0;
        let mut elevation_gain_m = 0.0;
        let mut prev: Option<(f64, f64, Option<f64>)> = None;

        for wp in &points {
            let lat = wp.point().y();
            let lng = wp.point().x();
            latlng.push([lat, lng]);

            let alt = wp.elevation;
            have_altitude |= alt.is_some();
            altitude.push(alt.unwrap_or(0.0));

            if let Some((plat, plng, palt)) = prev {
                distance_m += haversine_m(plat, plng, lat, lng);
                if let (Some(a0), Some(a1)) = (palt, alt) {
                    elevation_gain_m += (a1 - a0).max(0.0);
                }
            }
            prev = Some((lat, lng, alt));

            let t = point_epoch(wp).or(first_epoch).unwrap_or(0) - first_epoch.unwrap_or(0);
            time.push(t as f64);
        }

        let started_at = match first_epoch {
            Some(epoch) => chrono::DateTime::from_timestamp(epoch, 0)
                .ok_or_else(|| anyhow::anyhow!("invalid GPX timestamp"))?
                .to_rfc3339(),
            // No <time> elements at all — GPX allows this (a plain route
            // trace). Fall back to "now" so the activity still gets a
            // sensible position in the dashboard's history.
            None => chrono::Utc::now().to_rfc3339(),
        };
        let duration_s = time.last().copied().unwrap_or(0.0) as i64;

        Ok(ParsedActivity {
            started_at,
            duration_s,
            distance_m,
            elevation_gain_m,
            avg_power_w: None,
            normalized_power_w: None,
            avg_hr: None,
            max_hr: None,
            streams_json: super::build_streams_json(
                time,
                latlng,
                Vec::new(),
                if have_altitude { altitude } else { Vec::new() },
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            laps: Vec::new(),
            workout_name: None,
        })
    }
}

pub fn parse_fit(bytes: &[u8]) -> anyhow::Result<ParsedActivity> {
    fit::parse(bytes)
}

pub fn parse_gpx(bytes: &[u8]) -> anyhow::Result<ParsedActivity> {
    gpx_fmt::parse(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real device recordings (bundled with the `fitparser` crate's own test
    /// suite — see tests/fixtures) rather than a hand-built fixture, since
    /// the whole point is to validate the FIT field names/types against an
    /// actual device output, not against our own assumptions about them.
    #[test]
    fn parses_real_garmin_fenix_5_bike_fixture() {
        let bytes = std::fs::read("tests/fixtures/garmin-fenix-5-bike.fit").unwrap();
        let p = parse_fit(&bytes).unwrap();

        assert_eq!(p.started_at, "2017-06-12T16:09:22+00:00");
        assert_eq!(p.duration_s, 60);
        assert!((p.distance_m - 459.52).abs() < 0.01);
        assert_eq!(p.avg_hr, Some(101.0));
        assert_eq!(p.max_hr, Some(114.0));
        // No power meter paired on this recording — must stay None, not 0.0.
        assert_eq!(p.avg_power_w, None);

        assert_eq!(p.laps.len(), 1);
        assert_eq!(p.laps[0].elapsed_time_s, 60);
        assert_eq!(p.laps[0].avg_hr, Some(101.0));

        let streams = p.streams_json.as_object().unwrap();
        assert!(streams.contains_key("latlng"));
        assert!(streams.contains_key("altitude"));
        assert!(streams.contains_key("heartrate"));
        // No power/cadence sensor in this recording — the keys must be
        // omitted entirely (matches Strava's own streams shape), not
        // present with zero-filled data.
        assert!(!streams.contains_key("watts"));
        assert!(!streams.contains_key("cadence"));
        for key in ["latlng", "altitude", "heartrate", "distance", "time"] {
            assert_eq!(
                streams[key]["data"].as_array().unwrap().len(),
                19,
                "stream {key}"
            );
        }
    }

    #[test]
    fn parses_real_activity_fit_fixture_without_sensors() {
        let bytes = std::fs::read("tests/fixtures/Activity.fit").unwrap();
        let p = parse_fit(&bytes).unwrap();

        assert_eq!(p.started_at, "2012-04-09T21:22:26+00:00");
        assert_eq!(p.duration_s, 14);
        assert!((p.distance_m - 5.73).abs() < 0.01);
        assert_eq!(p.avg_power_w, None);
        assert_eq!(p.avg_hr, None);

        let streams = p.streams_json.as_object().unwrap();
        assert!(!streams.contains_key("heartrate"));
        assert!(!streams.contains_key("watts"));
        assert_eq!(streams["time"]["data"].as_array().unwrap().len(), 14);
    }

    #[test]
    fn rejects_non_fit_bytes() {
        assert!(parse_fit(b"not a fit file").is_err());
    }

    const SAMPLE_GPX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx version="1.1" creator="test" xmlns="http://www.topografix.com/GPX/1/1">
  <trk><name>Test ride</name><trkseg>
    <trkpt lat="49.1193" lon="6.1757"><ele>200.0</ele><time>2026-06-01T10:00:00Z</time></trkpt>
    <trkpt lat="49.1200" lon="6.1770"><ele>210.0</ele><time>2026-06-01T10:00:30Z</time></trkpt>
    <trkpt lat="49.1210" lon="6.1790"><ele>205.0</ele><time>2026-06-01T10:01:00Z</time></trkpt>
  </trkseg></trk>
</gpx>"#;

    #[test]
    fn parses_gpx_track_computing_distance_elevation_and_duration() {
        let p = parse_gpx(SAMPLE_GPX.as_bytes()).unwrap();

        assert_eq!(p.started_at, "2026-06-01T10:00:00+00:00");
        assert_eq!(p.duration_s, 60);
        // No power/HR/cadence exist in GPX at all.
        assert_eq!(p.avg_power_w, None);
        assert_eq!(p.avg_hr, None);
        assert!(p.laps.is_empty());
        // 200 -> 210 (+10) -> 205 (down, ignored): only ascents count.
        assert!((p.elevation_gain_m - 10.0).abs() < 1e-9);
        assert!(p.distance_m > 0.0);

        let streams = p.streams_json.as_object().unwrap();
        assert!(streams.contains_key("latlng"));
        assert!(streams.contains_key("altitude"));
        assert!(!streams.contains_key("watts"));
        assert!(!streams.contains_key("heartrate"));
        assert_eq!(
            streams["time"]["data"],
            serde_json::json!([0.0, 30.0, 60.0])
        );
    }

    #[test]
    fn rejects_gpx_with_too_few_points() {
        let gpx = r#"<?xml version="1.0"?><gpx version="1.1" xmlns="http://www.topografix.com/GPX/1/1">
        <trk><trkseg><trkpt lat="0" lon="0"></trkpt></trkseg></trk></gpx>"#;
        assert!(parse_gpx(gpx.as_bytes()).is_err());
    }
}
