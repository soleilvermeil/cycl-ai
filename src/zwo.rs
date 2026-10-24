//! Export of a `Workout` to Zwift's `.zwo` XML format. Zwift expresses every
//! target as a fraction of FTP, so power/HR/zone targets all go through
//! `Target::power_frac`.

use crate::workout::{Athlete, SimpleStep, Workout, WorkoutStep};

/// Fraction of FTP used for a step without a target (same default as the
/// load estimate).
const DEFAULT_FRAC: f64 = 0.6;

pub fn to_zwo(w: &Workout, a: &Athlete) -> String {
    let mut body = String::new();
    write_steps(&w.steps, a, &mut body);
    format!(
        "<workout_file>\n  <author>CyclAI</author>\n  <name>{}</name>\n  <description></description>\n  <sportType>bike</sportType>\n  <tags/>\n  <workout>\n{body}  </workout>\n</workout_file>\n",
        xml_escape(&w.name)
    )
}

fn write_steps(steps: &[WorkoutStep], a: &Athlete, out: &mut String) {
    for step in steps {
        match step {
            WorkoutStep::Warmup(s) => ramp("Warmup", s, a, false, out),
            WorkoutStep::Cooldown(s) => ramp("Cooldown", s, a, true, out),
            WorkoutStep::Interval(s) | WorkoutStep::Recovery(s) => {
                let (lo, hi) = fracs(s, a);
                steady(s, (lo + hi) / 2.0, out);
            }
            WorkoutStep::Repeat { count, steps } => match steps.as_slice() {
                // Zwift's native on/off block; anything else is unrolled
                // since .zwo has no nested repeats.
                [WorkoutStep::Interval(on), WorkoutStep::Recovery(off)] if on.note.is_none() && off.note.is_none() => {
                    let (on_lo, on_hi) = fracs(on, a);
                    let (off_lo, off_hi) = fracs(off, a);
                    out.push_str(&format!(
                        "    <IntervalsT Repeat=\"{count}\" OnDuration=\"{}\" OffDuration=\"{}\" OnPower=\"{:.3}\" OffPower=\"{:.3}\"/>\n",
                        secs(on),
                        secs(off),
                        (on_lo + on_hi) / 2.0,
                        (off_lo + off_hi) / 2.0
                    ));
                }
                _ => (0..*count).for_each(|_| write_steps(steps, a, out)),
            },
        }
    }
}

fn fracs(s: &SimpleStep, a: &Athlete) -> (f64, f64) {
    use crate::workout::Target;
    match &s.target {
        Some(Target::PowerRange { low, high }) if a.ftp_watts > 0.0 => (low / a.ftp_watts, high / a.ftp_watts),
        Some(Target::PowerZone { zone }) => crate::workout::POWER_ZONE_RANGES_FRAC[(*zone).clamp(1, 7) as usize - 1],
        Some(t) => {
            let f = t.power_frac(a);
            (f, f)
        }
        None => (DEFAULT_FRAC, DEFAULT_FRAC),
    }
}

fn secs(s: &SimpleStep) -> u32 {
    (s.duration.minutes() * 60.0).round().max(1.0) as u32
}

fn steady(s: &SimpleStep, frac: f64, out: &mut String) {
    let head = format!("    <SteadyState Duration=\"{}\" Power=\"{frac:.3}\"", secs(s));
    push_with_note(head, s, out);
}

/// Warmup ramps up from the low to the high end of the target; a cooldown
/// ramps down, so its start is the high end.
fn ramp(tag: &str, s: &SimpleStep, a: &Athlete, down: bool, out: &mut String) {
    let (lo, hi) = fracs(s, a);
    let (start, end) = if down { (hi, lo) } else { (lo, hi) };
    let head = format!("    <{tag} Duration=\"{}\" PowerLow=\"{start:.3}\" PowerHigh=\"{end:.3}\"", secs(s));
    push_with_note_tag(head, tag, s, out);
}

fn push_with_note(head: String, s: &SimpleStep, out: &mut String) {
    push_with_note_tag(head, "SteadyState", s, out)
}

fn push_with_note_tag(head: String, tag: &str, s: &SimpleStep, out: &mut String) {
    out.push_str(&head);
    match s.note.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(n) => out.push_str(&format!(
            ">\n      <textevent timeoffset=\"0\" message=\"{}\"/>\n    </{tag}>\n",
            xml_escape(n)
        )),
        None => out.push_str("/>\n"),
    }
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn athlete() -> Athlete {
        Athlete { ftp_watts: 200.0, hr_rest: 50.0, hr_max: 190.0, run_threshold_pace_s: 0.0 }
    }

    #[test]
    fn exports_ramps_intervals_and_unrolls_complex_repeats() {
        let w: Workout = serde_json::from_value(json!({"name": "A & B", "steps": [
            {"type": "warmup", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 100, "high": 150}},
            {"type": "repeat", "count": 3, "steps": [
                {"type": "interval", "duration": {"type": "time", "value": "5min"}, "target": {"type": "power_range", "low": 200, "high": 200}},
                {"type": "recovery", "duration": {"type": "time", "value": "2min"}, "target": {"type": "power_range", "low": 100, "high": 100}}]},
            {"type": "repeat", "count": 2, "steps": [
                {"type": "interval", "duration": {"type": "time", "value": "1min"}, "target": {"type": "power_zone", "zone": 4}},
                {"type": "interval", "duration": {"type": "time", "value": "1min"}, "target": {"type": "power_zone", "zone": 5}},
                {"type": "recovery", "duration": {"type": "time", "value": "1min"}}]},
            {"type": "cooldown", "duration": {"type": "time", "value": "5min"}, "target": {"type": "power_range", "low": 100, "high": 150}}
        ]})).unwrap();
        let x = to_zwo(&w, &athlete());
        assert!(x.contains("<name>A &amp; B</name>"));
        assert!(x.contains("<Warmup Duration=\"600\" PowerLow=\"0.500\" PowerHigh=\"0.750\"/>"));
        assert!(x.contains("<IntervalsT Repeat=\"3\" OnDuration=\"300\" OffDuration=\"120\" OnPower=\"1.000\" OffPower=\"0.500\"/>"));
        assert_eq!(x.matches("<SteadyState").count(), 6);
        assert!(x.contains("<Cooldown Duration=\"300\" PowerLow=\"0.750\" PowerHigh=\"0.500\"/>"));
    }
}
