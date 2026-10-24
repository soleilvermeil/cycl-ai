use super::format::{format_datetime_fr, format_duration_hms};
use super::{annotate_acronyms, escape_html, llm_notice, markdown_download, render_zone_bar, AppState, WarnQuery};
use axum::extract::{Form, Path, Query, State};
use axum::response::{Html, Redirect};
use serde::Deserialize;
use serde_json::Value;
use sqlx::SqlitePool;

#[derive(sqlx::FromRow)]
struct ActivityDetailRow {
    sport_type: String,
    started_at: String,
    distance_m: f64,
    duration_s: i64,
    elevation_gain_m: f64,
    avg_power_w: Option<f64>,
    normalized_power_w: Option<f64>,
    avg_hr: Option<f64>,
    max_hr: Option<f64>,
    tss: Option<f64>,
    intensity_factor: Option<f64>,
    zone1_s: i64,
    zone2_s: i64,
    zone3_s: i64,
    zone4_s: i64,
    zone5_s: i64,
    zone6_s: i64,
    zone7_s: i64,
    bilan: Option<String>,
    streams_json: Option<String>,
    user_context: Option<String>,
    description: Option<String>,
}

struct ActivityDetail {
    is_run: bool,
    started_at: String,
    distance_m: f64,
    duration_s: i64,
    elevation_gain_m: f64,
    avg_power_w: Option<f64>,
    normalized_power_w: Option<f64>,
    avg_hr: Option<f64>,
    max_hr: Option<f64>,
    tss: Option<f64>,
    intensity_factor: Option<f64>,
    zone_secs: [i64; 7],
    bilan: Option<String>,
    streams_json: Option<String>,
    user_context: Option<String>,
    description: Option<String>,
}

struct SegmentEffortRow {
    reference_segment_id: i64,
    name: String,
    avg_power_w: f64,
    delta_vs_pr_pct: Option<f64>,
    delta_vs_last_pct: Option<f64>,
    start_distance_m: Option<f64>,
    end_distance_m: Option<f64>,
    pr_activity_id: Option<i64>,
    last_activity_id: Option<i64>,
}

/// A PR/last effort's power+HR curve on a given segment, re-baselined so
/// distance 0 is that effort's own segment entry — lets it be overlaid on
/// the current activity's (distance-based) chart by shifting onto the
/// current effort's own [start_km, end_km] window regardless of how long
/// or far that other effort actually took to cover the same segment.
struct SegmentCurve {
    distance_km: Vec<f64>,
    power: Option<Vec<f64>>,
    hr: Option<Vec<f64>>,
}

struct LapRow {
    lap_index: i64,
    elapsed_time_s: i64,
    distance_m: f64,
    avg_power_w: Option<f64>,
    avg_hr: Option<f64>,
}

async fn fetch_activity(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<ActivityDetail>> {
    let row: Option<ActivityDetailRow> = sqlx::query_as(
        r#"
        SELECT a.sport_type, a.started_at, a.distance_m, a.duration_s, a.elevation_gain_m,
               a.avg_power_w, a.normalized_power_w, a.avg_hr, a.max_hr, a.tss, a.intensity_factor,
               a.zone1_s, a.zone2_s, a.zone3_s, a.zone4_s, a.zone5_s, a.zone6_s, a.zone7_s,
               b.text AS bilan, s.streams_json, a.user_context, a.description
        FROM activities a
        LEFT JOIN activity_bilans b ON b.activity_id = a.id
        LEFT JOIN activity_streams s ON s.activity_id = a.id
        WHERE a.id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| ActivityDetail {
        is_run: crate::metrics::is_run(&r.sport_type),
        started_at: r.started_at,
        distance_m: r.distance_m,
        duration_s: r.duration_s,
        elevation_gain_m: r.elevation_gain_m,
        avg_power_w: r.avg_power_w,
        normalized_power_w: r.normalized_power_w,
        avg_hr: r.avg_hr,
        max_hr: r.max_hr,
        tss: r.tss,
        intensity_factor: r.intensity_factor,
        zone_secs: [r.zone1_s, r.zone2_s, r.zone3_s, r.zone4_s, r.zone5_s, r.zone6_s, r.zone7_s],
        bilan: r.bilan,
        streams_json: r.streams_json,
        user_context: r.user_context,
        description: r.description,
    }))
}

#[allow(clippy::type_complexity)]
async fn fetch_segment_efforts(pool: &SqlitePool, activity_id: i64) -> anyhow::Result<Vec<SegmentEffortRow>> {
    let rows: Vec<(i64, String, f64, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<i64>, Option<i64>)> = sqlx::query_as(
        r#"
        SELECT rs.id, rs.name, se.avg_power_w, se.delta_vs_pr_pct, se.delta_vs_last_pct,
               se.start_distance_m, se.end_distance_m, se.pr_activity_id, se.last_activity_id
        FROM segment_efforts se
        JOIN reference_segments rs ON rs.id = se.reference_segment_id
        WHERE se.activity_id = ?
        "#,
    )
    .bind(activity_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(reference_segment_id, name, avg_power_w, delta_vs_pr_pct, delta_vs_last_pct, start_distance_m, end_distance_m, pr_activity_id, last_activity_id)| SegmentEffortRow {
            reference_segment_id,
            name,
            avg_power_w,
            delta_vs_pr_pct,
            delta_vs_last_pct,
            start_distance_m,
            end_distance_m,
            pr_activity_id,
            last_activity_id,
        })
        .collect())
}

/// Fetches another activity's own effort on `reference_segment_id` (its own
/// [start_distance_m, end_distance_m] window, from a previous local match —
/// not re-matched here) and slices its power/HR stream down to that window,
/// re-baselined to km-since-segment-start so it can be distance-shifted
/// onto the current activity's own segment window for an overlay.
async fn fetch_segment_curve(
    pool: &SqlitePool,
    other_activity_id: i64,
    reference_segment_id: i64,
) -> anyhow::Result<Option<SegmentCurve>> {
    let row: Option<(Option<f64>, Option<f64>, String)> = sqlx::query_as(
        r#"
        SELECT se.start_distance_m, se.end_distance_m, s.streams_json
        FROM segment_efforts se
        JOIN activity_streams s ON s.activity_id = se.activity_id
        WHERE se.activity_id = ? AND se.reference_segment_id = ?
        "#,
    )
    .bind(other_activity_id)
    .bind(reference_segment_id)
    .fetch_optional(pool)
    .await?;
    let Some((Some(start_m), Some(end_m), streams_json)) = row else {
        return Ok(None);
    };
    let parsed: Value = serde_json::from_str(&streams_json)?;
    let distance = f64_stream(&parsed, "distance").unwrap_or_default();
    let watts = f64_stream(&parsed, "watts");
    let heartrate = f64_stream(&parsed, "heartrate");

    let idxs: Vec<usize> = distance
        .iter()
        .enumerate()
        .filter(|&(_, &d)| d >= start_m && d <= end_m)
        .map(|(i, _)| i)
        .collect();
    if idxs.len() < 2 {
        return Ok(None);
    }
    let sampled = downsample_indices(idxs.len(), 200);
    let sel: Vec<usize> = sampled.iter().map(|&i| idxs[i]).collect();
    let distance_km: Vec<f64> = sel.iter().map(|&i| (distance[i] - start_m) / 1000.0).collect();
    let power = pick(watts.as_deref(), &sel);
    let hr = pick(heartrate.as_deref(), &sel);
    Ok(Some(SegmentCurve { distance_km, power, hr }))
}

/// Only meaningful with >1 row — Strava always returns at least one lap
/// (the whole ride) even with no manual laps pressed.
async fn fetch_laps(pool: &SqlitePool, activity_id: i64) -> anyhow::Result<Vec<LapRow>> {
    let rows: Vec<(i64, i64, f64, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT lap_index, elapsed_time_s, distance_m, avg_power_w, avg_hr FROM laps WHERE activity_id = ? ORDER BY lap_index",
    )
    .bind(activity_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(lap_index, elapsed_time_s, distance_m, avg_power_w, avg_hr)| LapRow {
            lap_index,
            elapsed_time_s,
            distance_m,
            avg_power_w,
            avg_hr,
        })
        .collect())
}

fn f64_stream(v: &Value, key: &str) -> Option<Vec<f64>> {
    v.get(key)?.get("data")?.as_array()?.iter().map(|x| x.as_f64()).collect()
}

fn latlng_stream(v: &Value) -> Option<Vec<[f64; 2]>> {
    v.get("latlng")?
        .get("data")?
        .as_array()?
        .iter()
        .map(|pair| {
            let arr = pair.as_array()?;
            Some([arr.first()?.as_f64()?, arr.get(1)?.as_f64()?])
        })
        .collect()
}

fn downsample_indices(n: usize, max_points: usize) -> Vec<usize> {
    let stride = (n / max_points.max(1)).max(1);
    (0..n).step_by(stride).collect()
}

/// Samples `arr` at `indices`, clamping to the last known value if `arr` is
/// shorter than expected (shouldn't happen — Strava's parallel streams are
/// normally all the same length — but keeps this defensive rather than
/// panicking on a malformed stream).
fn pick(arr: Option<&[f64]>, indices: &[usize]) -> Option<Vec<f64>> {
    arr.map(|a| {
        indices
            .iter()
            .map(|&i| a.get(i).copied().unwrap_or_else(|| *a.last().unwrap_or(&0.0)))
            .collect()
    })
}

/// Looks up the distance (m) covered at a given elapsed-time offset, via the
/// nearest sample in `time` (assumed sorted ascending) — used to convert a
/// lap's cumulative-elapsed-time boundary (from the Strava laps API, which
/// only gives durations) into a distance-km boundary for the distance-based
/// chart's x-axis.
fn distance_at_time_s(time: &[f64], distance: &[f64], t_s: f64) -> f64 {
    let idx = time.partition_point(|&t| t < t_s).min(time.len().saturating_sub(1));
    distance.get(idx).copied().unwrap_or(0.0)
}

/// Derives a speed (km/h) stream from distance+time deltas, same length as
/// `time` so it lines up positionally with the other streams.
fn build_speed_stream(time: &[f64], distance: &[f64]) -> Vec<f64> {
    let n = time.len().min(distance.len());
    let mut speed = vec![0.0; n];
    for i in 1..n {
        let dt = (time[i] - time[i - 1]).max(0.1);
        let dd = (distance[i] - distance[i - 1]).max(0.0);
        speed[i] = dd / dt * 3.6;
    }
    if n > 1 {
        speed[0] = speed[1];
    }
    speed
}

/// Running pace stream in minutes per km (the unit the pace axis plots),
/// derived from the km/h speed stream. Standing still / GPS drift would give
/// an infinite pace, so anything slower than 15 min/km is clamped to 15.
fn pace_from_speed(speed_kmh: &[f64]) -> Vec<f64> {
    speed_kmh
        .iter()
        .map(|v| if *v > 4.0 { 60.0 / v } else { MAX_PACE_MIN_PER_KM })
        .collect()
}

const MAX_PACE_MIN_PER_KM: f64 = 15.0;

/// Consistent color scheme across the user's training tools: rouge=FC,
/// bleu clair=vitesse, orange=cadence, mauve=puissance, altitude=aire pleine grise.
const SERIES_COLOR_POWER: &str = "#a855f7";
const SERIES_COLOR_HR: &str = "#dc2626";
const SERIES_COLOR_SPEED: &str = "#38bdf8";
const SERIES_COLOR_CADENCE: &str = "#f97316";
const SERIES_COLOR_ALTITUDE: &str = "#9ca3af";

/// Interactive overlay chart via Chart.js (CDN-loaded once in the page
/// head) instead of a hand-rolled SVG renderer — proper axes, legend,
/// tooltips, and antialiasing for free. Altitude is always plotted as a
/// filled area on its own (hidden) axis; up to 2 of {power, hr, speed,
/// cadence} can be visible at once, each on its own left/right y-axis so
/// very different units stay readable — enforced via a custom legend
/// click handler since Chart.js's default just toggles freely.
#[allow(clippy::too_many_arguments)]
fn render_combo_chart(
    distance_km: &[f64],
    time_min: &[f64],
    altitude: Option<&[f64]>,
    power: Option<&[f64]>,
    hr: Option<&[f64]>,
    speed: Option<&[f64]>,
    cadence: Option<&[f64]>,
    is_run: bool,
) -> String {
    let n = distance_km.len().min(time_min.len());
    if n < 2 {
        return "<p class=\"hint\">Pas de données</p>".to_string();
    }
    // A much higher cap than the old 300 — that budget was fine zoomed out
    // over a whole ride, but left a lap/segment zoom looking far coarser
    // than the PR/last-effort overlay curve (sampled densely within just
    // that short window). Most rides (1-2h at ~1Hz) fit under this cap
    // entirely, i.e. full resolution everywhere; only very long activities
    // still downsample, and even then far less aggressively.
    let indices = downsample_indices(n, 8000);
    let x_values: Vec<f64> = indices.iter().map(|&i| distance_km[i]).collect();
    let t_values: Vec<f64> = indices.iter().map(|&i| time_min[i]).collect();
    let alt = pick(altitude, &indices);
    let pw = pick(power, &indices);
    let hrv = pick(hr, &indices);
    let sp = pick(speed, &indices);
    let cad = pick(cadence, &indices);

    if alt.is_none() && pw.is_none() && hrv.is_none() && sp.is_none() && cad.is_none() {
        return "<p class=\"hint\">Pas de données</p>".to_string();
    }

    let mut datasets: Vec<String> = Vec::new();
    let mut scales: Vec<String> = Vec::new();
    // Full-resolution series, kept in JS as the source data for client-side
    // resampling: whatever the current zoom window is, only ~400 points
    // from within it are ever assigned to a dataset's `data`, so a lap/
    // segment zoom stays exactly as readable as the full-ride view instead
    // of dumping thousands of points into a narrow x-range.
    let mut full_series: Vec<String> = Vec::new();

    // Fixed axis bounds computed from the *full* activity, not just the
    // visible/downsampled window — otherwise zooming to a lap rescales the
    // y-axis to that lap's own min/max, making laps visually incomparable.
    let full_min_max = |data: &[f64]| -> (f64, f64) {
        let min = data.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = data.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (min, max)
    };

    if let Some(alt) = &alt {
        let (min, max) = full_min_max(altitude.unwrap_or(&[]));
        datasets.push(format!(
            r#"{{label:"Altitude (m)",data:[],_fullKey:"alt",borderColor:"{c}",backgroundColor:"rgba(156,163,175,0.35)",fill:true,yAxisID:"yAlt",pointRadius:0,borderWidth:1,order:10,tension:0.2}}"#,
            c = SERIES_COLOR_ALTITUDE,
        ));
        full_series.push(format!("alt:{}", serde_json::to_string(alt).unwrap()));
        scales.push(format!(
            r#""yAlt":{{display:false,position:"left",min:{:.0},max:{:.0}}}"#,
            (min - 10.0).max(0.0),
            max + 10.0
        ));
    }

    let candidates: [(&str, &Option<Vec<f64>>, Option<&[f64]>, &str, &str, &str); 4] = [
        ("power", &pw, power, SERIES_COLOR_POWER, "W", "Puissance"),
        ("hr", &hrv, hr, SERIES_COLOR_HR, "bpm", "FC"),
        (
            "speed",
            &sp,
            speed,
            SERIES_COLOR_SPEED,
            if is_run { "min/km" } else { "km/h" },
            if is_run { "Allure" } else { "Vitesse" },
        ),
        ("cadence", &cad, cadence, SERIES_COLOR_CADENCE, "rpm", "Cadence"),
    ];

    let mut available: Vec<(&str, &str, &str)> = Vec::new(); // (key, label, color)
    for (i, (key, data, full_data, color, unit, label)) in candidates.iter().enumerate() {
        let Some(d) = data else { continue };
        available.push((key, label, color));
        let axis_id = format!("y_{key}");
        let visible = available.len() <= 2;
        let side = if i % 2 == 0 { "left" } else { "right" };
        datasets.push(format!(
            r#"{{label:"{label} ({unit})",key:"{key}",data:[],_fullKey:"{key}",borderColor:"{color}",backgroundColor:"{color}",yAxisID:"{axis_id}",pointRadius:0,borderWidth:1.5,hidden:{},tension:0.2}}"#,
            !visible,
        ));
        full_series.push(format!("{key}:{}", serde_json::to_string(d).unwrap()));
        let (min, max) = full_min_max(full_data.unwrap_or(&[]));
        if is_run && *key == "speed" {
            // Pace axis: inverted (faster = higher) with m:ss ticks. Bounds
            // snapped to whole minutes so the ticks land on round values.
            scales.push(format!(
                r#""{axis_id}":{{display:{visible},position:"{side}",reverse:true,min:{:.0},max:{:.0},title:{{display:true,text:"{unit}",color:"{color}"}},ticks:{{color:"{color}",callback:(v)=>fmtPace(v)}},grid:{{drawOnChartArea:false}}}}"#,
                min.floor().max(2.0),
                max.ceil().min(MAX_PACE_MIN_PER_KM),
            ));
            continue;
        }
        let axis_min = if *key == "hr" { (min - 5.0).max(0.0) } else { 0.0 };
        scales.push(format!(
            r#""{axis_id}":{{display:{visible},position:"{side}",min:{axis_min:.0},max:{:.0},title:{{display:true,text:"{unit}",color:"{color}"}},ticks:{{color:"{color}"}},grid:{{drawOnChartArea:false}}}}"#,
            max + max * 0.05,
        ));
    }

    // Two <select> dropdowns instead of legend checkboxes/clicks — picking
    // which 2 series to overlay this way, per user feedback that felt more
    // intuitive than toggling boxes.
    let option_tags = |skip: usize| -> String {
        available
            .iter()
            .enumerate()
            .map(|(i, (key, label, _))| {
                format!(
                    "<option value=\"{key}\"{selected}>{}</option>",
                    escape_html(label),
                    selected = if i == skip { " selected" } else { "" }
                )
            })
            .collect()
    };
    let selectors_html = if available.len() >= 2 {
        format!(
            "<div class=\"chart-selectors\">\
             <label><span class=\"dot\" id=\"select1-dot\"></span>Courbe 1 <select id=\"series-select-1\">{}</select></label>\
             <label><span class=\"dot\" id=\"select2-dot\"></span>Courbe 2 <select id=\"series-select-2\">{}</select></label>\
             </div>",
            option_tags(0),
            option_tags(1)
        )
    } else {
        String::new()
    };
    let colors_js: String = available
        .iter()
        .map(|(key, _, color)| format!("{key}:\"{color}\""))
        .collect::<Vec<_>>()
        .join(",");

    let x_json = serde_json::to_string(&x_values).unwrap();
    let t_json = serde_json::to_string(&t_values).unwrap();
    let x_min = x_values.first().copied().unwrap_or(0.0);
    let x_max = x_values.last().copied().unwrap_or(0.0);
    let datasets_js = datasets.join(",");
    let scales_js = scales.join(",\n        ");
    let full_series_js = full_series.join(",");

    // Numeric linear x-axis (distance in km), not category labels — lets
    // "zoom to this lap/segment" just set scales.x.min/max and call update().
    // T (elapsed minutes, parallel to X) is kept only to show a "at 12:34"
    // hint in the tooltip alongside the distance.
    format!(
        r##"{selectors_html}
<div class="combo-wrap"><canvas id="combo-chart"></canvas></div>
<script>
(function() {{
  const ctx = document.getElementById('combo-chart').getContext('2d');
  if (!Chart.Tooltip.positioners.cursor) {{
    Chart.Tooltip.positioners.cursor = function(elements, eventPosition) {{ return eventPosition; }};
  }}
  const X = {x_json};
  const T = {t_json};
  const FULL = {{ {full_series_js} }};
  // Whatever the current zoom window, at most this many points are ever
  // handed to a dataset — a lap/segment zoom stays exactly as readable as
  // the full-ride view instead of dumping the full-resolution buffer into
  // a narrow x-range.
  const MAX_VISIBLE_POINTS = 400;
  const COLORS = {{{colors_js}}};
  function fmtTime(mins) {{
    const totalSec = Math.round(mins*60);
    const h = Math.floor(totalSec/3600), m = Math.floor((totalSec%3600)/60), s = totalSec%60;
    return (h>0 ? h+':'+String(m).padStart(2,'0') : m) + ':' + String(s).padStart(2,'0');
  }}
  function fmtKm(km) {{ return km.toFixed(2) + ' km'; }}
  function fmtPace(minPerKm) {{
    const total = Math.round(minPerKm * 60);
    return Math.floor(total / 60) + ':' + String(total % 60).padStart(2, '0');
  }}
  const IS_RUN = {is_run};
  function lowerBound(arr, val) {{
    let lo = 0, hi = arr.length;
    while (lo < hi) {{ const mid = (lo + hi) >> 1; if (arr[mid] < val) lo = mid + 1; else hi = mid; }}
    return lo;
  }}
  function nearestT(km) {{
    const i = Math.min(lowerBound(X, km), X.length - 1);
    return T[i];
  }}
  function applyResample(xMin, xMax) {{
    const lo = lowerBound(X, xMin);
    const hi = Math.min(lowerBound(X, xMax) + 1, X.length);
    const count = Math.max(0, hi - lo);
    const stride = Math.max(1, Math.ceil(count / MAX_VISIBLE_POINTS));
    const idxs = [];
    for (let i = lo; i < hi; i += stride) idxs.push(i);
    chart.data.datasets.forEach(ds => {{
      if (!ds._fullKey) return;
      const src = FULL[ds._fullKey];
      ds.data = idxs.map(i => ({{x: X[i], y: src[i]}}));
    }});
  }}
  const chart = new Chart(ctx, {{
    type: 'line',
    data: {{ labels: X, datasets: [{datasets_js}] }},
    options: {{
      responsive: true,
      maintainAspectRatio: false,
      animation: false,
      interaction: {{ mode: 'x', intersect: false }},
      onHover: function(evt, elements) {{
        if (elements.length) {{
          const xVal = chart.scales.x.getValueForPixel(evt.x);
          if (window.showMapPointAtDistance) window.showMapPointAtDistance(xVal);
        }} else if (window.clearMapPoint) {{
          window.clearMapPoint();
        }}
      }},
      plugins: {{
        legend: {{ display: false }},
        tooltip: {{
          mode: 'x', intersect: false, position: 'cursor',
          filter: (item, index, items) => items.findIndex(i => i.datasetIndex === item.datasetIndex) === index,
          callbacks: {{
            title: (items) => items.length ? fmtKm(items[0].parsed.x) + ' (' + fmtTime(nearestT(items[0].parsed.x)) + ')' : '',
            label: (item) => IS_RUN && item.dataset.key === 'speed'
              ? 'Allure: ' + fmtPace(item.parsed.y) + ' /km'
              : item.dataset.label + ': ' + item.formattedValue
          }}
        }},
        annotation: {{ annotations: {{ hoverBand: {{
          type: 'box', xMin: 0, xMax: 0, backgroundColor: 'rgba(37,99,235,0.12)', borderWidth: 0, display: false
        }} }} }}
      }},
      scales: {{
        x: {{ type: 'linear', min: {x_min}, max: {x_max}, ticks: {{ maxTicksLimit: 8, color: '#9ca3af', callback: (v) => fmtKm(v) }}, grid: {{ display: false }} }},
        {scales_js}
      }}
    }}
  }});
  applyResample(chart.options.scales.x.min, chart.options.scales.x.max);
  chart.update('none');
  document.getElementById('combo-chart').addEventListener('mouseleave', function() {{
    if (window.clearMapPoint) window.clearMapPoint();
  }});
  window.comboChart = chart;

  const sel1 = document.getElementById('series-select-1');
  const sel2 = document.getElementById('series-select-2');
  function applySelection() {{
    if (!sel1 || !sel2) return;
    const chosen = [sel1.value, sel2.value];
    chart.data.datasets.forEach((ds, i) => {{
      if (ds.yAxisID === 'yAlt') return;
      const active = chosen.includes(ds.key);
      chart.getDatasetMeta(i).hidden = !active;
      const axis = chart.options.scales[ds.yAxisID];
      if (axis) {{
        axis.display = active;
        if (active) axis.position = ds.key === sel1.value ? 'left' : 'right';
      }}
    }});
    document.querySelectorAll('#series-select-1 option').forEach(o => o.disabled = (o.value === sel2.value));
    document.querySelectorAll('#series-select-2 option').forEach(o => o.disabled = (o.value === sel1.value));
    const d1 = document.getElementById('select1-dot'), d2 = document.getElementById('select2-dot');
    if (d1) d1.style.background = COLORS[sel1.value] || 'transparent';
    if (d2) d2.style.background = COLORS[sel2.value] || 'transparent';
    chart.update();
  }}
  window.applyChartSelection = applySelection;
  if (sel1 && sel2) {{
    sel1.addEventListener('change', applySelection);
    sel2.addEventListener('change', applySelection);
    applySelection();
  }}

  window.zoomToRange = function(startMin, endMin) {{
    chart.options.scales.x.min = startMin;
    chart.options.scales.x.max = endMin;
    applyResample(startMin, endMin);
    chart.update();
    document.querySelectorAll('.lap-row').forEach(el => el.classList.remove('lap-row-active'));
  }};
  window.resetZoom = function() {{
    chart.options.scales.x.min = {x_min};
    chart.options.scales.x.max = {x_max};
    applyResample({x_min}, {x_max});
    chart.update();
    document.querySelectorAll('.lap-row').forEach(el => el.classList.remove('lap-row-active'));
    if (window.resetZoneHighlight) window.resetZoneHighlight();
  }};
  // Hover preview (before committing to a zoom): shaded band on the chart,
  // does not touch the x-axis min/max so it doesn't fight with an active zoom.
  window.previewLap = function(startMin, endMin) {{
    const ann = chart.options.plugins.annotation.annotations.hoverBand;
    ann.xMin = startMin; ann.xMax = endMin; ann.display = true;
    chart.update('none');
  }};
  window.clearLapPreview = function() {{
    chart.options.plugins.annotation.annotations.hoverBand.display = false;
    chart.update('none');
  }};
}})();
</script>"##,
    )
}

pub(super) const HR_ZONE_NAMES: [&str; 5] = ["Récupération", "Endurance", "Tempo", "Seuil", "VO2max/Anaérobie"];
pub(super) const HR_ZONE_BOUNDS: [f64; 6] = [0.0, 0.60, 0.70, 0.80, 0.90, f64::INFINITY];
// Matches the user's other training tools: gris/bleu/vert/orange/rouge.
const HR_ZONE_COLORS: [&str; 5] = ["#9ca3af", "#3b82f6", "#22c55e", "#f97316", "#dc2626"];

/// Karvonen 5-zone model: %HRR (heart rate reserve) + HR rest, distinct
/// from the 7-zone power model — HR and power don't share a boundary set.
fn hr_zone(bpm: f64, hr_rest: f64, hr_max: f64) -> usize {
    let hrr = (hr_max - hr_rest).max(1.0);
    let pct = (bpm - hr_rest) / hrr;
    for i in 0..5 {
        if pct < HR_ZONE_BOUNDS[i + 1] {
            return i;
        }
    }
    4
}

fn compute_hr_zone_secs(heartrate: &[f64], time: &[f64], hr_rest: f64, hr_max: f64) -> [i64; 5] {
    let mut zones = [0i64; 5];
    let n = heartrate.len().min(time.len());
    for i in 1..n {
        let dt = (time[i] - time[i - 1]).max(0.0) as i64;
        zones[hr_zone(heartrate[i], hr_rest, hr_max)] += dt;
    }
    zones
}

/// Restricted to samples whose elapsed time falls within [start_s, end_s] —
/// used to break a single lap's contribution to each zone out of the
/// whole-activity total, for the "fade the rest of this zone's bar" feature.
fn compute_lap_hr_zone_secs(
    heartrate: &[f64],
    time: &[f64],
    start_s: f64,
    end_s: f64,
    hr_rest: f64,
    hr_max: f64,
) -> [i64; 5] {
    let mut zones = [0i64; 5];
    let n = heartrate.len().min(time.len());
    for i in 1..n {
        if time[i] < start_s || time[i] > end_s {
            continue;
        }
        let dt = (time[i] - time[i - 1]).max(0.0) as i64;
        zones[hr_zone(heartrate[i], hr_rest, hr_max)] += dt;
    }
    zones
}

/// 1-based zone index (1..=7), matching metrics::power_zone's boundaries
/// (kept in sync via `super::ZONE_BOUNDS`, which is what the power zone
/// bars themselves use).
fn power_zone_index(watts: f64, ftp: f64) -> usize {
    let pct = watts / ftp;
    for i in 0..7 {
        if pct < super::ZONE_BOUNDS[i + 1] {
            return i + 1;
        }
    }
    7
}

fn compute_lap_power_zone_secs(watts: &[f64], time: &[f64], start_s: f64, end_s: f64, ftp: f64) -> [i64; 7] {
    let mut zones = [0i64; 7];
    let n = watts.len().min(time.len());
    for i in 1..n {
        if time[i] < start_s || time[i] > end_s {
            continue;
        }
        let dt = (time[i] - time[i - 1]).max(0.0) as i64;
        zones[power_zone_index(watts[i], ftp) - 1] += dt;
    }
    zones
}

fn render_hr_zone_bar(zone_secs: &[i64; 5], hr_rest: f64, hr_max: f64) -> String {
    let hrr = (hr_max - hr_rest).max(1.0);
    let rows: Vec<(String, String, &str, i64, String)> = (0..5)
        .rev()
        .map(|i| {
            let low = hr_rest + HR_ZONE_BOUNDS[i] * hrr;
            let high = HR_ZONE_BOUNDS[i + 1];
            let range = if high.is_finite() {
                format!("{low:.0}-{:.0}bpm", hr_rest + high * hrr)
            } else {
                format!(">{low:.0}bpm")
            };
            let name = format!("Z{} {}", i + 1, HR_ZONE_NAMES[i]);
            (name, range, HR_ZONE_COLORS[i], zone_secs[i], format!("hr_{}", i + 1))
        })
        .collect();
    super::render_zone_rows(&rows)
}

/// Best average power over standard durations — a rolling-window max via
/// prefix sums, O(n) per duration. Assumes near-1Hz sampling (Strava's
/// time-based streams normally are), so "duration in seconds" ≈ "window in
/// samples" — good enough for a first cut, not exact for gappy streams.
fn power_curve(watts: &[f64]) -> Vec<(i64, f64)> {
    const DURATIONS_S: [i64; 7] = [5, 15, 30, 60, 300, 600, 1200];
    let n = watts.len();
    let mut prefix = vec![0.0; n + 1];
    for (i, w) in watts.iter().enumerate() {
        prefix[i + 1] = prefix[i] + w;
    }
    DURATIONS_S
        .iter()
        .filter_map(|&d| {
            let window = d as usize;
            if window == 0 || window > n {
                return None;
            }
            let best = (0..=(n - window))
                .map(|start| (prefix[start + window] - prefix[start]) / window as f64)
                .fold(0.0f64, f64::max);
            Some((d, best))
        })
        .collect()
}

fn format_duration_label(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}min", seconds / 60)
    }
}

/// `distance_km` must be the same length as `latlng` (both straight from the
/// raw streams, positionally aligned) — needed so a lap/segment's
/// [start,end] km range can be mapped back to the matching GPS points for
/// the hover highlight.
fn render_leaflet_map(latlng: &[[f64; 2]], distance_km: &[f64]) -> String {
    if latlng.len() < 2 {
        return "<p class=\"hint\">Pas de données GPS</p>".to_string();
    }
    let n = latlng.len().min(distance_km.len());
    let indices = downsample_indices(n, 500);
    let sampled: Vec<[f64; 2]> = indices.iter().map(|&i| latlng[i]).collect();
    let sampled_distances: Vec<f64> = indices.iter().map(|&i| distance_km[i]).collect();
    let lat_mid = sampled.iter().map(|p| p[0]).sum::<f64>() / sampled.len() as f64;
    let lng_mid = sampled.iter().map(|p| p[1]).sum::<f64>() / sampled.len() as f64;
    let points_json = serde_json::to_string(&sampled).unwrap_or_else(|_| "[]".to_string());
    let distances_json = serde_json::to_string(&sampled_distances).unwrap_or_else(|_| "[]".to_string());

    format!(
        r##"<div id="detail-map" class="detail-map" style="border-radius:8px;"></div>
<link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css" crossorigin="" />
<script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js" crossorigin=""></script>
<script>
(function() {{
  var map = L.map('detail-map').setView([{lat_mid}, {lng_mid}], 12);
  L.tileLayer('https://{{s}}.tile.openstreetmap.org/{{z}}/{{x}}/{{y}}.png', {{
    maxZoom: 19,
    attribution: '&copy; OpenStreetMap contributors'
  }}).addTo(map);
  var route = L.polyline({points_json}, {{color: cssVar('--accent'), weight: 3}}).addTo(map);
  map.fitBounds(route.getBounds());

  var MAP_DISTANCES = {distances_json};
  var MAP_POINTS = {points_json};
  var highlightLayer = null;
  window.highlightLapOnMap = function(startKm, endKm) {{
    var segment = [];
    for (var i = 0; i < MAP_DISTANCES.length; i++) {{
      if (MAP_DISTANCES[i] >= startKm && MAP_DISTANCES[i] <= endKm) segment.push(MAP_POINTS[i]);
    }}
    if (segment.length < 2) return;
    if (highlightLayer) map.removeLayer(highlightLayer);
    highlightLayer = L.polyline(segment, {{color: '#dc2626', weight: 5, opacity: 0.9}}).addTo(map);
  }};
  window.clearLapMapHighlight = function() {{
    if (highlightLayer) {{ map.removeLayer(highlightLayer); highlightLayer = null; }}
  }};

  var pointMarker = null;
  window.showMapPointAtDistance = function(km) {{
    var idx = 0, best = Infinity;
    for (var i = 0; i < MAP_DISTANCES.length; i++) {{
      var d = Math.abs(MAP_DISTANCES[i] - km);
      if (d < best) {{ best = d; idx = i; }}
    }}
    var p = MAP_POINTS[idx];
    if (!p) return;
    if (!pointMarker) {{
      pointMarker = L.circleMarker(p, {{radius: 7, color: '#fff', weight: 2, fillColor: cssVar('--accent'), fillOpacity: 1}}).addTo(map);
    }} else {{
      pointMarker.setLatLng(p);
    }}
  }};
  window.clearMapPoint = function() {{
    if (pointMarker) {{ map.removeLayer(pointMarker); pointMarker = null; }}
  }};
}})();
</script>"##
    )
}

pub async fn activity_detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<WarnQuery>,
) -> Html<String> {
    let activity = fetch_activity(&state.pool, id).await.ok().flatten();
    let Some(activity) = activity else {
        return Html("<p>Activité introuvable. <a href=\"/\">Retour</a></p>".to_string());
    };
    let segment_efforts = fetch_segment_efforts(&state.pool, id).await.unwrap_or_default();
    let laps = fetch_laps(&state.pool, id).await.unwrap_or_default();
    let (ftp_watts, hr_rest, hr_max): (i64, i64, i64) =
        sqlx::query_as("SELECT ftp_watts, hr_rest, hr_max FROM athlete_config WHERE id = 1")
            .fetch_one(&state.pool)
            .await
            .unwrap_or((205, 53, 192));

    let streams: Option<Value> = activity
        .streams_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());

    let time = streams.as_ref().and_then(|v| f64_stream(v, "time")).unwrap_or_default();
    let watts = streams.as_ref().and_then(|v| f64_stream(v, "watts"));
    let heartrate = streams.as_ref().and_then(|v| f64_stream(v, "heartrate"));
    let altitude = streams.as_ref().and_then(|v| f64_stream(v, "altitude"));
    let distance = streams.as_ref().and_then(|v| f64_stream(v, "distance"));
    let cadence = streams.as_ref().and_then(|v| f64_stream(v, "cadence"));
    let latlng = streams.as_ref().and_then(latlng_stream).unwrap_or_default();

    let to_minutes = |t: &[f64]| -> Vec<f64> { t.iter().map(|s| s / 60.0).collect() };
    let time_min = to_minutes(&time);
    let speed = distance.as_ref().map(|d| build_speed_stream(&time, d));
    // Runners think in pace (min/km), not km/h: the "speed" series becomes a
    // pace series for runs.
    let speed = if activity.is_run { speed.map(|s| pace_from_speed(&s)) } else { speed };
    let distance_km: Vec<f64> = distance.as_ref().map(|d| d.iter().map(|m| m / 1000.0).collect()).unwrap_or_default();

    let combo_chart_html = render_combo_chart(
        &distance_km,
        &time_min,
        altitude.as_deref(),
        watts.as_deref(),
        heartrate.as_deref(),
        speed.as_deref(),
        cadence.as_deref(),
        activity.is_run,
    );

    let hr_zone_secs_total = heartrate
        .as_ref()
        .map(|h| compute_hr_zone_secs(h, &time, hr_rest as f64, hr_max as f64));
    let hr_zone_html = hr_zone_secs_total
        .as_ref()
        .map(|zones| render_hr_zone_bar(zones, hr_rest as f64, hr_max as f64));

    // Per-lap zone breakdown, embedded for the client to compute "what
    // fraction of each zone's total time falls inside this lap" and fade
    // the rest of that zone's bar accordingly when a lap is selected.
    let lap_zones_json = if laps.len() > 1 {
        let mut cursor_s = 0.0f64;
        let per_lap: Vec<serde_json::Value> = laps
            .iter()
            .map(|l| {
                let start_s = cursor_s;
                cursor_s += l.elapsed_time_s as f64;
                let end_s = cursor_s;
                let power = watts.as_deref().map(|w| {
                    compute_lap_power_zone_secs(w, &time, start_s, end_s, ftp_watts as f64)
                });
                let hr = heartrate.as_deref().map(|h| {
                    compute_lap_hr_zone_secs(h, &time, start_s, end_s, hr_rest as f64, hr_max as f64)
                });
                serde_json::json!({ "power": power, "hr": hr })
            })
            .collect();
        serde_json::to_string(&per_lap).unwrap_or_else(|_| "[]".to_string())
    } else {
        "[]".to_string()
    };
    let power_zone_secs_total_json = serde_json::to_string(&activity.zone_secs).unwrap();
    let hr_zone_secs_total_json =
        serde_json::to_string(&hr_zone_secs_total.unwrap_or([0i64; 5])).unwrap();

    let power_curve_data = watts.as_ref().map(|w| power_curve(w));
    let power_curve_chart = power_curve_data.as_ref().map(|pc| {
        let labels: Vec<String> = pc.iter().map(|(d, _)| format_duration_label(*d)).collect();
        let values: Vec<f64> = pc.iter().map(|(_, p)| *p).collect();
        format!(
            r##"<div style="height:180px"><canvas id="power-curve-chart"></canvas></div>
<script>
(function() {{
  if (!Chart.Tooltip.positioners.cursor) {{
    Chart.Tooltip.positioners.cursor = function(elements, eventPosition) {{ return eventPosition; }};
  }}
  new Chart(document.getElementById('power-curve-chart').getContext('2d'), {{
    type: 'line',
    data: {{ labels: {labels}, datasets: [{{ label: 'Puissance (W)', data: {values}, borderColor: '{color}', backgroundColor: '{color}', pointRadius: 3, borderWidth: 2, tension: 0.25 }}] }},
    options: {{
      responsive: true, maintainAspectRatio: false, animation: false,
      plugins: {{ legend: {{ display: false }}, tooltip: {{ mode: 'index', intersect: false, position: 'cursor' }} }},
      scales: {{ y: {{ title: {{ display: true, text: 'W' }} }}, x: {{ title: {{ display: true, text: 'Durée' }} }} }}
    }}
  }});
}})();
</script>"##,
            labels = serde_json::to_string(&labels).unwrap(),
            values = serde_json::to_string(&values).unwrap(),
            color = SERIES_COLOR_POWER,
        )
    });
    let power_curve_cells = power_curve_data.as_ref().map(|pc| {
        pc.iter()
            .map(|(d, p)| format!("<div class=\"pc-cell\"><div class=\"pc-value\">{p:.0}W</div><div class=\"pc-label\">{}</div></div>", format_duration_label(*d)))
            .collect::<String>()
    });

    let map_html = render_leaflet_map(&latlng, &distance_km);

    // For each segment effort, pull the PR's and last effort's own power/HR
    // curve (from whichever other activity produced them) so the segments
    // tab can overlay a "ghost" of that pacing on the current chart,
    // distance-shifted onto this activity's own [start_km, end_km] window
    // for that segment.
    let mut segments_data: Vec<Value> = Vec::new();
    for s in &segment_efforts {
        let (Some(start_m), Some(end_m)) = (s.start_distance_m, s.end_distance_m) else {
            continue;
        };
        let pr_curve = match s.pr_activity_id {
            Some(aid) => fetch_segment_curve(&state.pool, aid, s.reference_segment_id).await.ok().flatten(),
            None => None,
        };
        let last_curve = match s.last_activity_id {
            Some(aid) => fetch_segment_curve(&state.pool, aid, s.reference_segment_id).await.ok().flatten(),
            None => None,
        };
        let curve_json = |c: &Option<SegmentCurve>| -> Value {
            match c {
                Some(c) => serde_json::json!({
                    "distance_km": c.distance_km,
                    "power": c.power,
                    "hr": c.hr,
                }),
                None => Value::Null,
            }
        };
        segments_data.push(serde_json::json!({
            "start_km": start_m / 1000.0,
            "end_km": end_m / 1000.0,
            "pr": curve_json(&pr_curve),
            "last": curve_json(&last_curve),
        }));
    }
    let segments_data_json = serde_json::to_string(&segments_data).unwrap_or_else(|_| "[]".to_string());

    let segments_rows_html = if segment_efforts.is_empty() {
        "<p class=\"hint\">Aucun segment détecté sur cette sortie.</p>".to_string()
    } else {
        let rows = segment_efforts
            .iter()
            .enumerate()
            .map(|(idx, s)| {
                let data = &segments_data[idx];
                let start_km = data["start_km"].as_f64().unwrap_or(0.0);
                let end_km = data["end_km"].as_f64().unwrap_or(0.0);
                let has_pr = !data["pr"].is_null();
                let has_last = !data["last"].is_null();
                let mut deltas = Vec::new();
                if let Some(v) = s.delta_vs_pr_pct {
                    deltas.push(format!("{v:+.1}% vs RP"));
                }
                if let Some(v) = s.delta_vs_last_pct {
                    deltas.push(format!("{v:+.1}% vs dernier"));
                }
                format!(
                    "<tr class=\"lap-row segment-row\" \
                     onclick=\"window.zoomToRange({start_km},{end_km});this.parentElement.querySelectorAll('.lap-row').forEach(r=>r.classList.remove('lap-row-active'));this.classList.add('lap-row-active')\" \
                     onmouseenter=\"window.previewLap && window.previewLap({start_km},{end_km});window.highlightLapOnMap && window.highlightLapOnMap({start_km},{end_km})\" \
                     onmouseleave=\"window.clearLapPreview && window.clearLapPreview();window.clearLapMapHighlight && window.clearLapMapHighlight()\">\
                     <td>{}</td><td>{:.0}W{}</td>\
                     <td><input type=\"checkbox\" class=\"segment-overlay-cb\" onclick=\"event.stopPropagation()\" onchange=\"window.toggleSegmentOverlay(this,'pr',{idx})\" {}></td>\
                     <td><input type=\"checkbox\" class=\"segment-overlay-cb\" onclick=\"event.stopPropagation()\" onchange=\"window.toggleSegmentOverlay(this,'last',{idx})\" {}></td>\
                     </tr>",
                    escape_html(&s.name),
                    s.avg_power_w,
                    if deltas.is_empty() { String::new() } else { format!("<br><span class=\"hint\">{}</span>", deltas.join(", ")) },
                    if has_pr { "" } else { "disabled" },
                    if has_last { "" } else { "disabled" },
                )
            })
            .collect::<String>();
        format!(
            "<table class=\"laps-table segments-table\">\
             <thead><tr><th>Segment</th><th>Puissance</th><th>RP</th><th>Dernier</th></tr></thead>\
             <tbody>{rows}</tbody></table>"
        )
    };

    let laps_rows_html = if laps.len() > 1 {
        let mut cursor_s: i64 = 0;
        let rows = laps
            .iter()
            .enumerate()
            .map(|(pos, l)| {
                let start_km = distance_at_time_s(&time, distance.as_deref().unwrap_or(&[]), cursor_s as f64) / 1000.0;
                cursor_s += l.elapsed_time_s;
                let end_km = distance_at_time_s(&time, distance.as_deref().unwrap_or(&[]), cursor_s as f64) / 1000.0;
                // Position in the sorted list, matching LAP_ZONES — the stored
                // `lap_index` is 0-based from Strava but 1-based from imports.
                let lap_index = pos + 1;
                format!(
                    "<tr class=\"lap-row\" \
                     onclick=\"window.zoomToRange({start_km},{end_km});window.highlightLapInZones && window.highlightLapInZones({lap_index});this.classList.add('lap-row-active')\" \
                     onmouseenter=\"window.previewLap && window.previewLap({start_km},{end_km});window.highlightLapOnMap && window.highlightLapOnMap({start_km},{end_km})\" \
                     onmouseleave=\"window.clearLapPreview && window.clearLapPreview();window.clearLapMapHighlight && window.clearLapMapHighlight()\">\
                     <td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    l.lap_index,
                    super::format_zone_duration(l.elapsed_time_s),
                    if activity.is_run {
                        super::format::format_pace(l.distance_m, l.elapsed_time_s)
                            .map(|p| format!("{p} /km"))
                            .unwrap_or_else(|| "—".to_string())
                    } else {
                        l.avg_power_w.map(|v| format!("{v:.0}W")).unwrap_or_else(|| "—".to_string())
                    },
                    l.avg_hr.map(|v| format!("{v:.0}bpm")).unwrap_or_else(|| "—".to_string()),
                )
            })
            .collect::<String>();
        format!(
            "<table class=\"laps-table\"><thead><tr><th>#</th><th>Durée</th><th>{}</th><th>FC</th></tr></thead><tbody>{rows}</tbody></table>",
            if activity.is_run { "Allure" } else { "Puissance" }
        )
    } else {
        "<p class=\"hint\">Aucun lap manuel enregistré.</p>".to_string()
    };

    let show_side_panel = laps.len() > 1 || !segment_efforts.is_empty();
    let laps_html = if show_side_panel {
        format!(
            "<div class=\"laps-panel\">\
             <div class=\"laps-panel-header\">\
               <div class=\"panel-tabs\">\
                 <button type=\"button\" class=\"panel-tab active\" onclick=\"window.switchPanelTab('laps',this)\">Laps</button>\
                 <button type=\"button\" class=\"panel-tab\" onclick=\"window.switchPanelTab('segments',this)\">Segments</button>\
               </div>\
               <button type=\"button\" class=\"laps-reset\" onclick=\"window.resetZoom()\">Voir tout</button>\
             </div>\
             <div id=\"panel-laps\">{laps_rows_html}</div>\
             <div id=\"panel-segments\" style=\"display:none\">{segments_rows_html}</div>\
             </div>\
             <script>\
             window.switchPanelTab = function(name, btn) {{\
               document.getElementById('panel-laps').style.display = name === 'laps' ? '' : 'none';\
               document.getElementById('panel-segments').style.display = name === 'segments' ? '' : 'none';\
               btn.parentElement.querySelectorAll('.panel-tab').forEach(b => b.classList.remove('active'));\
               btn.classList.add('active');\
             }};\
             const SEGMENTS_DATA = {segments_data_json};\
             window.toggleSegmentOverlay = function(checkbox, kind, idx) {{\
               document.querySelectorAll('.segment-overlay-cb').forEach(cb => {{ if (cb !== checkbox) cb.checked = false; }});\
               const chart = window.comboChart;\
               if (!chart) return;\
               chart.data.datasets = chart.data.datasets.filter(ds => !ds.overlay);\
               if (checkbox.checked) {{\
                 const seg = SEGMENTS_DATA[idx];\
                 const curve = seg[kind];\
                 const label = kind === 'pr' ? 'RP' : 'Dernier';\
                 if (curve && curve.power) {{\
                   chart.data.datasets.push({{\
                     label: label + ' — Puissance (W)', overlay: true,\
                     data: curve.distance_km.map((d, i) => ({{x: seg.start_km + d, y: curve.power[i]}})),\
                     borderColor: '{power_color}', borderDash: [5, 4], borderWidth: 1.5, pointRadius: 0,\
                     yAxisID: 'y_power', tension: 0.2,\
                   }});\
                   if (chart.options.scales.y_power) chart.options.scales.y_power.display = true;\
                 }}\
                 if (curve && curve.hr) {{\
                   chart.data.datasets.push({{\
                     label: label + ' — FC (bpm)', overlay: true,\
                     data: curve.distance_km.map((d, i) => ({{x: seg.start_km + d, y: curve.hr[i]}})),\
                     borderColor: '{hr_color}', borderDash: [5, 4], borderWidth: 1.5, pointRadius: 0,\
                     yAxisID: 'y_hr', tension: 0.2,\
                   }});\
                   if (chart.options.scales.y_hr) chart.options.scales.y_hr.display = true;\
                 }}\
               }} else if (window.applyChartSelection) {{\
                 window.applyChartSelection();\
               }}\
               chart.update();\
             }};\
             </script>",
            power_color = SERIES_COLOR_POWER,
            hr_color = SERIES_COLOR_HR,
        )
    } else {
        String::new()
    };

    let bilan_html = activity
        .bilan
        .as_deref()
        .map(|t| format!("<p class=\"bilan\">{}</p>", annotate_acronyms(&escape_html(t))))
        .unwrap_or_default();

    let has_api_key = crate::settings::fetch(&state.pool).await.map(|s| s.has_llm_key()).unwrap_or(false);
    let llm_notice_html = llm_notice(has_api_key, q.warn.as_deref());

    let context_form_html = format!(
        "<form method=\"post\" action=\"/activity/{id}/context\" class=\"context-form\">\
         <label for=\"user_context\">Contexte additionnel pour cette sortie</label>\
         <textarea id=\"user_context\" name=\"user_context\" rows=\"2\" \
         placeholder=\"ex: sortie avec un rhume, vélo neuf pas encore réglé, sortie groupe imposant le rythme...\">{}</textarea>\
         <div class=\"context-actions\">\
         <button type=\"submit\">{button_label}</button>\
         <button type=\"submit\" formaction=\"/activity/{id}/report\" class=\"secondary-action\">Télécharger le rapport IA</button>\
         </div>\
         </form>",
        escape_html(activity.user_context.as_deref().unwrap_or("")),
        button_label = if activity.bilan.is_some() { "Régénérer le bilan avec ce contexte" } else { "Générer le bilan" },
    );

    // Editable in the UI (not just synced from Strava) since a GPX/FIT
    // import never carries a description — see importer::ParsedActivity.
    let description_form_html = format!(
        "<form method=\"post\" action=\"/activity/{id}/description\" class=\"context-form\">\
         <label for=\"description\">Description</label>\
         <textarea id=\"description\" name=\"description\" rows=\"2\" \
         placeholder=\"description de la sortie...\">{}</textarea>\
         <button type=\"submit\">Enregistrer la description</button>\
         </form>",
        escape_html(activity.description.as_deref().unwrap_or("")),
    );

    // Once an analysis exists the edit fields are tucked away behind a link
    // (the LLM notice stays visible, it may carry an error).
    let edit_html = if activity.bilan.is_some() {
        format!(
            "{llm_notice_html}<details class=\"edit-details\"><summary>✏️ Modifier la description et le contexte</summary>{description_form_html}{context_form_html}</details>"
        )
    } else {
        format!("{llm_notice_html}{description_form_html}{context_form_html}")
    };

    // Imported (GPX/FIT) activities use negative ids (see web::import) —
    // there's no matching Strava activity to link to for those.
    let strava_link_html = if id > 0 {
        format!(r#"<p><a href="https://www.strava.com/activities/{id}" target="_blank" rel="noopener">voir sur Strava ↗</a></p>"#)
    } else {
        String::new()
    };

    let stat_row = |label: &str, value: String| format!("<div class=\"stat\"><div class=\"value\">{value}</div><div class=\"label\">{label}</div></div>");
    let stats_html = [
        stat_row("Distance", format!("{:.1} km", activity.distance_m / 1000.0)),
        stat_row("Durée", format_duration_hms(activity.duration_s)),
        stat_row("D+", format!("{:.0} m", activity.elevation_gain_m)),
        activity
            .tss
            .map(|t| stat_row(&annotate_acronyms(if activity.is_run { "hrTSS" } else { "TSS" }), format!("{t:.0}")))
            .unwrap_or_default(),
        activity.intensity_factor.map(|v| stat_row(&annotate_acronyms("IF"), format!("{v:.2}"))).unwrap_or_default(),
        if activity.is_run {
            super::format::format_pace(activity.distance_m, activity.duration_s)
                .map(|p| stat_row("Allure moy.", format!("{p} /km")))
                .unwrap_or_default()
        } else {
            String::new()
        },
        activity.avg_power_w.map(|v| stat_row("Puiss. moy.", format!("{v:.0}W"))).unwrap_or_default(),
        activity.normalized_power_w.map(|v| stat_row(&annotate_acronyms("NP"), format!("{v:.0}W"))).unwrap_or_default(),
        activity.avg_hr.map(|v| stat_row("FC moy.", format!("{v:.0}bpm"))).unwrap_or_default(),
        activity.max_hr.map(|v| stat_row("FC max", format!("{v:.0}bpm"))).unwrap_or_default(),
    ]
    .concat();

    // No power zones for runs (or any activity without power data).
    let power_zone_section = if activity.zone_secs.iter().sum::<i64>() > 0 {
        format!(
            "<section>
<h2>Répartition zones de puissance</h2>
{}
</section>",
            render_zone_bar(&activity.zone_secs, ftp_watts as f64)
        )
    } else {
        String::new()
    };

    let body = format!(
        r#"<a class="back-link" href="/">&larr; Tableau de bord</a>
<div class="page-header"><div><h1>{date}</h1></div></div>

<section>
<div class="stats">{stats_html}</div>
{bilan_html}
{edit_html}
{strava_link_html}
</section>

<section>
<h2>Courbes</h2>
<div class="chart-with-laps">
<div class="chart-block combo-block">{combo_chart_html}</div>
{laps_html}
</div>
</section>

<div class="detail-grid">
<div class="detail-col">

<section>
<h2>Trajet</h2>
{map_html}
</section>

</div>
<div class="detail-col">
<div class="detail-col-side">
<div>

{power_zone_section}

{hr_zone_section}

</div>
<div>

{power_curve_section}

</div>
</div>
</div>
</div>

<script>
(function() {{
  const LAP_ZONES = {lap_zones_json};
  const POWER_TOTALS = {power_zone_secs_total_json};
  const HR_TOTALS = {hr_zone_secs_total_json};
  function applyFade(kind, totals, lapZones) {{
    for (let i = 0; i < totals.length; i++) {{
      const el = document.querySelector('[data-zone-id="' + kind + '_' + (i + 1) + '"] .zone-row-bar-dim');
      if (!el) continue;
      const lapSecs = lapZones ? (lapZones[i] || 0) : 0;
      const total = totals[i] || 0;
      const fraction = total > 0 ? Math.min(1, lapSecs / total) : 0;
      el.style.width = lapZones ? ((1 - fraction) * 100) + '%' : '0%';
    }}
  }}
  window.highlightLapInZones = function(lapIndex) {{
    const lz = LAP_ZONES[lapIndex - 1];
    if (!lz) return;
    applyFade('power', POWER_TOTALS, lz.power);
    applyFade('hr', HR_TOTALS, lz.hr);
  }};
  window.resetZoneHighlight = function() {{
    applyFade('power', POWER_TOTALS, null);
    applyFade('hr', HR_TOTALS, null);
  }};
}})();
</script>

"#,
        date = escape_html(&format_datetime_fr(&activity.started_at)),
        combo_chart_html = combo_chart_html,
        laps_html = laps_html,
        lap_zones_json = lap_zones_json,
        power_zone_secs_total_json = power_zone_secs_total_json,
        hr_zone_secs_total_json = hr_zone_secs_total_json,
        hr_zone_section = hr_zone_html
            .map(|h| format!("<section><h2>Répartition zones cardiaques</h2>{h}</section>"))
            .unwrap_or_default(),
        power_curve_section = power_curve_chart
            .map(|chart| format!(
                "<section><h2>Courbe de puissance</h2><div class=\"chart-block\">{chart}</div><div class=\"pc-row\">{}</div></section>",
                power_curve_cells.unwrap_or_default()
            ))
            .unwrap_or_default(),
    );

    Html(
        super::layout::Page::new(&format_datetime_fr(&activity.started_at), super::layout::Nav::Home, &body)
            .class("wide detail-page")
            .head(DETAIL_HEAD)
            .css(DETAIL_CSS)
            .render(),
    )
}

#[derive(Deserialize)]
pub struct DescriptionForm {
    #[serde(default)]
    description: String,
}

/// GPX/FIT imports never carry a description (see `importer::ParsedActivity`
/// doc comment) — this is how they get one, and it also lets a
/// Strava-synced description be corrected by hand.
pub async fn update_description(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<DescriptionForm>,
) -> Redirect {
    let description = form.description.trim();
    let description = if description.is_empty() { None } else { Some(description) };

    let _ = sqlx::query("UPDATE activities SET description = ? WHERE id = ?")
        .bind(description)
        .bind(id)
        .execute(&state.pool)
        .await;

    Redirect::to(&format!("/activity/{id}"))
}

#[derive(Deserialize)]
pub struct ContextForm {
    #[serde(default)]
    user_context: String,
}

/// Saves the user-supplied context for this activity, then (re)generates its
/// bilan (via `llm::generate_activity_bilans`, which injects that context
/// into the prompt) — a single LLM call, not a full re-sync. Since sync no
/// longer auto-generates bilans, this is also the *first* generation for a
/// freshly-synced activity, not just a regeneration. Without an API key the
/// context is still saved but no call is made; the page shows a warning
/// pointing at the downloadable report instead.
pub async fn regenerate_bilan(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<ContextForm>,
) -> Redirect {
    save_user_context(&state.pool, id, &form.user_context).await;

    let Ok(settings) = crate::settings::fetch(&state.pool).await else {
        tracing::error!(activity_id = id, "failed to load settings for bilan regeneration");
        return Redirect::to(&format!("/activity/{id}"));
    };
    if !settings.has_llm_key() {
        tracing::warn!(activity_id = id, "bilan requested without an Anthropic API key, skipping LLM call");
        return Redirect::to(&format!("/activity/{id}?warn=no_api_key"));
    }
    let client = crate::llm::AnthropicClient::new(settings.anthropic_api_key.clone());
    if let Err(err) = crate::llm::generate_activity_bilans(
        &state.pool,
        &client,
        &[id],
        &settings.athlete_profile,
        &settings.bilan_system_prompt,
    )
    .await
    {
        tracing::error!(?err, activity_id = id, "bilan regeneration failed");
        return Redirect::to(&format!("/activity/{id}?warn=llm_failed"));
    }

    Redirect::to(&format!("/activity/{id}"))
}

/// No-API alternative to `regenerate_bilan`: saves the context the same way,
/// then returns the prompt + data as a markdown file to paste into any chat
/// assistant.
pub async fn activity_report(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<ContextForm>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    save_user_context(&state.pool, id, &form.user_context).await;

    let Ok(settings) = crate::settings::fetch(&state.pool).await else {
        tracing::error!(activity_id = id, "failed to load settings for activity report");
        return Redirect::to(&format!("/activity/{id}")).into_response();
    };
    match crate::llm::build_activity_message(&state.pool, id, &settings.athlete_profile).await {
        Ok(user_message) => {
            let body = crate::report::render(
                &format!("Sortie du {}", format_datetime_fr(&started_at_of(&state.pool, id).await)),
                &settings.bilan_system_prompt,
                &user_message,
                crate::report::BILAN_ANSWER_FORMAT,
            );
            markdown_download(&format!("cyclai-activite-{}.md", id.abs()), body)
        }
        Err(err) => {
            tracing::error!(?err, activity_id = id, "activity report generation failed");
            Redirect::to(&format!("/activity/{id}")).into_response()
        }
    }
}

async fn started_at_of(pool: &SqlitePool, id: i64) -> String {
    sqlx::query_scalar::<_, String>("SELECT started_at FROM activities WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

async fn save_user_context(pool: &SqlitePool, id: i64, raw: &str) {
    let context = raw.trim();
    let context = if context.is_empty() { None } else { Some(context) };
    let _ = sqlx::query("UPDATE activities SET user_context = ? WHERE id = ?")
        .bind(context)
        .bind(id)
        .execute(pool)
        .await;
}


const DETAIL_HEAD: &str = r#"<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.4/dist/chart.umd.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/chartjs-plugin-annotation@3.0.1/dist/chartjs-plugin-annotation.min.js"></script>"#;

const DETAIL_CSS: &str = r##".chart-block { margin-bottom: 1.2rem; }
.chart-block h3 { font-size: 0.85rem; color: var(--text-muted); margin-bottom: 0.3rem; }
.pc-row { display: flex; flex-wrap: wrap; gap: 0.75rem; }
.pc-cell { text-align: center; min-width: 56px; }
.pc-value { font-weight: 600; font-size: 0.95rem; }
.pc-label { font-size: 0.7rem; color: var(--text-muted); }
.combo-wrap { height: 380px; width: 100%; }
.detail-map { height: 380px; }
.chart-selectors { display: flex; flex-wrap: wrap; gap: 1.2rem; margin-bottom: 0.75rem; font-size: 0.85rem; color: var(--text-muted); }
.chart-selectors label { display: inline-flex; align-items: center; gap: 0.4rem; margin: 0; }
.chart-selectors select { border: 1px solid var(--border); border-radius: 6px; padding: 0.25rem 0.5rem; font-size: 0.85rem; font-family: inherit; }
.chart-selectors .dot { width: 9px; height: 9px; border-radius: 50%; display: inline-block; }
.combo-block { flex: 1; min-width: 0; margin-bottom: 0; }
.chart-with-laps { display: flex; flex-direction: column; gap: 1rem; }
.laps-panel { flex-shrink: 0; }
.laps-panel-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 0.4rem; gap: 0.5rem; }
.laps-panel-header h3 { margin: 0; font-size: 0.85rem; color: var(--text-muted); }
.panel-tabs { display: flex; gap: 0.3rem; }
.panel-tab { background: none; border: 1px solid var(--border); border-radius: 6px; padding: 0.25rem 0.6rem; font-size: 0.75rem; color: var(--text-muted); cursor: pointer; margin: 0; }
.panel-tab.active { background: var(--accent-soft); color: var(--accent); border-color: var(--accent); font-weight: 600; }
.laps-reset { background: none; border: 1px solid var(--border); border-radius: 6px; padding: 0.25rem 0.6rem; font-size: 0.75rem; color: var(--accent); cursor: pointer; margin: 0; }
.laps-reset:hover { background: var(--accent-soft); }
.laps-panel .laps-table { max-height: 260px; display: block; overflow-y: auto; }
.laps-panel .laps-table thead, .laps-panel .laps-table tbody { display: table; width: 100%; table-layout: fixed; }
.lap-row { cursor: pointer; }
.lap-row:hover { background: var(--accent-soft); }
.lap-row-active { background: var(--accent-soft); font-weight: 600; }
.segments-table th:nth-child(3), .segments-table th:nth-child(4),
.segments-table td:nth-child(3), .segments-table td:nth-child(4) { width: 44px; text-align: center; }
.segment-overlay-cb { cursor: pointer; }
.segment-overlay-cb:disabled { opacity: 0.25; cursor: default; }
.edit-details { margin: 0.6rem 0; }
.edit-details > summary { cursor: pointer; color: var(--accent); font-size: 0.85rem; }
.edit-details[open] > summary { margin-bottom: 0.4rem; }
.context-form { display: flex; flex-direction: column; gap: 0.4rem; margin: 0.6rem 0; }
.context-form label { font-size: 0.75rem; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.03em; }
.context-form textarea { font-family: inherit; font-size: 0.88rem; padding: 0.5rem 0.6rem; border: 1px solid var(--border); border-radius: 6px; resize: vertical; }
.context-form button { align-self: flex-start; background: var(--accent-soft); color: var(--accent); border: 1px solid var(--accent); border-radius: 6px; padding: 0.35rem 0.8rem; font-size: 0.82rem; cursor: pointer; }
.context-form button:hover { background: var(--accent); color: var(--on-accent); }
.context-actions { display: flex; flex-wrap: wrap; gap: 0.5rem; }
.context-form button.secondary-action { background: transparent; }
.context-form button.secondary-action:hover { background: var(--accent); }
.detail-grid { display: block; }
.detail-col { min-width: 0; }
.detail-col-side { display: block; min-width: 0; }
@media (min-width: 900px) {
  .container.detail-page { max-width: 1180px; }
  .detail-grid { display: grid; grid-template-columns: minmax(320px, 420px) 1fr; gap: 1.5rem; align-items: start; }
  .chart-with-laps { flex-direction: row; align-items: stretch; }
  .laps-panel { width: 320px; }
  .laps-panel .laps-table { max-height: 380px; }
  .detail-map { height: 460px; }
}
@media (min-width: 1400px) {
  .container.detail-page { max-width: min(94vw, 1700px); }
  .detail-grid { grid-template-columns: minmax(380px, 520px) 1fr; }
  .combo-wrap { height: 440px; }
  .laps-panel { width: 360px; }
  .laps-panel .laps-table { max-height: 440px; }
}
@media (min-width: 1700px) {
  .container.detail-page { max-width: min(92vw, 2200px); }
  .detail-grid { grid-template-columns: minmax(420px, 620px) 1fr; }
  .detail-col-side { display: grid; grid-template-columns: 1fr 1fr; gap: 1.5rem; align-items: start; }
  .detail-map { height: 560px; }
  .combo-wrap { height: 500px; }
}
"##;
