mod detail;
mod format;
mod workout;

use axum::extract::{Form, State};
use axum::response::{Html, Redirect};
use axum::routing::{get, post};
use axum::Router;
use chrono::{Datelike, NaiveDate};
use format::{format_datetime_fr, format_duration_hms};
use regex::Regex;
use serde::Deserialize;
use sqlx::SqlitePool;
use std::sync::OnceLock;

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
    config: crate::config::Config,
}

pub async fn serve(pool: SqlitePool, config: crate::config::Config) -> anyhow::Result<()> {
    let state = AppState { pool, config };
    let app = Router::new()
        .route("/", get(dashboard))
        .route("/activity/{id}", get(detail::activity_detail))
        .route("/activity/{id}/context", post(detail::regenerate_bilan))
        .route("/workouts", get(workout::list).post(workout::create))
        .route("/workouts/{id}", get(workout::edit_form).post(workout::update))
        .route("/workouts/{id}/delete", post(workout::delete))
        .route("/config/ftp", post(update_ftp))
        .route("/digest", post(trigger_digest))
        .route("/config/gym", post(update_gym_config))
        .route("/sync", post(trigger_sync))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    tracing::info!("dashboard listening on http://0.0.0.0:3000");
    axum::serve(listener, app).await?;
    Ok(())
}

struct AthleteConfig {
    ftp_watts: i64,
    hr_max: i64,
    hr_rest: i64,
}

struct LoadSummary {
    tss_period: f64,
    ctl: f64,
    atl: f64,
    tsb: f64,
    zone_secs: [i64; 7],
}

struct ActivityRow {
    id: i64,
    started_at: String,
    distance_m: f64,
    duration_s: i64,
    elevation_gain_m: f64,
    tss: Option<f64>,
    intensity_factor: Option<f64>,
    bilan: Option<String>,
    route_svg: Option<String>,
}

struct DigestRow {
    date: String,
    free_text: Option<String>,
    llm_response_json: String,
    llm_response_text: String,
}

async fn fetch_athlete_config(pool: &SqlitePool) -> anyhow::Result<AthleteConfig> {
    let (ftp_watts, hr_max, hr_rest): (i64, i64, i64) =
        sqlx::query_as("SELECT ftp_watts, hr_max, hr_rest FROM athlete_config WHERE id = 1")
            .fetch_one(pool)
            .await?;
    Ok(AthleteConfig {
        ftp_watts,
        hr_max,
        hr_rest,
    })
}

/// Window is 4 weeks, not 1 — a weekend-only rider can have zero activity
/// on 5 of 7 days, so a 7-day window is too sparse to be a useful summary.
const LOAD_WINDOW_DAYS: &str = "-27 days";

async fn fetch_load_summary(pool: &SqlitePool) -> anyhow::Result<LoadSummary> {
    let latest: Option<(f64, f64, f64)> =
        sqlx::query_as("SELECT ctl, atl, tsb FROM daily_load ORDER BY date DESC LIMIT 1")
            .fetch_optional(pool)
            .await?;
    let (ctl, atl, tsb) = latest.unwrap_or((0.0, 0.0, 0.0));

    let (tss_period,): (f64,) = sqlx::query_as(&format!(
        "SELECT COALESCE(SUM(tss), 0.0) FROM daily_load WHERE date >= date('now', '{LOAD_WINDOW_DAYS}')"
    ))
    .fetch_one(pool)
    .await?;

    let zones: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(&format!(
        r#"
        SELECT COALESCE(SUM(zone1_s),0), COALESCE(SUM(zone2_s),0), COALESCE(SUM(zone3_s),0),
               COALESCE(SUM(zone4_s),0), COALESCE(SUM(zone5_s),0), COALESCE(SUM(zone6_s),0), COALESCE(SUM(zone7_s),0)
        FROM activities WHERE date(started_at) >= date('now', '{LOAD_WINDOW_DAYS}')
        "#
    ))
    .fetch_one(pool)
    .await?;

    Ok(LoadSummary {
        tss_period,
        ctl,
        atl,
        tsb,
        zone_secs: [zones.0, zones.1, zones.2, zones.3, zones.4, zones.5, zones.6],
    })
}

#[allow(clippy::type_complexity)]
async fn fetch_recent_activities(pool: &SqlitePool) -> anyhow::Result<Vec<ActivityRow>> {
    let rows: Vec<(i64, String, f64, i64, f64, Option<f64>, Option<f64>, Option<String>, Option<String>)> =
        sqlx::query_as(
            r#"
        SELECT a.id, a.started_at, a.distance_m, a.duration_s, a.elevation_gain_m,
               a.tss, a.intensity_factor, b.text, s.streams_json
        FROM activities a
        LEFT JOIN activity_bilans b ON b.activity_id = a.id
        LEFT JOIN activity_streams s ON s.activity_id = a.id
        ORDER BY a.started_at DESC
        LIMIT 10
        "#,
        )
        .fetch_all(pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(
            |(id, started_at, distance_m, duration_s, elevation_gain_m, tss, intensity_factor, bilan, streams_json)| {
                let route_svg = streams_json
                    .as_deref()
                    .and_then(extract_latlng)
                    .and_then(|pts| route_svg(&pts));
                ActivityRow {
                    id,
                    started_at,
                    distance_m,
                    duration_s,
                    elevation_gain_m,
                    tss,
                    intensity_factor,
                    bilan,
                    route_svg,
                }
            },
        )
        .collect())
}

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

const ROUTE_THUMB_W: f64 = 96.0;
const ROUTE_THUMB_H: f64 = 64.0;

/// Renders the GPS track as a small inline SVG polyline — a route-shape
/// thumbnail (like Strava's own small previews), not a real map: no tile
/// server dependency, works fully offline behind the VPN.
fn route_svg(latlng: &[[f64; 2]]) -> Option<String> {
    if latlng.len() < 2 {
        return None;
    }
    let lat_min = latlng.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let lat_max = latlng.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
    let lng_min = latlng.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let lng_max = latlng.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);

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

    // Downsample dense streams (a 2h ride at 1Hz is ~7000 points) — a route
    // shape doesn't need per-second resolution to look right.
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

struct CalendarActivity {
    id: i64,
    date: String,
    distance_km: f64,
    tss: Option<f64>,
    intensity_factor: Option<f64>,
}

/// "mois transitif": roughly the last 3 weeks plus the next 2 (~5 weeks).
/// The grid is week-aligned (Monday-start) rather than a strict -21/+14 day
/// cut, so FullCalendar's stock dayGrid can wrap rows at 7 columns like it
/// would for a real month — a partial-week cut would need a hand-rolled
/// grid, which is exactly what we're avoiding here.
const CALENDAR_WEEKS_BACK: i64 = 3;
const CALENDAR_WEEKS_SPAN: i64 = 5;

/// Monday on/before `today - weeks_back` weeks — the first visible cell.
fn calendar_window_start(today: NaiveDate) -> NaiveDate {
    let target = today - chrono::Duration::weeks(CALENDAR_WEEKS_BACK);
    target - chrono::Duration::days(target.weekday().num_days_from_monday() as i64)
}

async fn fetch_calendar_activities(pool: &SqlitePool) -> anyhow::Result<Vec<CalendarActivity>> {
    let start = calendar_window_start(chrono::Utc::now().date_naive());
    let end = start + chrono::Duration::weeks(CALENDAR_WEEKS_SPAN);

    let rows: Vec<(i64, String, f64, Option<f64>, Option<f64>)> = sqlx::query_as(
        r#"
        SELECT id, date(started_at) as day, distance_m, tss, intensity_factor
        FROM activities
        WHERE date(started_at) >= ?1 AND date(started_at) < ?2
        ORDER BY started_at
        "#,
    )
    .bind(start.to_string())
    .bind(end.to_string())
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, date, distance_m, tss, intensity_factor)| CalendarActivity {
            id,
            date,
            distance_km: distance_m / 1000.0,
            tss,
            intensity_factor,
        })
        .collect())
}

/// FullCalendar (CDN, single bundled script — same pattern as Chart.js/
/// Leaflet elsewhere in this app) rather than a hand-rolled month grid.
/// Shows completed rides now; structured/planned workouts are a follow-up.
fn render_calendar_html(activities: &[CalendarActivity]) -> String {
    let start = calendar_window_start(chrono::Utc::now().date_naive());

    let events: Vec<serde_json::Value> = activities
        .iter()
        .map(|a| {
            let title = match a.tss {
                Some(tss) => format!("{:.0} km · TSS {:.0}", a.distance_km, tss),
                None => format!("{:.0} km", a.distance_km),
            };
            let color = a.intensity_factor.map(zone_color_for_intensity).unwrap_or("#9ca3af");
            // Z4/Seuil (yellow) is too light for white text to stay readable.
            let text_color = if color == ZONE_COLORS[3] { "#1f2430" } else { "#fff" };
            serde_json::json!({
                "title": title,
                "start": a.date,
                "allDay": true,
                "url": format!("/activity/{}", a.id),
                "backgroundColor": color,
                "borderColor": color,
                "textColor": text_color,
            })
        })
        .collect();

    format!(
        r#"<script src="https://cdn.jsdelivr.net/npm/fullcalendar@6.1.15/index.global.min.js"></script>
<div id="calendar"></div>
<script>
document.addEventListener('DOMContentLoaded', function() {{
  var calendar = new FullCalendar.Calendar(document.getElementById('calendar'), {{
    headerToolbar: false,
    firstDay: 1,
    height: 'auto',
    initialDate: '{start}',
    initialView: 'slidingWindow',
    views: {{
      slidingWindow: {{
        type: 'dayGrid',
        duration: {{ weeks: {span} }},
      }},
    }},
    events: {events_json},
  }});
  calendar.render();
}});
</script>"#,
        start = start,
        span = CALENDAR_WEEKS_SPAN,
        events_json = serde_json::to_string(&events).unwrap_or_else(|_| "[]".to_string()),
    )
}

async fn fetch_latest_digest(pool: &SqlitePool) -> anyhow::Result<Option<DigestRow>> {
    let row: Option<(String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT date, free_text, llm_response_json, llm_response_text FROM digests ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(date, free_text, llm_response_json, llm_response_text)| DigestRow {
        date,
        free_text,
        llm_response_json,
        llm_response_text,
    }))
}

async fn fetch_gym_equipment(pool: &SqlitePool) -> anyhow::Result<Vec<String>> {
    let (equipment_json,): (String,) =
        sqlx::query_as("SELECT equipment_json FROM gym_config WHERE id = 1")
            .fetch_one(pool)
            .await?;
    Ok(serde_json::from_str(&equipment_json).unwrap_or_default())
}

pub(super) const BASE_CSS: &str = r#"
:root {
  --bg: #f5f6f8;
  --card-bg: #ffffff;
  --text: #1f2430;
  --text-muted: #6b7280;
  --border: #e5e7eb;
  --accent: #2563eb;
  --accent-soft: #eff6ff;
  --radius: 10px;
}
* { box-sizing: border-box; }
body {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Arial, sans-serif;
  background: var(--bg);
  color: var(--text);
  max-width: 760px;
  margin: 0 auto;
  padding: 2.5rem 1.25rem 4rem;
  line-height: 1.5;
}
h1 { font-size: 1.6rem; margin: 0 0 1.75rem; }
h2 { font-size: 1.05rem; margin: 0 0 1rem; }
h3 { font-size: 1rem; margin: 0.6rem 0; }
section {
  background: var(--card-bg);
  border: 1px solid var(--border);
  border-radius: var(--radius);
  padding: 1.25rem 1.4rem;
  margin-bottom: 1.5rem;
  box-shadow: 0 1px 2px rgba(16,24,40,0.04);
}
label { font-size: 0.85rem; color: var(--text-muted); display: block; margin-bottom: 0.4rem; }
input[type=number], input[type=text], textarea {
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 0.45rem 0.6rem;
  font-size: 0.9rem;
  font-family: inherit;
  width: 100%;
  max-width: 260px;
}
textarea { max-width: 100%; }
button {
  background: var(--accent);
  color: white;
  border: none;
  border-radius: 6px;
  padding: 0.5rem 1.1rem;
  font-size: 0.85rem;
  cursor: pointer;
  margin-top: 0.65rem;
}
button:hover { opacity: 0.9; }
.hint { color: var(--text-muted); font-size: 0.85rem; margin: 0.4rem 0 0; }
.zone-rows { display: grid; grid-template-columns: minmax(120px, auto) 1fr auto; row-gap: 0.35rem; column-gap: 0.6rem; align-items: center; font-size: 0.8rem; }
.zone-row { display: contents; }
.zone-row-label { color: var(--text-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.zone-row-bar-wrap { background: var(--bg); border-radius: 4px; height: 10px; overflow: hidden; }
.zone-row-bar { height: 100%; border-radius: 4px; min-width: 2px; position: relative; }
.zone-row-bar-dim { position: absolute; top: 0; right: 0; bottom: 0; width: 0; background: var(--card-bg); opacity: 0.6; transition: width 0.2s; }
.zone-row-stats { color: var(--text-muted); white-space: nowrap; text-align: right; min-width: 84px; }
.stats { display: flex; flex-wrap: wrap; gap: 1.5rem; margin-bottom: 1rem; }
.stats .stat .value { font-size: 1.3rem; font-weight: 600; }
.stats .stat .label { font-size: 0.7rem; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.03em; }
.activity-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 0.75rem; }
.activity-card { display: flex; gap: 1rem; padding: 0.7rem; border: 1px solid var(--border); border-radius: 8px; align-items: flex-start; }
.route-thumb { flex-shrink: 0; background: var(--accent-soft); border-radius: 6px; }
.route-thumb-empty { opacity: 0.5; }
.activity-body { flex: 1; min-width: 0; }
.activity-meta { font-size: 0.85rem; color: var(--text-muted); margin-bottom: 0.3rem; }
.activity-meta a { color: var(--accent); text-decoration: none; }
.activity-meta a:hover { text-decoration: underline; }
.activity-stats { flex-shrink: 0; display: flex; flex-direction: column; gap: 0.15rem; font-size: 0.8rem; color: var(--text-muted); text-align: right; min-width: 84px; }
.bilan { font-style: italic; color: #444; font-size: 0.88rem; margin: 0; }
.bilan.pending { color: #999; font-style: normal; }
.digest-date { font-size: 0.8rem; color: var(--text-muted); margin: 0.5rem 0 0.75rem; }
.workout-steps ul { list-style: none; padding-left: 1rem; margin: 0.3rem 0; border-left: 2px solid var(--border); }
.workout-steps li { padding: 0.15rem 0 0.15rem 0.6rem; font-size: 0.88rem; }
abbr[data-tip] {
  text-decoration: underline dotted; text-decoration-color: var(--text-muted);
  cursor: help; position: relative;
}
abbr[data-tip]:hover::after {
  content: attr(data-tip);
  position: absolute;
  left: 50%;
  bottom: calc(100% + 6px);
  transform: translateX(-50%);
  width: max-content;
  max-width: 260px;
  background: rgba(17, 24, 39, 0.92);
  color: #fff;
  font-weight: 400;
  font-style: normal;
  font-size: 0.78rem;
  line-height: 1.35;
  letter-spacing: normal;
  text-align: left;
  text-transform: none;
  padding: 0.4rem 0.6rem;
  border-radius: 6px;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25);
  z-index: 30;
  pointer-events: none;
  white-space: normal;
}
#calendar {
  --fc-border-color: var(--border);
  --fc-page-bg-color: var(--card-bg);
  --fc-neutral-bg-color: var(--bg);
  --fc-today-bg-color: var(--accent-soft);
  --fc-event-text-color: #fff;
  --fc-small-font-size: 0.75rem;
  font-family: inherit;
}
#calendar .fc-daygrid-day-number { color: var(--text); font-size: 0.8rem; }
#calendar .fc-col-header-cell-cushion { color: var(--text-muted); font-size: 0.75rem; text-transform: uppercase; }
#calendar .fc-event { border-radius: 4px; padding: 0 2px; cursor: pointer; }
.tsb-gauge-wrap { margin: 0.9rem 0 1.2rem; }
.tsb-gauge { position: relative; display: flex; height: 12px; border-radius: 6px; overflow: hidden; }
.tsb-gauge-seg:first-child { border-top-left-radius: 6px; border-bottom-left-radius: 6px; }
.tsb-gauge-seg:last-child { border-top-right-radius: 6px; border-bottom-right-radius: 6px; }
.tsb-gauge-cursor {
  position: absolute;
  top: -4px;
  width: 12px;
  height: 20px;
  margin-left: -6px;
  background: var(--text);
  border: 2px solid var(--card-bg);
  border-radius: 3px;
  box-shadow: 0 1px 3px rgba(0,0,0,0.3);
}
.tsb-status { font-weight: 600; font-size: 0.95rem; margin: 0.6rem 0 0; }
.tsb-estimate.hint { margin-top: 0.15rem; }
"#;

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

struct GlossaryTerm {
    acronym: &'static str,
    full_name: &'static str,
    definition: &'static str,
}

const GLOSSARY: &[GlossaryTerm] = &[
    GlossaryTerm { acronym: "TSS", full_name: "Training Stress Score", definition: "Charge d'une séance, combine intensité et durée." },
    GlossaryTerm { acronym: "IF", full_name: "Intensity Factor", definition: "Intensité relative de la séance (NP ÷ FTP)." },
    GlossaryTerm { acronym: "NP", full_name: "Normalized Power", definition: "Puissance normalisée, pondère les pics d'effort plus que la moyenne brute." },
    GlossaryTerm { acronym: "VI", full_name: "Variability Index", definition: "Régularité de l'effort (NP ÷ puissance moyenne) ; proche de 1 = effort stable." },
    GlossaryTerm { acronym: "CTL", full_name: "Chronic Training Load", definition: "Charge chronique ('forme'), moyenne mobile ~42 jours du TSS." },
    GlossaryTerm { acronym: "ATL", full_name: "Acute Training Load", definition: "Charge aiguë ('fatigue'), moyenne mobile ~7 jours du TSS." },
    GlossaryTerm { acronym: "TSB", full_name: "Training Stress Balance", definition: "Fraîcheur, CTL moins ATL. Négatif = fatigué, positif = reposé." },
    GlossaryTerm { acronym: "FTP", full_name: "Functional Threshold Power", definition: "Puissance seuil, tenable environ 1h." },
];

/// Wraps every standalone occurrence of a glossary acronym in `<abbr
/// title="...">` so hovering shows a definition — applied to already
/// html-escaped text in a single regex pass (a per-term loop would risk a
/// later term matching text inside an abbr title just inserted by an
/// earlier one, since some definitions mention other acronyms).
fn annotate_acronyms(escaped_text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        let alternation = GLOSSARY.iter().map(|t| t.acronym).collect::<Vec<_>>().join("|");
        Regex::new(&format!(r"\b({alternation})\b")).unwrap()
    });
    re.replace_all(escaped_text, |caps: &regex::Captures| {
        let matched = &caps[1];
        let term = GLOSSARY.iter().find(|t| t.acronym == matched).expect("regex only matches glossary acronyms");
        format!(
            "<abbr data-tip=\"{} — {}\">{matched}</abbr>",
            term.full_name,
            term.definition.replace('"', "'")
        )
    })
    .into_owned()
}

fn render_workout_steps(steps: &[serde_json::Value]) -> String {
    let mut out = String::from("<ul>");
    for step in steps {
        let step_type = step.get("type").and_then(|t| t.as_str()).unwrap_or("?");
        if step_type == "repeat" {
            let count = step.get("count").and_then(|c| c.as_u64()).unwrap_or(1);
            let inner: Vec<serde_json::Value> = step
                .get("steps")
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default();
            out += &format!("<li>Répéter x{count}{}</li>", render_workout_steps(&inner));
        } else {
            let duration = step
                .get("duration")
                .and_then(|d| d.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let target = step.get("target").map(|t| {
                let low = t.get("low").and_then(|l| l.as_f64()).unwrap_or(0.0);
                let high = t.get("high").and_then(|h| h.as_f64()).unwrap_or(low);
                if (low - high).abs() < 0.01 {
                    format!("{low:.0}W")
                } else {
                    format!("{low:.0}-{high:.0}W")
                }
            });
            let terrain = step.get("terrain").and_then(|t| t.as_str()).unwrap_or("plat");
            let note = step
                .get("note")
                .and_then(|n| n.as_str())
                .map(|n| format!(" — {}", escape_html(n)))
                .unwrap_or_default();
            out += &format!(
                "<li>{}: {}{} ({}){}</li>",
                escape_html(step_type),
                escape_html(duration),
                target.map(|t| format!(" @ {t}")).unwrap_or_default(),
                escape_html(terrain),
                note
            );
        }
    }
    out += "</ul>";
    out
}

fn render_digest_section(digest: Option<&DigestRow>) -> String {
    let latest = match digest {
        Some(d) => d,
        None => return "<p>Aucun digest généré pour l'instant.</p>".to_string(),
    };

    let parsed: serde_json::Value =
        serde_json::from_str(&latest.llm_response_json).unwrap_or(serde_json::Value::Null);
    let workout = parsed.get("workout");
    let workout_html = match workout {
        Some(w) => {
            let name = w.get("name").and_then(|n| n.as_str()).unwrap_or("Séance");
            let distance = w
                .get("estimated_distance_km")
                .and_then(|d| d.as_f64())
                .map(|d| format!(" (~{d:.1}km estimés)"))
                .unwrap_or_default();
            let steps = w
                .get("steps")
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default();
            format!(
                "<h3>{}{}</h3>{}",
                escape_html(name),
                distance,
                render_workout_steps(&steps)
            )
        }
        None => format!("<pre>{}</pre>", escape_html(&latest.llm_response_text)),
    };

    let gym = parsed
        .get("gym_suggestion")
        .and_then(|g| g.as_str())
        .filter(|g| !g.is_empty())
        .map(|g| format!("<p><strong>Renfort salle:</strong> {}</p>", escape_html(g)))
        .unwrap_or_default();

    format!(
        "<p class=\"digest-date\">{}{}</p>{}<p class=\"bilan\">{}</p>{}",
        escape_html(&latest.date),
        latest
            .free_text
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(|t| format!(" — note: {}", escape_html(t)))
            .unwrap_or_default(),
        workout_html,
        annotate_acronyms(&escape_html(&latest.llm_response_text)),
        gym
    )
}

// Matches the user's other training tools: gris/bleu/vert/jaune/orange/rouge/mauve.
const ZONE_COLORS: [&str; 7] = [
    "#9ca3af", "#3b82f6", "#22c55e", "#eab308", "#f97316", "#dc2626", "#a855f7",
];
const ZONE_NAMES: [&str; 7] = [
    "Récupération active", "Endurance", "Tempo", "Seuil", "VO2max", "Capacité anaérobie", "Neuromusculaire",
];
// Must stay in sync with metrics::power_zone's %FTP boundaries.
const ZONE_BOUNDS: [f64; 8] = [0.0, 0.55, 0.75, 0.90, 1.05, 1.20, 1.50, f64::INFINITY];

/// Maps an activity's IF (NP/FTP, same ratio `metrics::power_zone` buckets
/// watts by) to its power-zone color — reuses the zone palette so the
/// calendar's intensity coloring matches the zone bar rather than inventing
/// a second scale.
fn zone_color_for_intensity(intensity_factor: f64) -> &'static str {
    let zone = ZONE_BOUNDS
        .windows(2)
        .position(|w| intensity_factor < w[1])
        .unwrap_or(6);
    ZONE_COLORS[zone]
}

fn zone_range_watts(i: usize, ftp: f64) -> String {
    let low = ZONE_BOUNDS[i] * ftp;
    let high = ZONE_BOUNDS[i + 1];
    if high.is_finite() {
        format!("{low:.0}-{:.0}W", high * ftp)
    } else {
        format!(">{low:.0}W")
    }
}

fn render_zone_bar(zone_secs: &[i64; 7], ftp: f64) -> String {
    // Highest zone first so it renders at the top and Z1 ends up at the
    // bottom of the vertical stack.
    let rows: Vec<(String, String, &str, i64, String)> = (0..7)
        .rev()
        .map(|i| {
            let name = format!("Z{} {}", i + 1, ZONE_NAMES[i]);
            (name, zone_range_watts(i, ftp), ZONE_COLORS[i], zone_secs[i], format!("power_{}", i + 1))
        })
        .collect();
    render_zone_rows(&rows)
}

/// Horizontal bar-per-zone chart: each row shows the zone's own bar (0-100%
/// of that zone's own share of total time — not relative to the biggest
/// zone) plus the share as a percentage and a duration. The value range
/// (e.g. "114-155W") is a hover tooltip, not inline text — the visible
/// label is just the zone name. `.zone-rows` (not each `.zone-row`) is the
/// grid container so every row shares the same column widths and the bars
/// all start at the same x position. Each row's `data-zone-id` (e.g.
/// "power_4") lets the activity detail page's per-lap JS find and fade the
/// portion of a zone's bar that falls outside the currently selected lap —
/// unused on the dashboard's aggregate view, harmless there.
pub(super) fn render_zone_rows(rows: &[(String, String, &str, i64, String)]) -> String {
    let total: i64 = rows.iter().map(|(_, _, _, s, _)| *s).sum();
    let items: String = rows
        .iter()
        .map(|(name, range, color, secs, zone_id)| {
            let pct = if total > 0 { *secs as f64 / total as f64 * 100.0 } else { 0.0 };
            format!(
                "<div class=\"zone-row\" title=\"{}\" data-zone-id=\"{zone_id}\"><div class=\"zone-row-label\">{}</div>\
                 <div class=\"zone-row-bar-wrap\"><div class=\"zone-row-bar\" style=\"width:{pct:.1}%;background:{color}\">\
                 <div class=\"zone-row-bar-dim\"></div></div></div><div class=\"zone-row-stats\">{pct:.0}% · {}</div></div>",
                escape_html(range),
                escape_html(name),
                format_zone_duration(*secs)
            )
        })
        .collect();
    format!("<div class=\"zone-rows\">{items}</div>")
}

/// Chronometer-style H:MM:SS, e.g. 754 -> "0:12:34", 3780 -> "1:03:00".
pub(crate) fn format_zone_duration(secs: i64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h}:{m:02}:{s:02}")
}

/// TSB (form) interpretation bands from training-load literature (Coggan's
/// PMC guidelines): (upper bound, label, color). Ranges are contiguous and
/// read in order — a TSB belongs to the first band whose upper bound it's
/// under, last band catches everything above +25.
const TSB_ZONES: [(f64, &str, &str); 6] = [
    (-30.0, "Fatigue forte", "#dc2626"),
    (-10.0, "Fatigue modérée, entraînement productif", "#f97316"),
    (0.0, "Fatigue légère, entraînement productif", "#eab308"),
    (10.0, "Frais", "#22c55e"),
    (25.0, "Optimal", "#2563eb"),
    (f64::INFINITY, "Désentraînement", "#9ca3af"),
];

/// Visual edges for the gauge's 6 segments and for clamping the cursor.
/// Mirrors `TSB_ZONES`' boundaries but caps the two open-ended bands to a
/// fixed 10-point "runway" instead of reserving infinite width for them.
const TSB_DISPLAY_BOUNDS: [f64; 7] = [-40.0, -30.0, -10.0, 0.0, 10.0, 25.0, 35.0];

fn tsb_zone_index(tsb: f64) -> usize {
    TSB_ZONES.iter().position(|(upper, _, _)| tsb < *upper).unwrap_or(5)
}

/// Simulates forward day-by-day rest (TSS=0) using the same Banister EWMA
/// as `metrics::recompute_daily_load`, and returns the number of days until
/// CTL-ATL reaches `target`. Rest doesn't push TSB up forever — ATL decays
/// faster than CTL at first (raising TSB) but both eventually fade toward 0,
/// so a high target may never be reached; capped at 180 days, returning
/// `None` rather than a misleading huge number in that case.
fn days_of_rest_to_reach(mut ctl: f64, mut atl: f64, target: f64) -> Option<i64> {
    if ctl - atl >= target {
        return Some(0);
    }
    for day in 1..=180 {
        ctl += -ctl / 42.0;
        atl += -atl / 7.0;
        if ctl - atl >= target {
            return Some(day);
        }
    }
    None
}

fn render_tsb_gauge(ctl: f64, atl: f64, tsb: f64) -> String {
    let span = TSB_DISPLAY_BOUNDS[6] - TSB_DISPLAY_BOUNDS[0];
    let segments: String = (0..6)
        .map(|i| {
            let width_pct = (TSB_DISPLAY_BOUNDS[i + 1] - TSB_DISPLAY_BOUNDS[i]) / span * 100.0;
            format!(r#"<div class="tsb-gauge-seg" style="width:{width_pct:.3}%;background:{}"></div>"#, TSB_ZONES[i].2)
        })
        .collect();

    let cursor_pct = (tsb.clamp(TSB_DISPLAY_BOUNDS[0], TSB_DISPLAY_BOUNDS[6]) - TSB_DISPLAY_BOUNDS[0]) / span * 100.0;

    let zone_i = tsb_zone_index(tsb);
    let (next_upper, _, current_color) = TSB_ZONES[zone_i];
    let status_label = TSB_ZONES[zone_i].1;

    let estimate_html = if next_upper.is_finite() {
        match days_of_rest_to_reach(ctl, atl, next_upper) {
            Some(0) => String::new(),
            Some(days) => {
                let next_label = TSB_ZONES[zone_i + 1].1;
                let day_word = if days == 1 { "jour" } else { "jours" };
                format!(
                    r#"<p class="hint tsb-estimate">≈ {days} {day_word} de repos pour atteindre « {} »</p>"#,
                    escape_html(next_label)
                )
            }
            None => String::new(),
        }
    } else {
        String::new()
    };

    format!(
        r#"<div class="tsb-gauge-wrap">
<div class="tsb-gauge">{segments}<div class="tsb-gauge-cursor" style="left:{cursor_pct:.2}%"></div></div>
<p class="tsb-status" style="color:{current_color}">{}</p>
{estimate_html}
</div>"#,
        escape_html(status_label)
    )
}

fn render_activity_card(a: &ActivityRow) -> String {
    let bilan = a
        .bilan
        .as_deref()
        .map(|t| format!("<p class=\"bilan\">{}</p>", annotate_acronyms(&escape_html(t))))
        .unwrap_or_else(|| format!("<p class=\"bilan pending\">bilan non généré — <a href=\"/activity/{}\">ajouter du contexte et lancer l'analyse</a></p>", a.id));
    let route = a
        .route_svg
        .clone()
        .unwrap_or_else(|| format!("<div class=\"route-thumb route-thumb-empty\" style=\"width:{ROUTE_THUMB_W}px;height:{ROUTE_THUMB_H}px\"></div>"));

    let stats = format!(
        r#"<div class="activity-stats">
<div>📏 {:.1} km</div>
<div>⏱ {}</div>
<div>⛰ {:.0} m</div>
{}{}
</div>"#,
        a.distance_m / 1000.0,
        format_duration_hms(a.duration_s),
        a.elevation_gain_m,
        a.tss.map(|t| format!("<div>🔥 {} {t:.0}</div>", annotate_acronyms("TSS"))).unwrap_or_default(),
        a.intensity_factor.map(|v| format!("<div>🎯 {} {v:.2}</div>", annotate_acronyms("IF"))).unwrap_or_default(),
    );

    format!(
        r#"<li class="activity-card">
{route}
<div class="activity-body">
<div class="activity-meta"><a href="/activity/{}">{}</a> · <a href="https://www.strava.com/activities/{}" target="_blank" rel="noopener">voir sur Strava ↗</a></div>
{}
</div>
{stats}
</li>"#,
        a.id,
        format_datetime_fr(&a.started_at),
        a.id,
        bilan
    )
}

fn render_page(
    config: &AthleteConfig,
    load: &LoadSummary,
    activities: &[ActivityRow],
    calendar_activities: &[CalendarActivity],
    digest: Option<&DigestRow>,
    gym_equipment: &[String],
) -> String {
    let zone_bar_html = render_zone_bar(&load.zone_secs, config.ftp_watts as f64);
    let tsb_gauge_html = render_tsb_gauge(load.ctl, load.atl, load.tsb);
    let activity_items = activities.iter().map(render_activity_card).collect::<String>();
    let calendar_html = render_calendar_html(calendar_activities);

    format!(
        r#"<!doctype html>
<html lang="fr">
<head>
<meta charset="utf-8">
<title>CyclAI</title>
<style>{base_css}</style>
</head>
<body>
<h1>CyclAI</h1>
<p class="hint"><a href="/workouts">Séances &rarr;</a></p>
<form method="post" action="/sync">
  <button type="submit">Synchroniser Strava</button>
</form>

<section>
<h2>Config</h2>
<form method="post" action="/config/ftp">
  <label>{ftp_label} (W)
  <input type="number" name="ftp_watts" value="{ftp}" min="1">
  </label>
  <button type="submit">Mettre à jour</button>
</form>
<p class="hint">HR max {hr_max}bpm / repos {hr_rest}bpm</p>
</section>

<section>
<h2>4 dernières semaines</h2>
<div class="stats">
  <div class="stat"><div class="value">{tss_period:.0}</div><div class="label">{tss_label}</div></div>
  <div class="stat"><div class="value">{ctl:.1}</div><div class="label">{ctl_label}</div></div>
  <div class="stat"><div class="value">{atl:.1}</div><div class="label">{atl_label}</div></div>
  <div class="stat"><div class="value">{tsb:.1}</div><div class="label">{tsb_label}</div></div>
</div>
{tsb_gauge_html}
{zone_bar_html}
</section>

<section>
<h2>Calendrier</h2>
{calendar_html}
</section>

<section>
<h2>Activités récentes</h2>
<ul class="activity-list">{activity_items}</ul>
</section>

<section>
<h2>Digest hebdomadaire</h2>
<form method="post" action="/digest">
  <label>Note libre (optionnel)
  <input type="text" name="free_text" placeholder="ex: sortie longue prévue mardi">
  </label>
  <button type="submit">Générer mon digest</button>
</form>
<div class="workout-steps">
{digest_html}
</div>
</section>

<section>
<h2>Config salle</h2>
<form method="post" action="/config/gym">
  <label>Équipement disponible (un par ligne)
  <textarea name="equipment" rows="5">{gym_equipment}</textarea>
  </label>
  <button type="submit">Enregistrer</button>
</form>
</section>

</body>
</html>"#,
        ftp_label = annotate_acronyms("FTP"),
        ftp = config.ftp_watts,
        hr_max = config.hr_max,
        hr_rest = config.hr_rest,
        tss_period = load.tss_period,
        tss_label = annotate_acronyms("TSS"),
        ctl = load.ctl,
        ctl_label = annotate_acronyms("CTL"),
        atl = load.atl,
        atl_label = annotate_acronyms("ATL"),
        tsb = load.tsb,
        tsb_label = annotate_acronyms("TSB"),
        digest_html = render_digest_section(digest),
        gym_equipment = escape_html(&gym_equipment.join("\n")),
        base_css = BASE_CSS,
    )
}

async fn dashboard(State(state): State<AppState>) -> Html<String> {
    // Recomputed here (not just at sync) so CTL/ATL/TSB reflect rest days
    // that have passed since the last sync — otherwise TSB looks frozen at
    // whatever it was at last sync until the next activity comes in.
    if let Err(err) = crate::metrics::recompute_daily_load(&state.pool).await {
        tracing::error!(?err, "daily_load recompute on page load failed");
    }

    let config = fetch_athlete_config(&state.pool)
        .await
        .expect("athlete_config seeded at startup");
    let load = fetch_load_summary(&state.pool).await.expect("daily_load query");
    let activities = fetch_recent_activities(&state.pool).await.expect("activities query");
    let calendar_activities = fetch_calendar_activities(&state.pool).await.expect("calendar query");
    let digest = fetch_latest_digest(&state.pool).await.expect("digests query");
    let gym_equipment = fetch_gym_equipment(&state.pool).await.expect("gym_config query");
    Html(render_page(&config, &load, &activities, &calendar_activities, digest.as_ref(), &gym_equipment))
}

#[derive(Deserialize)]
struct FtpForm {
    ftp_watts: i64,
}

/// Updates the FTP, then immediately re-runs the deterministic metrics
/// pipeline so TSS/IF/zones retroactively reflect the new value across the
/// whole history — matches the sync-time behavior in metrics::run.
async fn update_ftp(State(state): State<AppState>, Form(form): Form<FtpForm>) -> Redirect {
    let _ = sqlx::query(
        "UPDATE athlete_config SET ftp_watts = ?, updated_at = datetime('now') WHERE id = 1",
    )
    .bind(form.ftp_watts)
    .execute(&state.pool)
    .await;

    let _ = crate::metrics::run(&state.pool).await;

    Redirect::to("/")
}

#[derive(Deserialize)]
struct DigestForm {
    #[serde(default)]
    free_text: String,
}

async fn trigger_digest(State(state): State<AppState>, Form(form): Form<DigestForm>) -> Redirect {
    let client = crate::llm::AnthropicClient::new(state.config.anthropic_api_key.clone());
    let free_text = (!form.free_text.trim().is_empty()).then_some(form.free_text.as_str());
    if let Err(err) =
        crate::digest::generate(&state.pool, &client, free_text, &state.config.athlete_profile).await
    {
        tracing::error!(?err, "digest generation failed");
    }
    Redirect::to("/")
}

/// Runs a synchronous fetch/import from the dashboard, mirroring the `sync`
/// CLI command — the only difference is it's triggered by a button instead
/// of the systemd timer. Blocks the request until Strava sync + metrics +
/// segment matching finish, same as the CLI invocation.
async fn trigger_sync(State(state): State<AppState>) -> Redirect {
    if let Err(err) = crate::sync::run(&state.pool, &state.config).await {
        tracing::error!(?err, "manual sync failed");
    }
    Redirect::to("/")
}

#[derive(Deserialize)]
struct GymConfigForm {
    #[serde(default)]
    equipment: String,
}

async fn update_gym_config(State(state): State<AppState>, Form(form): Form<GymConfigForm>) -> Redirect {
    let items: Vec<&str> = form
        .equipment
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let equipment_json = serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());

    let _ = sqlx::query("UPDATE gym_config SET equipment_json = ? WHERE id = 1")
        .bind(equipment_json)
        .execute(&state.pool)
        .await;

    Redirect::to("/")
}

#[cfg(test)]
mod tsb_gauge_tests {
    use super::*;

    #[test]
    fn zone_boundaries_match_literature_bands() {
        assert_eq!(tsb_zone_index(-31.0), 0); // Fatigue forte
        assert_eq!(tsb_zone_index(-30.0), 1); // Fatigue modérée (boundary is inclusive upward)
        assert_eq!(tsb_zone_index(-10.0), 2); // Fatigue légère
        assert_eq!(tsb_zone_index(-0.1), 2);
        assert_eq!(tsb_zone_index(0.0), 3); // Frais
        assert_eq!(tsb_zone_index(9.9), 3);
        assert_eq!(tsb_zone_index(10.0), 4); // Optimal
        assert_eq!(tsb_zone_index(24.9), 4);
        assert_eq!(tsb_zone_index(25.0), 5); // Désentraînement
        assert_eq!(tsb_zone_index(100.0), 5);
    }

    #[test]
    fn rest_simulation_reaches_target_and_reports_zero_when_already_there() {
        // Already past the target.
        assert_eq!(days_of_rest_to_reach(20.0, 5.0, 0.0), Some(0));
        // ATL (7-day) decays faster than CTL (42-day) under pure rest, so
        // TSB rises quickly at first — a couple of days to cross into Frais.
        let days = days_of_rest_to_reach(28.9, 37.4, 0.0).expect("reachable within cap");
        assert!((1..=5).contains(&days), "expected a small number of days, got {days}");
    }

    #[test]
    fn unreachable_target_returns_none_within_cap() {
        // With ctl=atl=0 (no history), TSB stays at 0 forever under rest —
        // it can never climb into the Optimal band.
        assert_eq!(days_of_rest_to_reach(0.0, 0.0, 10.0), None);
    }
}
