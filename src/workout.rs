use serde::{Deserialize, Deserializer, Serialize};

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
    /// Watts.
    PowerRange { low: f64, high: f64 },
    /// Heart-rate range, bpm.
    HrRange { low: f64, high: f64 },
    /// One of the 5 Karvonen heart-rate zones (1 = recovery ... 5 = VO2max).
    HrZone { zone: u8 },
    /// One of the 7 power zones (% of FTP, see `POWER_ZONE_RANGES_FRAC`).
    PowerZone { zone: u8 },
    /// Running pace range in seconds per km; `low` is the *faster* end
    /// (smaller number). Read from a number or an "m:ss" string.
    PaceRange {
        #[serde(deserialize_with = "de_pace")]
        low: f64,
        #[serde(deserialize_with = "de_pace")]
        high: f64,
    },
}

/// "5:30" -> 330.0 seconds per km (a plain number is taken as seconds).
pub fn parse_pace(s: &str) -> Option<f64> {
    let s = s.trim();
    match s.split_once(':') {
        Some((m, sec)) => {
            let (m, sec) = (m.trim().parse::<f64>().ok()?, sec.trim().parse::<f64>().ok()?);
            (m >= 0.0 && (0.0..60.0).contains(&sec)).then_some(m * 60.0 + sec)
        }
        None => s.replace(',', ".").parse::<f64>().ok(),
    }
}

fn de_pace<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Pace {
        Seconds(f64),
        Text(String),
    }
    match Pace::deserialize(d)? {
        Pace::Seconds(n) => Ok(n),
        Pace::Text(s) => parse_pace(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("allure invalide : « {s} » (attendu m:ss, ex: 5:30)"))),
    }
}

/// Each power zone as a (low, high) fraction of FTP — the 7-zone bounds of
/// `metrics::power_zone`, with the open ends (below Z1 / above Z7) capped at
/// 40 % / 180 % so the zones can be drawn and averaged.
pub const POWER_ZONE_RANGES_FRAC: [(f64, f64); 7] =
    [(0.40, 0.55), (0.55, 0.75), (0.75, 0.90), (0.90, 1.05), (1.05, 1.20), (1.20, 1.50), (1.50, 1.80)];

/// What the intensity estimates (distance, TSS) need to know about the
/// athlete: FTP for power targets, resting/max HR to place heart-rate targets
/// in a zone.
#[derive(Debug, Clone, Copy)]
pub struct Athlete {
    pub ftp_watts: f64,
    pub hr_rest: f64,
    pub hr_max: f64,
    /// Running threshold pace, seconds per km; 0 = not set.
    pub run_threshold_pace_s: f64,
}

impl Athlete {
    pub async fn load(pool: &sqlx::SqlitePool) -> Athlete {
        let (ftp, hr_max, hr_rest, pace): (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT ftp_watts, hr_max, hr_rest, run_threshold_pace_s FROM athlete_config WHERE id = 1",
        )
        .fetch_one(pool)
        .await
        .unwrap_or((205, 192, 53, 0));
        Athlete {
            ftp_watts: ftp as f64,
            hr_rest: hr_rest as f64,
            hr_max: hr_max as f64,
            run_threshold_pace_s: pace as f64,
        }
    }
}

/// Upper bounds (fraction of heart-rate reserve) of HR zones 1-4; zone 5 is
/// open-ended. Same bands as the activity page's HR zones (web/detail.rs).
pub const HR_ZONE_UPPER: [f64; 4] = [0.60, 0.70, 0.80, 0.90];

/// Typical power (as a fraction of FTP) sustained in each HR zone — how an
/// HR-targeted step is placed on the power-scaled chart and fed into the
/// distance/TSS estimates. A rough equivalence, not a physiological law.
pub const HR_ZONE_POWER_FRAC: [f64; 5] = [0.50, 0.65, 0.82, 0.97, 1.10];

pub fn hr_zone_of(bpm: f64, a: &Athlete) -> u8 {
    let hrr = (a.hr_max - a.hr_rest).max(1.0);
    let frac = (bpm - a.hr_rest) / hrr;
    HR_ZONE_UPPER.iter().position(|u| frac < *u).map(|i| i as u8 + 1).unwrap_or(5)
}

impl Workout {
    /// Total duration (minutes) and a rough TSS estimate from each step's
    /// average target power (no variability term) — used to show planned
    /// sessions on the calendar. Steps without a target count at 60 % FTP.
    pub fn load_estimate(&self, a: &Athlete) -> (f64, f64) {
        steps_load(&self.steps, a)
    }

    /// Minimal sanity checks — not a full training-science validator.
    /// Duration strings that fail to parse come back as 0 minutes, so 0 is
    /// treated as "didn't parse". Returns a user-facing (French) message.
    pub fn validate(&self) -> Option<String> {
        if self.steps.is_empty() {
            return Some("La séance doit contenir au moins une étape.".to_string());
        }
        check_steps(&self.steps)
    }

    /// Ported from `digest::estimate_distance_km` (kept as a separate copy,
    /// not shared code — digest.rs walks untyped `Value` and is private;
    /// duplicating ~60 lines here is cheaper than restructuring the
    /// existing, tested digest flow to expose it).
    pub fn estimate_distance_km(&self, a: &Athlete) -> f64 {
        steps_distance_km(&self.steps, a)
    }

    /// Distance of a running session from its steps' paces. A step without a
    /// pace target is placed at the pace matching its intensity relative to
    /// the threshold pace; with no threshold pace set that's impossible, so
    /// the whole estimate is `None` rather than a partial number.
    pub fn estimate_run_distance_km(&self, a: &Athlete) -> Option<f64> {
        steps_run_distance_km(&self.steps, a)
    }
}

fn steps_run_distance_km(steps: &[WorkoutStep], a: &Athlete) -> Option<f64> {
    let mut total = 0.0;
    for step in steps {
        match step {
            WorkoutStep::Repeat { count, steps } => total += *count as f64 * steps_run_distance_km(steps, a)?,
            WorkoutStep::Warmup(s) | WorkoutStep::Interval(s) | WorkoutStep::Recovery(s) | WorkoutStep::Cooldown(s) => {
                let pace_s_per_km = match &s.target {
                    Some(Target::PaceRange { low, high }) => (low + high) / 2.0,
                    Some(t) if a.run_threshold_pace_s > 0.0 => a.run_threshold_pace_s / t.power_frac(a).max(0.4),
                    _ => return None,
                };
                if pace_s_per_km <= 0.0 {
                    return None;
                }
                total += s.duration.minutes() * 60.0 / pace_s_per_km;
            }
        }
    }
    Some(total)
}

fn steps_load(steps: &[WorkoutStep], a: &Athlete) -> (f64, f64) {
    let mut minutes = 0.0;
    let mut tss = 0.0;
    for step in steps {
        match step {
            WorkoutStep::Repeat { count, steps } => {
                let (m, t) = steps_load(steps, a);
                minutes += *count as f64 * m;
                tss += *count as f64 * t;
            }
            WorkoutStep::Warmup(s) | WorkoutStep::Interval(s) | WorkoutStep::Recovery(s) | WorkoutStep::Cooldown(s) => {
                let m = s.duration.minutes();
                let frac = s.target.as_ref().map(|t| t.power_frac(a)).unwrap_or(0.6);
                minutes += m;
                tss += m / 60.0 * frac * frac * 100.0;
            }
        }
    }
    (minutes, tss)
}

fn check_steps(steps: &[WorkoutStep]) -> Option<String> {
    for step in steps {
        match step {
            WorkoutStep::Repeat { count, steps } => {
                if *count < 1 {
                    return Some("Le nombre de répétitions doit être au moins 1.".to_string());
                }
                if steps.is_empty() {
                    return Some("Un bloc répété doit contenir au moins une étape.".to_string());
                }
                if let Some(err) = check_steps(steps) {
                    return Some(err);
                }
            }
            WorkoutStep::Warmup(s) | WorkoutStep::Interval(s) | WorkoutStep::Recovery(s) | WorkoutStep::Cooldown(s) => {
                if s.duration.minutes() <= 0.0 {
                    let Duration::Time { value } = &s.duration;
                    return Some(format!("Durée invalide : « {value} » (ex: 10min, 90s, 1h30, 1:30)"));
                }
                match &s.target {
                    Some(Target::PowerRange { low, high }) if low > high => {
                        return Some("La cible de puissance basse doit être inférieure ou égale à la haute.".to_string());
                    }
                    Some(Target::HrRange { low, high }) => {
                        if *low <= 0.0 || low > high {
                            return Some("La cible de FC doit être positive, basse ≤ haute.".to_string());
                        }
                    }
                    Some(Target::HrZone { zone }) if !(1..=5).contains(zone) => {
                        return Some("La zone de FC doit être comprise entre 1 et 5.".to_string());
                    }
                    Some(Target::PowerZone { zone }) if !(1..=7).contains(zone) => {
                        return Some("La zone de puissance doit être comprise entre 1 et 7.".to_string());
                    }
                    Some(Target::PaceRange { low, high }) => {
                        if *low <= 0.0 || low > high {
                            return Some("L'allure doit être positive, la plus rapide (basse) ≤ la plus lente (haute).".to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

fn steps_distance_km(steps: &[WorkoutStep], a: &Athlete) -> f64 {
    steps.iter().map(|s| s.distance_km(a)).sum()
}

impl WorkoutStep {
    fn distance_km(&self, a: &Athlete) -> f64 {
        match self {
            WorkoutStep::Repeat { count, steps } => *count as f64 * steps_distance_km(steps, a),
            WorkoutStep::Warmup(s)
            | WorkoutStep::Interval(s)
            | WorkoutStep::Recovery(s)
            | WorkoutStep::Cooldown(s) => {
                let dur_min = s.duration.minutes();
                let frac = s.target.as_ref().map(|t| t.power_frac(a));
                dur_min / 60.0 * speed_kmh(frac, &s.terrain)
            }
        }
    }
}

impl Duration {
    pub fn minutes(&self) -> f64 {
        let Duration::Time { value } = self;
        parse_minutes(value)
    }
}

/// Parses the duration strings the builder and the LLM both produce:
/// "15min", "90s"/"90sec", "1h", "1h30", "1:30" (m:ss), or a bare number of
/// minutes. Anything unparseable is 0 (callers treat 0 as invalid). The
/// builder's chart (`workout_chart::CHART_JS`) implements the same grammar.
pub fn parse_minutes(raw: &str) -> f64 {
    let s = raw.trim().to_lowercase().replace(',', ".");
    let num = |t: &str| t.trim().parse::<f64>().ok();
    if let Some((h, rest)) = s.split_once('h') {
        let rest = rest.trim().trim_end_matches("min").trim();
        let minutes = if rest.is_empty() { Some(0.0) } else { num(rest) };
        return match (num(h), minutes) {
            (Some(h), Some(m)) => h * 60.0 + m,
            _ => 0.0,
        };
    }
    if let Some(n) = s.strip_suffix("min") {
        return num(n).unwrap_or(0.0);
    }
    if let Some(n) = s.strip_suffix("sec").or_else(|| s.strip_suffix('s')) {
        return num(n).map(|v| v / 60.0).unwrap_or(0.0);
    }
    if let Some((m, sec)) = s.split_once(':') {
        return match (num(m), num(sec)) {
            (Some(m), Some(sec)) => m + sec / 60.0,
            _ => 0.0,
        };
    }
    num(&s).unwrap_or(0.0)
}

impl Target {
    /// Intensity as a fraction of FTP: exact for power targets, via the
    /// HR-zone equivalence (`HR_ZONE_POWER_FRAC`) for heart-rate targets, and
    /// threshold pace / step pace for pace targets (0.75 when no threshold
    /// pace is set).
    pub fn power_frac(&self, a: &Athlete) -> f64 {
        match self {
            Target::PowerZone { zone } => {
                let (lo, hi) = POWER_ZONE_RANGES_FRAC[(*zone).clamp(1, 7) as usize - 1];
                (lo + hi) / 2.0
            }
            Target::PaceRange { low, high } => {
                let mid = (low + high) / 2.0;
                if a.run_threshold_pace_s > 0.0 && mid > 0.0 {
                    (a.run_threshold_pace_s / mid).clamp(0.4, 1.3)
                } else {
                    0.75
                }
            }
            Target::PowerRange { low, high } => (low + high) / 2.0 / a.ftp_watts,
            Target::HrZone { zone } => HR_ZONE_POWER_FRAC[(*zone).clamp(1, 5) as usize - 1],
            Target::HrRange { low, high } => {
                HR_ZONE_POWER_FRAC[hr_zone_of((low + high) / 2.0, a) as usize - 1]
            }
        }
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
    fn test_athlete() -> Athlete {
        Athlete { ftp_watts: 205.0, hr_rest: 53.0, hr_max: 192.0, run_threshold_pace_s: 270.0 }
    }

    #[test]
    fn hr_targets_round_trip_and_map_to_zone_intensity() {
        let steps_json = json!([
            {"type": "interval", "duration": {"type": "time", "value": "10min"}, "target": {"type": "hr_zone", "zone": 3}},
            {"type": "interval", "duration": {"type": "time", "value": "10min"}, "target": {"type": "hr_range", "low": 150, "high": 160}}
        ]);
        let steps: Vec<WorkoutStep> = serde_json::from_value(steps_json.clone()).unwrap();
        let re = serde_json::to_value(&steps).unwrap();
        assert_eq!(re[0]["target"], json!({"type": "hr_zone", "zone": 3}));
        assert_eq!(re[1]["target"]["type"], "hr_range");

        let a = test_athlete();
        let WorkoutStep::Interval(s0) = &steps[0] else { panic!() };
        assert_eq!(s0.target.as_ref().unwrap().power_frac(&a), HR_ZONE_POWER_FRAC[2]);
        // 155 bpm with rest 53 / max 192 is 73 % of reserve: zone 3.
        let WorkoutStep::Interval(s1) = &steps[1] else { panic!() };
        assert_eq!(s1.target.as_ref().unwrap().power_frac(&a), HR_ZONE_POWER_FRAC[2]);

        // HR-targeted steps still produce a duration and a load estimate.
        let w = Workout { name: "x".into(), steps, estimated_distance_km: None };
        let (min, tss) = w.load_estimate(&a);
        assert_eq!(min, 20.0);
        assert!(tss > 0.0);
        assert_eq!(w.validate(), None);
    }

    #[test]
    fn power_zone_and_pace_targets_round_trip_and_map_to_intensity() {
        let steps: Vec<WorkoutStep> = serde_json::from_value(json!([
            {"type": "interval", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_zone", "zone": 4}},
            {"type": "interval", "duration": {"type": "time", "value": "10min"}, "target": {"type": "pace_range", "low": "4:30", "high": 300}}
        ]))
        .unwrap();
        // "m:ss" and plain seconds both read as seconds per km, written back as numbers.
        let re = serde_json::to_value(&steps).unwrap();
        assert_eq!(re[1]["target"], json!({"type": "pace_range", "low": 270.0, "high": 300.0}));

        let a = test_athlete();
        let WorkoutStep::Interval(z) = &steps[0] else { panic!() };
        assert!((z.target.as_ref().unwrap().power_frac(&a) - 0.975).abs() < 1e-9);
        // Threshold pace 4:30; a 4:45 average pace is a bit under threshold.
        let WorkoutStep::Interval(p) = &steps[1] else { panic!() };
        let f = p.target.as_ref().unwrap().power_frac(&a);
        assert!((f - 270.0 / 285.0).abs() < 1e-9, "got {f}");
        // Without a threshold pace set, a pace step counts as an easy 0.75.
        let no_thr = Athlete { run_threshold_pace_s: 0.0, ..a };
        assert_eq!(p.target.as_ref().unwrap().power_frac(&no_thr), 0.75);

        assert!(serde_json::from_value::<Target>(json!({"type": "pace_range", "low": "fast", "high": 300})).is_err());
    }

    #[test]
    fn run_distance_comes_from_paces_and_needs_a_reference_for_other_targets() {
        let a = test_athlete(); // threshold pace 4:30
        let w: Workout = serde_json::from_value(json!({"name": "x", "steps": [
            {"type": "interval", "duration": {"type": "time", "value": "30min"}, "target": {"type": "pace_range", "low": 300, "high": 300}},
            {"type": "repeat", "count": 2, "steps": [
                {"type": "interval", "duration": {"type": "time", "value": "5min"}, "target": {"type": "pace_range", "low": 240, "high": 240}}]}
        ]})).unwrap();
        // 30 min at 5:00/km = 6 km, 2 x 5 min at 4:00/km = 2 x 1.25 km.
        assert!((w.estimate_run_distance_km(&a).unwrap() - 8.5).abs() < 1e-9);

        // A heart-rate step is placed via the threshold pace when it's known...
        let hr: Workout = serde_json::from_value(json!({"name": "x", "steps": [
            {"type": "interval", "duration": {"type": "time", "value": "60min"}, "target": {"type": "hr_zone", "zone": 2}}]})).unwrap();
        let km = hr.estimate_run_distance_km(&a).unwrap();
        assert!(km > 8.0 && km < 14.0, "got {km}");
        // ...and unknowable without one.
        assert_eq!(hr.estimate_run_distance_km(&Athlete { run_threshold_pace_s: 0.0, ..a }), None);
    }

    #[test]
    fn validates_new_targets() {
        let bad: Workout = serde_json::from_value(json!({"name": "x", "steps": [
            {"type": "interval", "duration": {"type": "time", "value": "5min"}, "target": {"type": "power_zone", "zone": 8}}]})).unwrap();
        assert!(bad.validate().unwrap().contains("zone de puissance"));
        let inverted: Workout = serde_json::from_value(json!({"name": "x", "steps": [
            {"type": "interval", "duration": {"type": "time", "value": "5min"}, "target": {"type": "pace_range", "low": 330, "high": 300}}]})).unwrap();
        assert!(inverted.validate().unwrap().contains("allure"));
    }

    #[test]
    fn validates_hr_targets() {
        let bad_zone: Workout = serde_json::from_value(json!({"name": "x", "steps": [
            {"type": "interval", "duration": {"type": "time", "value": "5min"}, "target": {"type": "hr_zone", "zone": 7}}]})).unwrap();
        assert!(bad_zone.validate().unwrap().contains("zone de FC"));
        let inverted: Workout = serde_json::from_value(json!({"name": "x", "steps": [
            {"type": "interval", "duration": {"type": "time", "value": "5min"}, "target": {"type": "hr_range", "low": 170, "high": 150}}]})).unwrap();
        assert!(inverted.validate().unwrap().contains("FC"));
    }

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
        let distance = steps_distance_km(&steps, &test_athlete());
        assert!(distance > 0.0);
        assert!(distance < 30.0);
    }

    #[test]
    fn climbing_terrain_uses_flat_climb_speed_regardless_of_power() {
        let steps_json = json!([
            {"type": "interval", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 300, "high": 300}, "terrain": "montée"}
        ]);
        let steps: Vec<WorkoutStep> = serde_json::from_value(steps_json).unwrap();
        let distance = steps_distance_km(&steps, &test_athlete());
        assert!((distance - 2.25).abs() < 1e-9);
    }

    #[test]
    fn duration_parses_minutes_and_seconds() {
        assert_eq!(Duration::Time { value: "15min".into() }.minutes(), 15.0);
        assert_eq!(Duration::Time { value: "90sec".into() }.minutes(), 1.5);
        assert_eq!(Duration::Time { value: "30s".into() }.minutes(), 0.5);
    }

    #[test]
    fn duration_parses_hours_and_clock_formats() {
        assert_eq!(parse_minutes("1h"), 60.0);
        assert_eq!(parse_minutes("1h30"), 90.0);
        assert_eq!(parse_minutes("1h30min"), 90.0);
        assert_eq!(parse_minutes("1:30"), 1.5);
        assert_eq!(parse_minutes("12,5min"), 12.5);
        assert_eq!(parse_minutes("20"), 20.0);
        assert_eq!(parse_minutes("abc"), 0.0);
        assert_eq!(parse_minutes(""), 0.0);
    }
}
