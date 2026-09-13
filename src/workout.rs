use serde::{Deserialize, Serialize};

/// Shared contract for a structured workout — hand-built via the
/// `/workouts` builder page, or produced as a side-effect of the weekly
/// digest (`digest.rs`). Must serialize/deserialize compatibly with the
/// untyped JSON shape `digest.rs`'s LLM prompt already dictates, so
/// `web::render_workout_steps` keeps rendering both kinds unmodified — see
/// `round_trips_digest_fixture_shape` below, which guards that contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workout {
    pub name: String,
    pub steps: Vec<WorkoutStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_distance_km: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkoutStep {
    Warmup(SimpleStep),
    Interval(SimpleStep),
    Recovery(SimpleStep),
    Cooldown(SimpleStep),
    Repeat { count: u32, steps: Vec<WorkoutStep> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimpleStep {
    pub duration: Duration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    // String, not an enum: LLM output is loose ("plat"/"vallonné"/"montée"
    // today, but a typo shouldn't hard-fail parsing) — validated with a
    // match in the builder's server-side handler instead.
    #[serde(default = "default_terrain")]
    pub terrain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn default_terrain() -> String {
    "plat".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Duration {
    Time { value: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Target {
    PowerRange { low: f64, high: f64 },
}

impl Workout {
    /// Ported from `digest::estimate_distance_km` (kept as a separate copy,
    /// not shared code — digest.rs walks untyped `Value` and is private;
    /// duplicating ~60 lines here is cheaper than restructuring the
    /// existing, tested digest flow to expose it).
    pub fn estimate_distance_km(&self, ftp_watts: f64) -> f64 {
        steps_distance_km(&self.steps, ftp_watts)
    }
}

fn steps_distance_km(steps: &[WorkoutStep], ftp: f64) -> f64 {
    steps.iter().map(|s| s.distance_km(ftp)).sum()
}

impl WorkoutStep {
    fn distance_km(&self, ftp: f64) -> f64 {
        match self {
            WorkoutStep::Repeat { count, steps } => *count as f64 * steps_distance_km(steps, ftp),
            WorkoutStep::Warmup(s)
            | WorkoutStep::Interval(s)
            | WorkoutStep::Recovery(s)
            | WorkoutStep::Cooldown(s) => {
                let dur_min = s.duration.minutes();
                let frac = s.target.as_ref().map(|t| t.power_frac(ftp));
                dur_min / 60.0 * speed_kmh(frac, &s.terrain)
            }
        }
    }
}

impl Duration {
    pub fn minutes(&self) -> f64 {
        let Duration::Time { value } = self;
        let s = value.trim();
        if let Some(n) = s.strip_suffix("min") {
            n.trim().parse().unwrap_or(0.0)
        } else if let Some(n) = s.strip_suffix("sec") {
            n.trim().parse::<f64>().unwrap_or(0.0) / 60.0
        } else if let Some(n) = s.strip_suffix('s') {
            n.trim().parse::<f64>().unwrap_or(0.0) / 60.0
        } else {
            s.parse().unwrap_or(0.0)
        }
    }
}

impl Target {
    pub fn power_frac(&self, ftp: f64) -> f64 {
        let Target::PowerRange { low, high } = self;
        (low + high) / 2.0 / ftp
    }
}

/// Rough speed model calibrated on the athlete's known real-world pace:
/// ~30km/h Z2-Z3 on flat, 12-15km/h climbing. Climbing speed is dominated
/// by gradient, not power zone, so it's flattened to one value.
fn speed_kmh(power_frac_ftp: Option<f64>, terrain: &str) -> f64 {
    if terrain == "montée" {
        return 13.5;
    }
    match power_frac_ftp {
        Some(f) if f < 0.55 => 22.0,
        Some(f) if f < 0.75 => 28.0,
        Some(f) if f < 0.90 => 30.0,
        Some(f) if f < 1.05 => 32.0,
        Some(f) if f < 1.20 => 34.0,
        Some(_) => 36.0,
        None => 25.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// Injects the same "terrain" default the deserializer applies, so a
    /// parse→serialize round trip can be compared structurally against the
    /// original fixture (which omits terrain on most steps).
    fn with_default_terrain(mut v: Value) -> Value {
        match &mut v {
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    *item = with_default_terrain(item.take());
                }
            }
            Value::Object(map) => {
                if let Some(t) = map.get("type").and_then(|t| t.as_str()) {
                    if t == "repeat" {
                        if let Some(steps) = map.get_mut("steps") {
                            *steps = with_default_terrain(steps.take());
                        }
                    } else if !map.contains_key("terrain") {
                        map.insert("terrain".to_string(), json!("plat"));
                    }
                }
            }
            _ => {}
        }
        v
    }

    /// `Target`'s fields are `f64`, so a round trip turns JSON integer
    /// literals (154) into floats (154.0) — numerically identical but a
    /// different `serde_json::Number` variant, which plain `Value` equality
    /// treats as unequal. Normalize both sides through the same lossy path.
    fn normalize_numbers(v: Value) -> Value {
        match v {
            Value::Number(n) => n.as_f64().map(|f| json!(f)).unwrap_or(Value::Number(n)),
            Value::Array(arr) => Value::Array(arr.into_iter().map(normalize_numbers).collect()),
            Value::Object(map) => {
                Value::Object(map.into_iter().map(|(k, v)| (k, normalize_numbers(v))).collect())
            }
            other => other,
        }
    }

    /// Same fixture as `digest::tests::distance_estimate_matches_readme_example`
    /// — the regression guard that keeps `Workout`'s JSON shape compatible
    /// with what digest.rs's LLM prompt produces and what
    /// `web::render_workout_steps` (which stays on untyped `Value`) expects.
    #[test]
    fn round_trips_digest_fixture_shape() {
        let steps_json = json!([
            {"type": "warmup", "duration": {"type": "time", "value": "15min"}, "target": {"type": "power_range", "low": 123, "high": 154}},
            {"type": "repeat", "count": 2, "steps": [
                {"type": "repeat", "count": 3, "steps": [
                    {"type": "interval", "duration": {"type": "time", "value": "3min"}, "target": {"type": "power_range", "low": 195, "high": 195}},
                    {"type": "interval", "duration": {"type": "time", "value": "1min"}, "target": {"type": "power_range", "low": 215, "high": 220}}
                ]},
                {"type": "recovery", "duration": {"type": "time", "value": "3min"}, "target": {"type": "power_range", "low": 100, "high": 123}}
            ]},
            {"type": "cooldown", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 100, "high": 123}}
        ]);

        let parsed: Vec<WorkoutStep> =
            serde_json::from_value(steps_json.clone()).expect("must parse digest.rs's own JSON shape");
        let re_serialized = normalize_numbers(serde_json::to_value(&parsed).unwrap());
        let expected = normalize_numbers(with_default_terrain(steps_json));

        assert_eq!(re_serialized, expected);
    }

    #[test]
    fn distance_estimate_matches_digest_readme_example() {
        // Same fixture/expectations as digest.rs's test: 15+2*(3*(3+1)+3)+10 = 55min total.
        let steps_json = json!([
            {"type": "warmup", "duration": {"type": "time", "value": "15min"}, "target": {"type": "power_range", "low": 123, "high": 154}},
            {"type": "repeat", "count": 2, "steps": [
                {"type": "repeat", "count": 3, "steps": [
                    {"type": "interval", "duration": {"type": "time", "value": "3min"}, "target": {"type": "power_range", "low": 195, "high": 195}},
                    {"type": "interval", "duration": {"type": "time", "value": "1min"}, "target": {"type": "power_range", "low": 215, "high": 220}}
                ]},
                {"type": "recovery", "duration": {"type": "time", "value": "3min"}, "target": {"type": "power_range", "low": 100, "high": 123}}
            ]},
            {"type": "cooldown", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 100, "high": 123}}
        ]);
        let steps: Vec<WorkoutStep> = serde_json::from_value(steps_json).unwrap();
        let distance = steps_distance_km(&steps, 205.0);
        assert!(distance > 0.0);
        assert!(distance < 30.0);
    }

    #[test]
    fn climbing_terrain_uses_flat_climb_speed_regardless_of_power() {
        let steps_json = json!([
            {"type": "interval", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 300, "high": 300}, "terrain": "montée"}
        ]);
        let steps: Vec<WorkoutStep> = serde_json::from_value(steps_json).unwrap();
        let distance = steps_distance_km(&steps, 205.0);
        assert!((distance - 2.25).abs() < 1e-9);
    }

    #[test]
    fn duration_parses_minutes_and_seconds() {
        assert_eq!(Duration::Time { value: "15min".into() }.minutes(), 15.0);
        assert_eq!(Duration::Time { value: "90sec".into() }.minutes(), 1.5);
        assert_eq!(Duration::Time { value: "30s".into() }.minutes(), 0.5);
    }
}
