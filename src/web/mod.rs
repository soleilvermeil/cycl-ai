mod api;
mod auth;
mod detail;
pub(crate) mod format;
mod import;
mod layout;
mod planned;
mod route;
mod segments;
mod settings;
mod stats;
mod workout;
mod workout_chart;

use axum::Router;
use axum::extract::{Form, State};
use axum::response::{Html, Redirect};
use axum::routing::{get, post};
use chrono::{Datelike, NaiveDate};
use format::{format_datetime_fr, format_duration_hms};
use regex::Regex;
use serde::Deserialize;
use sqlx::SqlitePool;
use std::sync::OnceLock;

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
}

/// Web UI only (dashboard, settings, imports…) — meant for a private network.
/// The JSON API lives in `serve_api`.
pub async fn serve(pool: SqlitePool, bind: &str) -> anyhow::Result<()> {
    if let Ok(s) = crate::settings::fetch(&pool).await {
        layout::set_theme(&s.theme_color, &s.theme_mode);
    }
    let state = AppState { pool };
    let app = Router::new()
        .route("/", get(dashboard))
        .route("/favicon.ico", get(favicon))
        .route(
            "/manifest.webmanifest",
            get(|| {
                pwa_asset(
                    "application/manifest+json",
                    include_bytes!("../../assets/manifest.webmanifest"),
                )
            }),
        )
        .route(
            "/sw.js",
            get(|| pwa_asset("text/javascript", include_bytes!("../../assets/sw.js"))),
        )
        .route(
            "/icon-192.png",
            get(|| pwa_asset("image/png", include_bytes!("../../assets/icon-192.png"))),
        )
        .route(
            "/icon-512.png",
            get(|| pwa_asset("image/png", include_bytes!("../../assets/icon-512.png"))),
        )
        .route(
            "/apple-touch-icon.png",
            get(|| {
                pwa_asset(
                    "image/png",
                    include_bytes!("../../assets/apple-touch-icon.png"),
                )
            }),
        )
        .route("/activity/{id}", get(detail::activity_detail))
        .route("/activity/{id}/context", post(detail::regenerate_bilan))
        .route("/activity/{id}/report", post(detail::activity_report))
        .route(
            "/activity/{id}/description",
            post(detail::update_description),
        )
        .route("/workouts", get(workout::list).post(workout::create))
        .route(
            "/workouts/{id}",
            get(workout::edit_form).post(workout::update),
        )
        .route("/workouts/{id}/delete", post(workout::delete))
        .route("/workouts/{id}/zwo", get(workout::export_zwo))
        .route("/planned/new", get(planned::new_form))
        .route("/planned", post(planned::create))
        .route(
            "/planned/{id}",
            get(planned::edit_form).post(planned::update),
        )
        .route("/planned/{id}/delete", post(planned::delete))
        .route("/digest", post(trigger_digest))
        .route("/digest/report", post(digest_report))
        .route("/sync", post(trigger_sync))
        .route("/sync/full", post(trigger_full_sync))
        .route("/sync/status", get(sync_status))
        .route("/import", post(import::import_activity))
        .route("/route", get(route::page).post(route::analyze))
        .route("/stats", get(stats::page))
        .route("/segments", get(segments::page))
        .route("/segments/import", post(segments::import))
        .route("/settings", get(settings::page))
        .route("/settings/keys", post(settings::update_keys))
        .route("/settings/profile", post(settings::update_profile))
        .route(
            "/settings/prompts/bilan",
            post(settings::update_bilan_prompt),
        )
        .route(
            "/settings/prompts/digest",
            post(settings::update_digest_prompt),
        )
        .route("/settings/athlete", post(settings::update_athlete_config))
        .route("/settings/appearance", post(settings::update_appearance))
        .route("/settings/autodetect", post(settings::update_autodetect))
        .route(
            "/settings/autodetect/recompute",
            post(settings::recompute_autodetect),
        )
        .route("/settings/autodetect/clear", post(settings::clear_autodetect))
        .route("/settings/power", post(settings::update_power_estimate))
        .route(
            "/settings/power/recompute",
            post(settings::recompute_power_estimate),
        )
        .route("/settings/power/clear", post(settings::clear_power_estimate))
        // Planned-route GPX and FIT/GPX activity uploads can exceed axum's 2 MB default.
        .layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("dashboard listening on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn favicon() -> impl axum::response::IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/x-icon"),
            (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        include_bytes!("../../assets/favicon.ico").as_slice(),
    )
}

/// Static PWA files (manifest, service worker, icons). `no-cache` on all of
/// them so a changed service worker or manifest is picked up right away.
async fn pwa_asset(
    content_type: &'static str,
    body: &'static [u8],
) -> impl axum::response::IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
}

/// JSON API only, every request guarded by an RS256 bearer token checked
/// against the `api_keys` table. This is the process that can be exposed.
pub async fn serve_api(pool: SqlitePool, bind: &str) -> anyhow::Result<()> {
    let app = api_router(AppState { pool });
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("api listening on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn api_router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/profile", get(api::profile))
        .route("/api/v1/load", get(api::load))
        .route("/api/v1/summary", get(api::summary))
        .route("/api/v1/activities", get(api::activities))
        .route("/api/v1/activities/{id}", get(api::activity))
        .route(
            "/api/v1/activities/{id}/streams",
            get(api::activity_streams),
        )
        .route("/api/v1/workouts", get(api::workouts))
        .route(
            "/api/v1/planned",
            get(api::planned_list).post(api::planned_create),
        )
        .route(
            "/api/v1/planned/{id}",
            axum::routing::delete(api::planned_delete),
        )
        // `layer` (not `route_layer`): unknown paths answer 401 too, so the
        // exposed process reveals nothing about what it does or doesn't serve.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ))
        .with_state(state)
}

struct AthleteConfig {
    ftp_watts: i64,
    hr_rest: i64,
    hr_max: i64,
    run_threshold_pace_s: i64,
}

struct ActivityRow {
    id: i64,
    is_run: bool,
    started_at: String,
    distance_m: f64,
    duration_s: i64,
    elevation_gain_m: f64,
    tss: Option<f64>,
    intensity_factor: Option<f64>,
    bilan: Option<String>,
    route_svg: Option<String>,
}

impl AthleteConfig {
    fn athlete(&self) -> crate::workout::Athlete {
        crate::workout::Athlete {
            ftp_watts: self.ftp_watts as f64,
            hr_rest: self.hr_rest as f64,
            hr_max: self.hr_max as f64,
            run_threshold_pace_s: self.run_threshold_pace_s as f64,
        }
    }
}

struct DigestRow {
    date: String,
    free_text: Option<String>,
    llm_response_json: String,
    llm_response_text: String,
}

async fn fetch_athlete_config(pool: &SqlitePool) -> anyhow::Result<AthleteConfig> {
    let (ftp_watts, hr_rest, hr_max, run_threshold_pace_s): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT ftp_watts, hr_rest, hr_max, run_threshold_pace_s FROM athlete_config WHERE id = 1",
    )
    .fetch_one(pool)
    .await?;
    Ok(AthleteConfig {
        ftp_watts,
        hr_rest,
        hr_max,
        run_threshold_pace_s,
    })
}

/// Reads each activity's route thumbnail from `activities.route_thumb_svg`
/// (populated at sync/import time — see `sync::run`) instead of parsing the
/// full GPS stream on every dashboard load. Deliberately doesn't join
/// `activity_streams`, whose rows carry the full multi-hundred-KB streams.
#[allow(clippy::type_complexity)]
async fn fetch_recent_activities(pool: &SqlitePool) -> anyhow::Result<Vec<ActivityRow>> {
    let rows: Vec<(
        i64,
        String,
        String,
        f64,
        i64,
        f64,
        Option<f64>,
        Option<f64>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        r#"
        SELECT a.id, a.sport_type, a.started_at, a.distance_m, a.duration_s, a.elevation_gain_m,
               a.tss, a.intensity_factor, b.text, a.route_thumb_svg
        FROM activities a
        LEFT JOIN activity_bilans b ON b.activity_id = a.id
        ORDER BY a.started_at DESC
        LIMIT 10
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                sport_type,
                started_at,
                distance_m,
                duration_s,
                elevation_gain_m,
                tss,
                intensity_factor,
                bilan,
                route_thumb_svg,
            )| {
                let route_svg = route_thumb_svg.filter(|s| !s.is_empty());
                ActivityRow {
                    id,
                    is_run: crate::metrics::is_run(&sport_type),
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

struct CalendarActivity {
    id: i64,
    is_run: bool,
    date: String,
    distance_km: f64,
    tss: Option<f64>,
    intensity_factor: Option<f64>,
}

/// "mois transitif": the last 2 weeks plus the next 4 (6 weeks) — enough
/// look-ahead to plan sessions. The grid is week-aligned (Monday-start)
/// rather than a strict day cut, so FullCalendar's stock dayGrid can wrap
/// rows at 7 columns like it would for a real month — a partial-week cut
/// would need a hand-rolled grid, which is exactly what we're avoiding here.
const CALENDAR_WEEKS_BACK: i64 = 2;
const CALENDAR_WEEKS_SPAN: i64 = 6;

/// A planned session as a calendar event (see `crate::planned`).
struct CalendarPlanned {
    id: i64,
    date: String,
    name: String,
    sport: crate::planned::Sport,
    minutes: f64,
    /// Estimated IF (from the steps' average targets) — only used to pick the
    /// zone color, like completed rides.
    intensity: f64,
    /// An activity already exists on that day.
    done: bool,
}

async fn fetch_calendar_planned(
    pool: &SqlitePool,
    ath: &crate::workout::Athlete,
) -> anyhow::Result<Vec<CalendarPlanned>> {
    let start = calendar_window_start(chrono::Utc::now().date_naive());
    let end = start + chrono::Duration::weeks(CALENDAR_WEEKS_SPAN);
    let planned = crate::planned::list_between(pool, start, end).await?;

    let mut out = Vec::with_capacity(planned.len());
    for p in planned {
        let (minutes, tss) = serde_json::from_str::<crate::workout::Workout>(&p.steps_json)
            .map(|w| w.load_estimate(ath))
            .unwrap_or((0.0, 0.0));
        let intensity = if minutes > 0.0 {
            (tss / (minutes / 60.0) / 100.0).sqrt()
        } else {
            0.0
        };
        let (done,): (bool,) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM activities WHERE started_at >= ?1 AND started_at < date(?1, '+1 day'))",
        )
        .bind(&p.planned_date)
        .fetch_one(pool)
        .await?;
        out.push(CalendarPlanned {
            id: p.id,
            date: p.planned_date,
            name: p.name,
            sport: crate::planned::Sport::from_db(&p.sport),
            minutes,
            intensity,
            done,
        });
    }
    Ok(out)
}

/// Monday on/before `today - weeks_back` weeks — the first visible cell.
fn calendar_window_start(today: NaiveDate) -> NaiveDate {
    let target = today - chrono::Duration::weeks(CALENDAR_WEEKS_BACK);
    target - chrono::Duration::days(target.weekday().num_days_from_monday() as i64)
}

async fn fetch_calendar_activities(pool: &SqlitePool) -> anyhow::Result<Vec<CalendarActivity>> {
    let start = calendar_window_start(chrono::Utc::now().date_naive());
    let end = start + chrono::Duration::weeks(CALENDAR_WEEKS_SPAN);

    let rows: Vec<(i64, String, String, f64, Option<f64>, Option<f64>)> = sqlx::query_as(
        r#"
        SELECT id, sport_type, date(started_at) as day, distance_m, tss, intensity_factor
        FROM activities
        WHERE started_at >= ?1 AND started_at < ?2
        ORDER BY started_at
        "#,
    )
    .bind(start.to_string())
    .bind(end.to_string())
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(id, sport_type, date, distance_m, tss, intensity_factor)| CalendarActivity {
                id,
                is_run: crate::metrics::is_run(&sport_type),
                date,
                distance_km: distance_m / 1000.0,
                tss,
                intensity_factor,
            },
        )
        .collect())
}

/// FullCalendar (CDN, single bundled script — same pattern as Chart.js/
/// Leaflet elsewhere in this app) rather than a hand-rolled month grid.
/// Shows completed rides now; structured/planned workouts are a follow-up.
fn render_calendar_html(activities: &[CalendarActivity], planned: &[CalendarPlanned]) -> String {
    let start = calendar_window_start(chrono::Utc::now().date_naive());

    let planned_events = planned.iter().map(|p| {
        // Runs get the same teal as completed runs; everything else is colored
        // by its estimated intensity zone.
        let color = if p.sport == crate::planned::Sport::Running { RUN_COLOR } else { zone_color_for_intensity(p.intensity) };
        // Outlined + tinted rather than solid, so a plan reads as "not done
        // yet" next to the solid completed rides.
        serde_json::json!({
            "title": format!("{}{} {} · {}", if p.done { "✓ " } else { "" }, p.sport.icon(), p.name, format_duration_hms((p.minutes * 60.0).round() as i64)),
            // Compact chip in the cell (the full name goes in the tooltip).
            "extendedProps": {
                "l1": format!("{}{} {}", if p.done { "✓ " } else { "" }, p.sport.icon(), format_duration_hms((p.minutes * 60.0).round() as i64)),
                "l2": "",
            },
            "start": p.date,
            "allDay": true,
            "url": format!("/planned/{}", p.id),
            "backgroundColor": format!("{color}22"),
            "borderColor": color,
            "textColor": "var(--text)",
            "classNames": ["planned-event"],
        })
    });

    let events: Vec<serde_json::Value> = activities
        .iter()
        .map(|a| {
            let prefix = if a.is_run { "🏃 " } else { "" };
            let title = match a.tss {
                Some(tss) => format!("{prefix}{:.0} km · 🔥 {:.0}", a.distance_km, tss),
                None => format!("{prefix}{:.0} km", a.distance_km),
            };
            // Two lines so both values fit the narrow day cell.
            let line1 = format!("{prefix}{:.0} km", a.distance_km);
            let line2 = a.tss.map(|t| format!("🔥 {t:.0}")).unwrap_or_default();
            // Runs have no intensity factor (no power), so they get their own
            // teal instead of the "unknown" grey rides fall back to.
            let color = if a.is_run {
                RUN_COLOR
            } else {
                a.intensity_factor
                    .map(zone_color_for_intensity)
                    .unwrap_or("#9ca3af")
            };
            // Z4/Seuil (yellow) is too light for white text to stay readable.
            let text_color = if color == ZONE_COLORS[3] {
                "#1f2430"
            } else {
                "#fff"
            };
            serde_json::json!({
                "title": title,
                "extendedProps": { "l1": line1, "l2": line2 },
                "start": a.date,
                "allDay": true,
                "url": format!("/activity/{}", a.id),
                "backgroundColor": color,
                "borderColor": color,
                "textColor": text_color,
            })
        })
        .chain(planned_events)
        .collect();

    format!(
        r#"<script src="https://cdn.jsdelivr.net/npm/fullcalendar@6.1.15/index.global.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/@fullcalendar/core@6.1.15/locales/fr.global.min.js"></script>
<p class="hint calendar-hint">Clique sur un jour pour y planifier une séance.</p>
<div id="calendar"></div>
<script>
document.addEventListener('DOMContentLoaded', function() {{
  var calendar = new FullCalendar.Calendar(document.getElementById('calendar'), {{
    headerToolbar: false,
    locale: 'fr',
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
    eventContent: function(arg) {{
      var p = arg.event.extendedProps;
      return {{ html: '<div class="cal-ev"><div>' + p.l1 + '</div>' + (p.l2 ? '<div>' + p.l2 + '</div>' : '') + '</div>' }};
    }},
    eventDidMount: function(info) {{ info.el.title = info.event.title; }},
    dateClick: function(info) {{ window.location.href = '/planned/new?date=' + info.dateStr; }},
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
    Ok(row.map(
        |(date, free_text, llm_response_json, llm_response_text)| DigestRow {
            date,
            free_text,
            llm_response_json,
            llm_response_text,
        },
    ))
}

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
    GlossaryTerm {
        acronym: "TSS",
        full_name: "Training Stress Score",
        definition: "Charge d'une séance, combine intensité et durée.",
    },
    GlossaryTerm {
        acronym: "hrTSS",
        full_name: "Heart-rate TSS",
        definition: "TSS estimé à partir de la fréquence cardiaque (charge TRIMP rapportée à une heure à la FC seuil = 100). Utilisé pour la course à pied, qui s'additionne au TSS puissance du vélo dans CTL/ATL/TSB.",
    },
    GlossaryTerm {
        acronym: "IF",
        full_name: "Intensity Factor",
        definition: "Intensité relative de la séance (NP ÷ FTP).",
    },
    GlossaryTerm {
        acronym: "NP",
        full_name: "Normalized Power",
        definition: "Puissance normalisée, pondère les pics d'effort plus que la moyenne brute.",
    },
    GlossaryTerm {
        acronym: "VI",
        full_name: "Variability Index",
        definition: "Régularité de l'effort (NP ÷ puissance moyenne) ; proche de 1 = effort stable.",
    },
    GlossaryTerm {
        acronym: "CTL",
        full_name: "Chronic Training Load",
        definition: "Charge chronique ('forme'), moyenne mobile ~42 jours du TSS.",
    },
    GlossaryTerm {
        acronym: "ATL",
        full_name: "Acute Training Load",
        definition: "Charge aiguë ('fatigue'), moyenne mobile ~7 jours du TSS.",
    },
    GlossaryTerm {
        acronym: "TSB",
        full_name: "Training Stress Balance",
        definition: "Fraîcheur, CTL moins ATL. Négatif = fatigué, positif = reposé.",
    },
    GlossaryTerm {
        acronym: "FTP",
        full_name: "Functional Threshold Power",
        definition: "Puissance seuil, tenable environ 1h.",
    },
];

/// Wraps every standalone occurrence of a glossary acronym in `<abbr
/// title="...">` so hovering shows a definition — applied to already
/// html-escaped text in a single regex pass (a per-term loop would risk a
/// later term matching text inside an abbr title just inserted by an
/// earlier one, since some definitions mention other acronyms).
fn annotate_acronyms(escaped_text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        let alternation = GLOSSARY
            .iter()
            .map(|t| t.acronym)
            .collect::<Vec<_>>()
            .join("|");
        Regex::new(&format!(r"\b({alternation})\b")).unwrap()
    });
    re.replace_all(escaped_text, |caps: &regex::Captures| {
        let matched = &caps[1];
        let term = GLOSSARY
            .iter()
            .find(|t| t.acronym == matched)
            .expect("regex only matches glossary acronyms");
        format!(
            "<abbr data-tip=\"{} — {}\">{matched}</abbr>",
            term.full_name,
            term.definition.replace('"', "'")
        )
    })
    .into_owned()
}

/// "1 h 30", "45 min", "2 min 30", "40 s" — compact duration for step chips.
fn format_step_duration(minutes: f64) -> String {
    let total_s = (minutes * 60.0).round() as i64;
    if total_s <= 0 {
        return "?".to_string();
    }
    let (h, m, sec) = (total_s / 3600, (total_s % 3600) / 60, total_s % 60);
    match (h, m, sec) {
        (0, 0, s) => format!("{s} s"),
        (0, m, 0) => format!("{m} min"),
        (0, m, s) => format!("{m} min {s:02}"),
        (h, 0, 0) => format!("{h} h"),
        (h, m, _) => format!("{h} h {m:02}"),
    }
}

/// Target as short text ("195–215 W", "Z3 puissance", "4:30–5:00 /km").
fn format_step_target(t: &serde_json::Value) -> String {
    let kind = t.get("type").and_then(|k| k.as_str());
    if kind == Some("hr_zone") || kind == Some("power_zone") {
        let zone = t.get("zone").and_then(|z| z.as_u64()).unwrap_or(0);
        return format!(
            "Z{zone} {}",
            if kind == Some("hr_zone") {
                "FC"
            } else {
                "puissance"
            }
        );
    }
    if kind == Some("pace_range") {
        let pace = |k: &str| {
            t.get(k)
                .and_then(|v| v.as_f64())
                .map(format::format_pace_secs)
                .unwrap_or_default()
        };
        let (fast, slow) = (pace("low"), pace("high"));
        return if fast == slow {
            format!("{fast} /km")
        } else {
            format!("{fast}–{slow} /km")
        };
    }
    let unit = if kind == Some("hr_range") { "bpm" } else { "W" };
    let low = t.get("low").and_then(|l| l.as_f64()).unwrap_or(0.0);
    let high = t.get("high").and_then(|h| h.as_f64()).unwrap_or(low);
    if (low - high).abs() < 0.01 {
        format!("{low:.0} {unit}")
    } else {
        format!("{low:.0}–{high:.0} {unit}")
    }
}

/// Compact, symbol-based step list: one row per step (kind, ⏱ duration,
/// 🎯 target, terrain, 📝 note) and 🔁 blocks for repeats.
fn render_workout_steps(steps: &[serde_json::Value]) -> String {
    let mut out = String::from("<ul class=\"steps\">");
    for step in steps {
        let step_type = step.get("type").and_then(|t| t.as_str()).unwrap_or("?");
        if step_type == "repeat" {
            let count = step.get("count").and_then(|c| c.as_u64()).unwrap_or(1);
            let inner: Vec<serde_json::Value> = step
                .get("steps")
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default();
            out += &format!(
                "<li class=\"step step-repeat\"><span class=\"step-kind\">🔁 ×{count}</span>{}</li>",
                render_workout_steps(&inner)
            );
            continue;
        }
        let (icon, label) = match step_type {
            "warmup" => ("🔥", "Échauffement"),
            "interval" => ("⚡", "Effort"),
            "recovery" => ("💤", "Récup."),
            "cooldown" => ("🧊", "Retour au calme"),
            _ => ("▫️", step_type),
        };
        let minutes = step
            .get("duration")
            .and_then(|d| d.get("value"))
            .and_then(|v| v.as_str())
            .map(crate::workout::parse_minutes)
            .unwrap_or(0.0);
        let mut chips = format!(
            "<span class=\"chip\" title=\"Durée\">⏱ {}</span>",
            format_step_duration(minutes)
        );
        if let Some(t) = step.get("target") {
            chips += &format!(
                "<span class=\"chip chip-target\" title=\"Cible\">🎯 {}</span>",
                escape_html(&format_step_target(t))
            );
        }
        match step.get("terrain").and_then(|t| t.as_str()) {
            Some("montée") => chips += "<span class=\"chip\" title=\"Terrain\">⛰ montée</span>",
            Some("vallonné") => {
                chips += "<span class=\"chip\" title=\"Terrain\">〰 vallonné</span>"
            }
            _ => {}
        }
        if let Some(note) = step
            .get("note")
            .and_then(|n| n.as_str())
            .filter(|n| !n.trim().is_empty())
        {
            chips += &format!("<span class=\"step-note\">📝 {}</span>", escape_html(note));
        }
        out += &format!(
            "<li class=\"step step-{}\"><span class=\"step-kind\">{icon} {}</span>{chips}</li>",
            escape_html(step_type),
            escape_html(label)
        );
    }
    out += "</ul>";
    out
}

fn render_digest_section(digest: Option<&DigestRow>, ath: &crate::workout::Athlete) -> String {
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
                "<h3>{}{}</h3>{}{}",
                escape_html(name),
                distance,
                workout_chart::chart_div(&w.to_string(), ath),
                render_workout_steps(&steps)
            )
        }
        None => format!("<pre>{}</pre>", escape_html(&latest.llm_response_text)),
    };

    format!(
        "<p class=\"digest-date\">{}{}</p>{}<p class=\"bilan clamp-3\">{}</p>",
        escape_html(&latest.date),
        latest
            .free_text
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(|t| format!(" — note: {}", escape_html(t)))
            .unwrap_or_default(),
        workout_html,
        annotate_acronyms(&escape_html(&latest.llm_response_text)),
    )
}

const RUN_COLOR: &str = "#0d9488";

// Matches the user's other training tools: gris/bleu/vert/jaune/orange/rouge/mauve.
const ZONE_COLORS: [&str; 7] = [
    "#9ca3af", "#3b82f6", "#22c55e", "#eab308", "#f97316", "#dc2626", "#a855f7",
];
const ZONE_NAMES: [&str; 7] = [
    "Récupération active",
    "Endurance",
    "Tempo",
    "Seuil",
    "VO2max",
    "Capacité anaérobie",
    "Neuromusculaire",
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
            (
                name,
                zone_range_watts(i, ftp),
                ZONE_COLORS[i],
                zone_secs[i],
                format!("power_{}", i + 1),
            )
        })
        .collect();
    render_zone_rows(&rows)
}

/// Horizontal bar-per-zone chart: each row shows the zone's own bar (its share
/// of total time, on a scale whose maximum is max(biggest zone, 60 %)) plus the share as a percentage and a duration. The value range
/// (e.g. "114-155W") is a hover tooltip, not inline text — the visible
/// label is just the zone name. `.zone-rows` (not each `.zone-row`) is the
/// grid container so every row shares the same column widths and the bars
/// all start at the same x position. Each row's `data-zone-id` (e.g.
/// "power_4") lets the activity detail page's per-lap JS find and fade the
/// portion of a zone's bar that falls outside the currently selected lap —
/// unused on the dashboard's aggregate view, harmless there.
pub(super) fn render_zone_rows(rows: &[(String, String, &str, i64, String)]) -> String {
    let total: i64 = rows.iter().map(|(_, _, _, s, _)| *s).sum();
    // A full bar stands for max(biggest zone share, 60 %): with a spread-out
    // distribution (biggest zone ~20 %) bars stay readable instead of
    // squashed, while a dominant zone still fills the bar.
    let max_pct = rows
        .iter()
        .map(|(_, _, _, s, _)| {
            if total > 0 {
                *s as f64 / total as f64 * 100.0
            } else {
                0.0
            }
        })
        .fold(0.0, f64::max);
    let scale = max_pct.max(60.0);
    let items: String = rows
        .iter()
        .map(|(name, range, color, secs, zone_id)| {
            let pct = if total > 0 { *secs as f64 / total as f64 * 100.0 } else { 0.0 };
            let bar_pct = pct / scale * 100.0;
            format!(
                "<div class=\"zone-row\" title=\"{}\" data-zone-id=\"{zone_id}\"><div class=\"zone-row-label\">{}</div>\
                 <div class=\"zone-row-bar-wrap\"><div class=\"zone-row-bar\" style=\"width:{bar_pct:.1}%;background:{color}\">\
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
    TSB_ZONES
        .iter()
        .position(|(upper, _, _)| tsb < *upper)
        .unwrap_or(5)
}

fn render_tsb_gauge(tsb: f64, rest_days_to_next_band: Option<i64>) -> String {
    let span = TSB_DISPLAY_BOUNDS[6] - TSB_DISPLAY_BOUNDS[0];
    let segments: String = (0..6)
        .map(|i| {
            let width_pct = (TSB_DISPLAY_BOUNDS[i + 1] - TSB_DISPLAY_BOUNDS[i]) / span * 100.0;
            format!(
                r#"<div class="tsb-gauge-seg" style="width:{width_pct:.3}%;background:{}"></div>"#,
                TSB_ZONES[i].2
            )
        })
        .collect();

    let cursor_pct =
        (tsb.clamp(TSB_DISPLAY_BOUNDS[0], TSB_DISPLAY_BOUNDS[6]) - TSB_DISPLAY_BOUNDS[0]) / span
            * 100.0;

    let zone_i = tsb_zone_index(tsb);
    let (next_upper, _, current_color) = TSB_ZONES[zone_i];
    let status_label = TSB_ZONES[zone_i].1;

    // The estimate itself is precomputed with the rest of the dashboard
    // summary (metrics::refresh_dashboard_summary); only the wording is here.
    let estimate_html = match rest_days_to_next_band {
        Some(days) if days > 0 && next_upper.is_finite() => {
            let next_label = TSB_ZONES[zone_i + 1].1;
            let day_word = if days == 1 { "jour" } else { "jours" };
            format!(
                r#"<p class="hint tsb-estimate">≈ {days} {day_word} de repos pour atteindre « {} »</p>"#,
                escape_html(next_label)
            )
        }
        _ => String::new(),
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

/// Days of history sent to the load chart (the select narrows it client-side)
/// and days of rest projected forward.
const LOAD_CHART_HISTORY_DAYS: i64 = 180;
const LOAD_CHART_PROJECTION_DAYS: i64 = 7;

/// JSON for the "Évolution" chart: `daily_load` rows up to today, then the
/// same Banister recursion (`recompute_daily_load`) continued with TSS = 0 —
/// what CTL/ATL/TSB do if no activity is recorded in the coming days.
async fn load_chart_json(pool: &SqlitePool) -> String {
    let since = (chrono::Utc::now().date_naive() - chrono::Duration::days(LOAD_CHART_HISTORY_DAYS))
        .to_string();
    let rows: Vec<(String, f64, f64, f64, f64)> = sqlx::query_as(
        "SELECT date, tss, ctl, atl, tsb FROM daily_load WHERE date >= ? ORDER BY date",
    )
    .bind(since)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let r1 = |v: f64| (v * 10.0).round() / 10.0;
    let mut points: Vec<serde_json::Value> = rows
        .iter()
        .map(|(d, tss, ctl, atl, tsb)| serde_json::json!({"d": d, "tss": r1(*tss), "ctl": r1(*ctl), "atl": r1(*atl), "tsb": r1(*tsb)}))
        .collect();
    let today_idx = points.len().saturating_sub(1);
    if let Some((last_date, _, mut ctl, mut atl, _)) = rows.last().cloned() {
        if let Ok(mut day) = NaiveDate::parse_from_str(&last_date, "%Y-%m-%d") {
            for _ in 0..LOAD_CHART_PROJECTION_DAYS {
                day += chrono::Duration::days(1);
                ctl += -ctl / 42.0;
                atl += -atl / 7.0;
                points.push(serde_json::json!({"d": day.to_string(), "tss": 0, "ctl": r1(ctl), "atl": r1(atl), "tsb": r1(ctl - atl)}));
            }
        }
    }
    serde_json::json!({
        "points": points,
        "todayIdx": today_idx,
        "bands": TSB_ZONES.iter().take(5).map(|z| z.0).collect::<Vec<_>>(),
        "colors": TSB_ZONES.iter().map(|z| z.2).collect::<Vec<_>>(),
    })
    .to_string()
    .replace("</", "<\\/")
}

/// Collapsible CTL / ATL / TSB chart with a rest projection (dashed).
fn render_load_chart(json: &str) -> String {
    format!(
        r#"<details class="load-chart"><summary title="Évolution et prévision" aria-label="Afficher l'évolution et la prévision"></summary>
<div class="load-chart-controls">
  <label>Historique <select id="lc-range"><option value="14">14 jours</option><option value="21" selected>21 jours</option><option value="30">30 jours</option><option value="60">60 jours</option><option value="90">90 jours</option><option value="180">180 jours</option></select></label>
  <label class="inline-check"><input type="checkbox" id="lc-ctl" checked> <span style="color:#3b82f6">● CTL</span></label>
  <label class="inline-check"><input type="checkbox" id="lc-atl" checked> <span style="color:#f97316">● ATL</span></label>
  <label class="inline-check"><input type="checkbox" id="lc-tsb" checked> <span style="color:#16a34a">● TSB</span></label>
  <label class="inline-check"><input type="checkbox" id="lc-tss"> <span style="color:var(--text-muted)">▮ TSS</span></label>
</div>
<div class="load-chart-wrap"><canvas id="lc-canvas"></canvas></div>
</details>
<script>{script}</script>"#,
        script = LOAD_CHART_JS.replace("__DATA__", json),
    )
}

const LOAD_CHART_JS: &str = r##"
(function() {
  var D = __DATA__;
  var details = document.querySelector('.load-chart');
  var chart = null;
  var COLORS = { ctl: '#3b82f6', atl: '#f97316', tsb: '#16a34a' };
  function fmt(d) { var p = d.split('-'); return p[2] + '/' + p[1]; }
  function build() {
    var range = parseInt(document.getElementById('lc-range').value, 10);
    var start = Math.max(0, D.todayIdx - range + 1);
    var pts = D.points.slice(start);
    var today = D.todayIdx - start;
    var labels = pts.map(function(p) { return fmt(p.d); });
    var proj = function(ctx) { return ctx.p0DataIndex >= today ? [6, 4] : undefined; };
    function line(key, label) {
      return { label: label, data: pts.map(function(p) { return p[key]; }), borderColor: COLORS[key], backgroundColor: COLORS[key],
               borderWidth: key === 'tsb' ? 2.5 : 2, pointRadius: 0, tension: 0.25, hidden: !document.getElementById('lc-' + key).checked,
               segment: { borderDash: proj }, yAxisID: 'y' };
    }
    var tss = { type: 'bar', label: 'TSS', data: pts.map(function(p) { return p.tss; }), backgroundColor: cssVar('--text-faint'), yAxisID: 'y2',
                hidden: !document.getElementById('lc-tss').checked, order: 5, barPercentage: 0.9 };
    var bands = { id: 'tsbBands', beforeDatasetsDraw: function(c) {
      var a = c.chartArea, y = c.scales.y, ctx = c.ctx, edges = [-1e9].concat(D.bands).concat([1e9]);
      ctx.save();
      for (var i = 0; i < D.colors.length; i++) {
        var top = Math.max(a.top, Math.min(a.bottom, y.getPixelForValue(edges[i + 1])));
        var bottom = Math.max(a.top, Math.min(a.bottom, y.getPixelForValue(edges[i])));
        if (bottom > top) { ctx.globalAlpha = 0.09; ctx.fillStyle = D.colors[i]; ctx.fillRect(a.left, top, a.right - a.left, bottom - top); }
      }
      ctx.globalAlpha = 1;
      var x = c.scales.x.getPixelForValue(today);
      ctx.strokeStyle = cssVar('--text-muted'); ctx.setLineDash([2, 3]); ctx.beginPath(); ctx.moveTo(x, a.top); ctx.lineTo(x, a.bottom); ctx.stroke();
      ctx.restore();
    } };
    if (chart) chart.destroy();
    chart = new Chart(document.getElementById('lc-canvas'), {
      type: 'line',
      data: { labels: labels, datasets: [line('ctl', 'CTL (forme)'), line('atl', 'ATL (fatigue)'), line('tsb', 'TSB (fraîcheur)'), tss] },
      plugins: [bands],
      options: { responsive: true, maintainAspectRatio: false, animation: false, interaction: { mode: 'index', intersect: false },
        scales: { x: { grid: { display: false }, ticks: { maxTicksLimit: 8 } },
                  y: { grid: { color: cssVar('--border') } },
                  y2: { position: 'right', display: false, beginAtZero: true, grid: { drawOnChartArea: false }, max: 400 } },
        plugins: { legend: { display: false },
          tooltip: { callbacks: { title: function(items) { var i = items[0].dataIndex; return items[0].label + (i > today ? ' (prévision)' : i === today ? " (aujourd'hui)" : ''); } } } } }
    });
    chart.options.scales.y2.display = !chart.data.datasets[3].hidden;
    chart.update();
  }
  function sync() {
    if (!chart) return;
    ['ctl', 'atl', 'tsb'].forEach(function(k, i) { chart.setDatasetVisibility(i, document.getElementById('lc-' + k).checked); });
    var showTss = document.getElementById('lc-tss').checked;
    chart.setDatasetVisibility(3, showTss);
    chart.options.scales.y2.display = showTss;
    chart.update();
  }
  details.addEventListener('toggle', function() { if (details.open && !chart) build(); });
  document.getElementById('lc-range').addEventListener('change', build);
  ['ctl', 'atl', 'tsb', 'tss'].forEach(function(k) { document.getElementById('lc-' + k).addEventListener('change', sync); });
})();
"##;

fn render_activity_card(a: &ActivityRow) -> String {
    let bilan = a
        .bilan
        .as_deref()
        .map(|t| format!("<p class=\"bilan\">{}</p>", annotate_acronyms(&escape_html(t))))
        .unwrap_or_else(|| format!("<p class=\"bilan pending\">bilan non généré — <a href=\"/activity/{}\">ajouter du contexte et lancer l'analyse</a></p>", a.id));
    let route = a.route_svg.clone().unwrap_or_else(|| {
        format!(
            "<div class=\"route-thumb route-thumb-empty\" style=\"width:{}px;height:{}px\"></div>",
            crate::route_thumb::ROUTE_THUMB_W,
            crate::route_thumb::ROUTE_THUMB_H,
        )
    });

    let stats = format!(
        r#"<div class="activity-stats">
<div>📏 {:.1} km</div>
<div>⏱ {}</div>
<div>⛰ {:.0} m</div>
{}{}{}
</div>"#,
        a.distance_m / 1000.0,
        format_duration_hms(a.duration_s),
        a.elevation_gain_m,
        if a.is_run {
            format::format_pace(a.distance_m, a.duration_s)
                .map(|p| format!("<div>🏃 {p} /km</div>"))
                .unwrap_or_default()
        } else {
            String::new()
        },
        a.tss
            .map(|t| format!(
                "<div>🔥 {} {t:.0}</div>",
                annotate_acronyms(if a.is_run { "hrTSS" } else { "TSS" })
            ))
            .unwrap_or_default(),
        a.intensity_factor
            .map(|v| format!("<div>🎯 {} {v:.2}</div>", annotate_acronyms("IF")))
            .unwrap_or_default(),
    );

    // Imported (GPX/FIT) activities use negative ids (see web::import) —
    // there's no matching Strava activity to link to for those.
    let strava_link = if a.id > 0 {
        format!(
            r#" · <a href="https://www.strava.com/activities/{}" target="_blank" rel="noopener">voir sur Strava ↗</a>"#,
            a.id
        )
    } else {
        String::new()
    };

    format!(
        r#"<li class="activity-card">
{route}
<div class="activity-body">
<div class="activity-meta"><a href="/activity/{}">{}</a>{strava_link}</div>
{}
</div>
{stats}
</li>"#,
        a.id,
        format_datetime_fr(&a.started_at),
        bilan
    )
}

const SYNC_STATUS_CSS: &str = r#"
.sync-status { margin: 0 0 1rem; padding: .6rem .9rem; border-radius: 6px; border: 1px solid currentColor; opacity: .85; }
.sync-status.error { color: #c0392b; }
.sync-status .spinner { display: inline-block; width: .8em; height: .8em; margin-right: .4em; border: 2px solid currentColor; border-right-color: transparent; border-radius: 50%; animation: sync-spin .8s linear infinite; }
@keyframes sync-spin { to { transform: rotate(360deg); } }
"#;

/// Polls `/sync/status`; shows the current step while a sync runs and reloads
/// the page once it finishes. Errors stay visible (the sync runs server-side).
const SYNC_STATUS_JS: &str = r#"
(function() {
  var box = document.getElementById('sync-status');
  var wasRunning = false;
  function esc(s) { var d = document.createElement('div'); d.textContent = s; return d.innerHTML; }
  function tick() {
    fetch('/sync/status').then(function(r) { return r.json(); }).then(function(p) {
      if (p.running) {
        wasRunning = true;
        var pct = p.total > 0 ? ' (' + p.done + '/' + p.total + ')' : '';
        box.hidden = false;
        box.className = 'sync-status';
        box.innerHTML = '<span class="spinner"></span> Synchronisation : ' + esc(p.step) + pct;
        setTimeout(tick, 1000);
      } else if (wasRunning) {
        if (p.error) {
          box.hidden = false;
          box.className = 'sync-status error';
          box.textContent = 'Échec de la synchronisation : ' + p.error;
        } else {
          location.reload();
        }
      }
    }).catch(function() { setTimeout(tick, 3000); });
  }
  tick();
})();
"#;

fn render_page(
    config: &AthleteConfig,
    load: &crate::metrics::DashboardSummary,
    activities: &[ActivityRow],
    calendar_activities: &[CalendarActivity],
    calendar_planned: &[CalendarPlanned],
    load_chart_json: &str,
    sync_source: &str,
) -> String {
    let zone_bar_html = render_zone_bar(&load.zone_secs, config.ftp_watts as f64);
    let tsb_gauge_html = render_tsb_gauge(load.tsb, load.rest_days_to_next_band);
    let activity_items = activities
        .iter()
        .map(render_activity_card)
        .collect::<String>();
    let calendar_html = render_calendar_html(calendar_activities, calendar_planned);

    let (sync_label, full_title) = if sync_source == "garmin" {
        (
            "Synchroniser Garmin",
            "Importe les sorties Garmin plus anciennes pas encore en base",
        )
    } else {
        (
            "Synchroniser Strava",
            "Importe aussi les activités plus anciennes pas encore en base (ex: courses à pied)",
        )
    };
    let actions = format!(
        r#"<form method="post" action="/sync">
  <button type="submit">{sync_label}</button>
  <button type="submit" formaction="/sync/full" class="secondary" title="{full_title}">Tout l'historique</button>
</form>
<form method="post" action="/import" enctype="multipart/form-data">
  <input type="file" name="file" accept=".gpx,.fit" required>
  <button type="submit" class="secondary">Importer GPX/FIT</button>
</form>"#
    );

    let body = format!(
        r#"{header}
<div id="sync-status" class="sync-status" hidden></div>
<script>{sync_status_js}</script>
<div class="dash-grid">
<div class="dash-main">

<section>
<h2>Calendrier</h2>
{calendar_html}
</section>

<section>
<h2>Activités récentes</h2>
<ul class="activity-list">{activity_items}</ul>
<script>
document.querySelectorAll('.activity-body .bilan:not(.pending)').forEach(function(p) {{
  if (p.scrollHeight > p.clientHeight + 1) p.title = 'Cliquer pour tout afficher';
  p.addEventListener('click', function() {{ p.classList.toggle('expanded'); }});
}});
</script>
</section>

</div>
<div class="dash-side">

<section>
<h2>4 dernières semaines</h2>
<div class="stats">
  <div class="stat"><div class="value">{tss_period:.0}</div><div class="label">{tss_label}</div></div>
  <div class="stat"><div class="value">{ctl:.1}</div><div class="label">{ctl_label}</div></div>
  <div class="stat"><div class="value">{atl:.1}</div><div class="label">{atl_label}</div></div>
  <div class="stat"><div class="value">{tsb:.1}</div><div class="label">{tsb_label}</div></div>
</div>
{tsb_gauge_html}
{load_chart_html}
{zone_bar_html}
</section>

</div>
</div>"#,
        load_chart_html = render_load_chart(load_chart_json),
        sync_status_js = SYNC_STATUS_JS,
        header = layout::page_header("Tableau de bord", "", &actions),
        tss_period = load.tss_period,
        tss_label = annotate_acronyms("TSS"),
        ctl = load.ctl,
        ctl_label = annotate_acronyms("CTL"),
        atl = load.atl,
        atl_label = annotate_acronyms("ATL"),
        tsb = load.tsb,
        tsb_label = annotate_acronyms("TSB"),
    );

    layout::Page::new("", layout::Nav::Home, &body)
        .css(&format!("{}{}", workout_chart::CHART_CSS, SYNC_STATUS_CSS))
        .head(&format!(r#"<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.4/dist/chart.umd.min.js"></script>{}"#, workout_chart::CHART_JS))
        .render()
}

/// "Digest hebdomadaire" card (generate button + latest digest), shown on the
/// Séances page. `warn` is the `?warn=` flag set by the digest handlers.
pub(super) async fn digest_section(pool: &SqlitePool, warn: Option<&str>) -> String {
    let config = fetch_athlete_config(pool)
        .await
        .expect("athlete_config seeded at startup");
    let digest = fetch_latest_digest(pool).await.expect("digests query");
    let has_api_key = crate::settings::fetch(pool)
        .await
        .map(|s| s.has_llm_key())
        .unwrap_or(false);
    let llm_notice_html = llm_notice(has_api_key, warn);
    let digest_html = render_digest_section(digest.as_ref(), &config.athlete());
    // The workout saved as a template from the latest digest, if it was valid.
    let template_id: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM workouts WHERE digest_id = (SELECT MAX(id) FROM digests) LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let plan_link = template_id
        .map(|id| format!(r#"<p><a class="btn secondary" href="/planned/new?from={id}">Planifier cette séance &rarr;</a></p>"#))
        .unwrap_or_default();
    format!(
        r#"<section>
<h2>Digest hebdomadaire</h2>
{llm_notice_html}
<form method="post" action="/digest">
  <label>Note libre (optionnel)
  <input type="text" name="free_text" placeholder="ex: sortie longue prévue mardi">
  </label>
  <button type="submit">Générer mon digest</button>
  <button type="submit" formaction="/digest/report" class="secondary">Télécharger le rapport IA</button>
</form>
<div class="workout-steps">
{digest_html}
</div>
{plan_link}
<script>
document.querySelectorAll('.clamp-3').forEach(function(p) {{
  p.title = 'Cliquer pour tout afficher / réduire';
  p.addEventListener('click', function() {{ p.classList.toggle('expanded'); }});
}});
</script>
</section>
"#
    )
}

#[derive(Deserialize, Default)]
pub(super) struct WarnQuery {
    pub(super) warn: Option<String>,
}

/// Shown next to every LLM action. Without a key for the chosen provider the action
/// buttons would only fail silently server-side, so say so up front and point
/// at the no-API workaround (a report file to paste into any chat assistant).
/// `warn=llm_failed` is set by the handlers when the API call itself errored.
pub(super) fn llm_notice(has_api_key: bool, warn: Option<&str>) -> String {
    if warn == Some("llm_failed") {
        return r#"<p class="warn-banner">L'appel au LLM a échoué (clé invalide, quota, réseau…) — voir les logs. Le rapport IA reste téléchargeable pour l'analyser dans un assistant en ligne.</p>"#.to_string();
    }
    if !has_api_key {
        return r#"<p class="warn-banner">Aucune clé API pour le fournisseur choisi : la génération automatique est désactivée. Renseigne-la dans <a href="/settings">Paramètres</a>, ou télécharge le rapport IA et colle-le dans Claude, ChatGPT… pour obtenir la même analyse.</p>"#.to_string();
    }
    String::new()
}

/// Plain-text download response for the "rapport IA" files.
pub(super) fn markdown_download(filename: &str, body: String) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/markdown; charset=utf-8".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

async fn dashboard(State(state): State<AppState>) -> Html<String> {
    let config = fetch_athlete_config(&state.pool)
        .await
        .expect("athlete_config seeded at startup");
    // Single-row read of the precomputed summary; only rebuilt on the first
    // page load of a new day (see `metrics::dashboard_summary`).
    let load = crate::metrics::dashboard_summary(&state.pool)
        .await
        .expect("dashboard summary");
    let activities = fetch_recent_activities(&state.pool)
        .await
        .expect("activities query");
    let calendar_activities = fetch_calendar_activities(&state.pool)
        .await
        .expect("calendar query");
    let calendar_planned = fetch_calendar_planned(&state.pool, &config.athlete())
        .await
        .expect("planned query");
    let sync_source = crate::settings::fetch(&state.pool)
        .await
        .map(|settings| settings.sync_source)
        .unwrap_or_else(|_| "strava".into());
    Html(render_page(
        &config,
        &load,
        &activities,
        &calendar_activities,
        &calendar_planned,
        &load_chart_json(&state.pool).await,
        &sync_source,
    ))
}

#[derive(Deserialize)]
struct DigestForm {
    #[serde(default)]
    free_text: String,
}

async fn trigger_digest(State(state): State<AppState>, Form(form): Form<DigestForm>) -> Redirect {
    let Ok(settings) = crate::settings::fetch(&state.pool).await else {
        tracing::error!("failed to load settings for digest generation");
        return Redirect::to("/workouts");
    };
    if !settings.has_llm_key() {
        tracing::warn!("digest requested without an API key, skipping LLM call");
        return Redirect::to("/workouts?warn=no_api_key");
    }
    let client = crate::llm::ProviderClient::from_settings(&settings);
    let free_text = (!form.free_text.trim().is_empty()).then_some(form.free_text.as_str());
    if let Err(err) = crate::digest::generate(
        &state.pool,
        &client,
        free_text,
        &settings.athlete_profile,
        &settings.digest_system_prompt,
    )
    .await
    {
        tracing::error!(?err, "digest generation failed");
        return Redirect::to("/workouts?warn=llm_failed");
    }
    Redirect::to("/workouts")
}

/// No-API alternative to `trigger_digest`: same prompt + same data, as a
/// markdown file to paste into any chat assistant.
async fn digest_report(
    State(state): State<AppState>,
    Form(form): Form<DigestForm>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let settings = match crate::settings::fetch(&state.pool).await {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(?err, "failed to load settings for digest report");
            return Redirect::to("/workouts").into_response();
        }
    };
    let free_text = (!form.free_text.trim().is_empty()).then_some(form.free_text.as_str());
    match crate::digest::build_user_message(&state.pool, free_text, &settings.athlete_profile).await
    {
        Ok(user_message) => {
            let body = crate::report::render(
                "Digest hebdomadaire",
                &settings.digest_system_prompt,
                &user_message,
                crate::report::DIGEST_ANSWER_FORMAT,
            );
            let date = chrono::Utc::now().date_naive();
            markdown_download(&format!("cyclai-digest-{date}.md"), body)
        }
        Err(err) => {
            tracing::error!(?err, "digest report generation failed");
            Redirect::to("/workouts").into_response()
        }
    }
}

/// Starts a Strava sync in the background and returns right away; the
/// dashboard polls `/sync/status` to show the current step. Mirrors the `sync`
/// CLI command (same pipeline), triggered by a button instead of the timer.
async fn trigger_sync(State(state): State<AppState>) -> Redirect {
    crate::sync::spawn(state.pool.clone(), false);
    Redirect::to("/")
}

/// Same as `trigger_sync` but walks the whole Strava history for activities
/// not stored yet — how runs from before running support get imported.
async fn trigger_full_sync(State(state): State<AppState>) -> Redirect {
    crate::sync::spawn(state.pool.clone(), true);
    Redirect::to("/")
}

async fn sync_status() -> axum::Json<crate::sync::Progress> {
    axum::Json(crate::sync::progress())
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
    fn band_bounds_match_metrics_rest_estimate_bounds() {
        let web: Vec<f64> = TSB_ZONES
            .iter()
            .map(|(upper, _, _)| *upper)
            .filter(|u| u.is_finite())
            .collect();
        assert_eq!(web, crate::metrics::TSB_BAND_UPPER_BOUNDS.to_vec());
    }
}
