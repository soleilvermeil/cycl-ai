//! Route-shape thumbnail SVG (like Strava's own small previews), computed
//! once from an activity's raw GPS stream and cached in
//! `activities.route_thumb_svg` (see `sync::run`) instead of being re-parsed
//! from the full streams JSON on every dashboard load — a 2h ride at 1Hz is ~7000 GPS
//! points, and the dashboard lists 10 activities per page.

pub const ROUTE_THUMB_W: f64 = 96.0;
pub const ROUTE_THUMB_H: f64 = 64.0;

fn extract_latlng(streams_json: &str) -> Option<Vec<[f64; 2]>> {
    let parsed: serde_json::Value = serde_json::from_str(streams_json).ok()?;
    parsed
        .get("latlng")?
        .get("data")?
        .as_array()?
        .iter()
        .map(|pair| {
            let arr = pair.as_array()?;
            Some([arr.first()?.as_f64()?, arr.get(1)?.as_f64()?])
        })
        .collect()
}

fn render(latlng: &[[f64; 2]]) -> Option<String> {
    if latlng.len() < 2 {
        return None;
    }
    let lat_min = latlng.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let lat_max = latlng
        .iter()
        .map(|p| p[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let lng_min = latlng.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let lng_max = latlng
        .iter()
        .map(|p| p[1])
        .fold(f64::NEG_INFINITY, f64::max);

    // Longitude degrees compress towards the poles; correct so the shape
    // isn't stretched east-west.
    let lat_mid = (lat_min + lat_max) / 2.0;
    let lng_correction = lat_mid.to_radians().cos().max(0.1);

    let lat_range = (lat_max - lat_min).max(1e-9);
    let lng_range = ((lng_max - lng_min) * lng_correction).max(1e-9);

    const PAD: f64 = 4.0;
    let usable_w = ROUTE_THUMB_W - 2.0 * PAD;
    let usable_h = ROUTE_THUMB_H - 2.0 * PAD;
    let scale = (usable_w / lng_range).min(usable_h / lat_range);

    let offset_x = PAD + (usable_w - lng_range * scale) / 2.0;
    let offset_y = PAD + (usable_h - lat_range * scale) / 2.0;

    // Downsample dense streams — a route shape doesn't need per-second
    // resolution to look right.
    let stride = (latlng.len() / 150).max(1);
    let points: String = latlng
        .iter()
        .step_by(stride)
        .map(|p| {
            let x = offset_x + (p[1] - lng_min) * lng_correction * scale;
            let y = offset_y + (lat_max - p[0]) * scale;
            format!("{x:.1},{y:.1}")
        })
        .collect::<Vec<_>>()
        .join(" ");

    Some(format!(
        r##"<svg viewBox="0 0 {ROUTE_THUMB_W} {ROUTE_THUMB_H}" width="{ROUTE_THUMB_W}" height="{ROUTE_THUMB_H}" class="route-thumb" aria-hidden="true"><polyline points="{points}" fill="none" stroke="#2563eb" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>"##
    ))
}

/// Parses raw Strava stream JSON and renders the route thumbnail in one
/// step — the only entry point callers need.
pub fn compute(streams_json: &str) -> Option<String> {
    render(&extract_latlng(streams_json)?)
}

/// Simplified GPS track for the Statistiques map: at most ~150 points,
/// rounded to 5 decimals (~1 m), as a compact JSON `[[lat,lng],...]`.
pub fn compute_points(streams_json: &str) -> Option<String> {
    let latlng = extract_latlng(streams_json)?;
    if latlng.len() < 2 {
        return None;
    }
    let stride = (latlng.len() / 150).max(1);
    let mut pts: Vec<[f64; 2]> = latlng.iter().step_by(stride).copied().collect();
    if let (Some(last), Some(&end)) = (pts.last(), latlng.last()) {
        if *last != end {
            pts.push(end);
        }
    }
    let r = |v: f64| (v * 1e5).round() / 1e5;
    let rounded: Vec<[f64; 2]> = pts.iter().map(|p| [r(p[0]), r(p[1])]).collect();
    serde_json::to_string(&rounded).ok()
}
