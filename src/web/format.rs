use chrono::{DateTime, Datelike, Timelike};
use chrono_tz::Europe::Paris;

const WEEKDAYS_FR: [&str; 7] = ["Lun.", "Mar.", "Mer.", "Jeu.", "Ven.", "Sam.", "Dim."];
const MONTHS_FR: [&str; 12] = [
    "Janvier", "Février", "Mars", "Avril", "Mai", "Juin", "Juillet", "Août", "Septembre",
    "Octobre", "Novembre", "Décembre",
];

/// Strava's `start_date` is RFC3339 UTC. Converted to Europe/Paris (handles
/// CET/CEST DST automatically, unlike a hardcoded +2 offset) and formatted
/// in French: "Dim. 16 Août 17:42".
pub fn format_datetime_fr(rfc3339: &str) -> String {
    let Ok(dt) = DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let paris = dt.with_timezone(&Paris);
    let weekday = WEEKDAYS_FR[paris.weekday().num_days_from_monday() as usize];
    let month = MONTHS_FR[(paris.month() - 1) as usize];
    format!(
        "{weekday} {} {month} {:02}:{:02}",
        paris.day(),
        paris.hour(),
        paris.minute()
    )
}

/// "Chronometer" H:MM display, e.g. 7762s -> "2:09".
pub fn format_duration_hms(duration_s: i64) -> String {
    let h = duration_s / 3600;
    let m = (duration_s % 3600) / 60;
    format!("{h}:{m:02}")
}

/// Running pace as "m:ss" per km (e.g. 5:12), from distance and moving time.
/// `None` when there's no usable distance (treadmill without a sensor, etc.).
pub fn format_pace(distance_m: f64, duration_s: i64) -> Option<String> {
    if distance_m < 10.0 || duration_s <= 0 {
        return None;
    }
    Some(format_pace_secs(duration_s as f64 / (distance_m / 1000.0)))
}

/// Formats seconds-per-km as "m:ss" (rounded to the nearest second).
pub fn format_pace_secs(secs_per_km: f64) -> String {
    let total = secs_per_km.round() as i64;
    format!("{}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pace_is_minutes_seconds_per_km() {
        // 10 km in 52:00 -> 5:12 /km
        assert_eq!(format_pace(10_000.0, 3120).as_deref(), Some("5:12"));
        assert_eq!(format_pace(0.0, 3120), None);
        // Rounding carries into the minute: 299.6s -> 5:00
        assert_eq!(format_pace_secs(299.6), "5:00");
    }

    #[test]
    fn formats_utc_summer_time_to_paris_cest() {
        // 2026-08-16T15:42:51Z is CEST (UTC+2) -> 17:42, a Sunday.
        assert_eq!(format_datetime_fr("2026-08-16T15:42:51Z"), "Dim. 16 Août 17:42");
    }

    #[test]
    fn formats_utc_winter_time_to_paris_cet() {
        // 2026-01-05T10:00:00Z is CET (UTC+1) -> 11:00, a Monday.
        assert_eq!(format_datetime_fr("2026-01-05T10:00:00Z"), "Lun. 5 Janvier 11:00");
    }

    #[test]
    fn duration_chronometer_format() {
        assert_eq!(format_duration_hms(7762), "2:09");
        assert_eq!(format_duration_hms(59), "0:00");
        assert_eq!(format_duration_hms(3600), "1:00");
    }
}
