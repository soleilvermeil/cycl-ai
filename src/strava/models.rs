use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
}

/// Subset of Strava's SummaryActivity we actually persist.
#[derive(Debug, Deserialize)]
pub struct SummaryActivity {
    pub id: i64,
    /// The athlete-entered (or device-default) activity title — free with
    /// the summary list, unlike `description` which needs a per-activity
    /// detail call (see `DetailedActivity`).
    pub name: String,
    pub start_date: String, // RFC3339
    /// Strava's granular activity classification — the legacy `type` field
    /// conflates gravel/MTB rides under "Ride" too, so this is the field
    /// to filter on for "road cycling only".
    pub sport_type: String,
    pub moving_time: i64, // seconds
    pub distance: f64,    // meters
    pub total_elevation_gain: f64,
    pub average_watts: Option<f64>,
    /// Only meaningful when `device_watts` is true (real power meter, not estimated).
    pub device_watts: Option<bool>,
    pub weighted_average_watts: Option<f64>,
    pub average_heartrate: Option<f64>,
    pub max_heartrate: Option<f64>,
}

/// Subset of Strava's DetailedActivity — fetched via a separate per-activity
/// call (`GET /activities/{id}`) since `description` isn't in the summary
/// list response at all.
#[derive(Debug, Deserialize)]
pub struct DetailedActivity {
    pub description: Option<String>,
}

/// Subset of Strava's SummarySegment. Used only to seed reference_segments'
/// coordinates — effort computation is done locally, never via Strava's own
/// segment_efforts.
#[derive(Debug, Deserialize)]
pub struct StarredSegment {
    pub name: String,
    pub distance: f64,
    pub elevation_high: f64,
    pub elevation_low: f64,
    pub start_latlng: [f64; 2],
    pub end_latlng: [f64; 2],
}

/// A manual lap press (or auto-lap) on a structured workout. Distinct from
/// our own segment matching — this is whatever the athlete/device marked
/// during the ride, e.g. each interval of a structured session.
#[derive(Debug, Deserialize)]
pub struct Lap {
    pub lap_index: i64,
    pub elapsed_time: i64,
    pub distance: f64,
    pub average_watts: Option<f64>,
    pub device_watts: Option<bool>,
    pub average_heartrate: Option<f64>,
    pub max_heartrate: Option<f64>,
}
