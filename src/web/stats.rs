//! `/stats`: distance totals (ride / run), monthly volume, every recorded
//! track on one map, best-time records over benchmark distances and a radar
//! of best power over benchmark durations. Everything is read from the
//! activities table and the precomputed `activity_bests` /
//! `route_points_json` columns — no raw stream is parsed here.

use super::format::{format_datetime_fr, format_pace_secs};
use super::{escape_html, layout, AppState};
use crate::bests::{POWER_DURATIONS, RIDE_DISTANCES, RUN_DISTANCES};
use axum::extract::{Query, State};
use axum::response::Html;
use chrono::{Datelike, Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Deserialize, Default)]
pub struct StatsQuery {
    period: Option<String>,
}

const PERIODS: [(&str, &str); 4] = [("all", "Tout"), ("year", "Cette année"), ("90d", "90 jours"), ("30d", "30 jours")];

struct Act {
    id: i64,
    run: bool,
    started_at: String,
    distance_m: f64,
    duration_s: i64,
    elevation_m: f64,
    points: Option<String>,
}

/// A record and where it was set.
#[derive(Clone)]
struct Best {
    value: f64,
    activity_id: i64,
    started_at: String,
}

/// Lower bound (`started_at >= since`, RFC 3339 sorts as text) of a period.
fn since_for(period: &str) -> String {
    let today = Utc::now().date_naive();
    match period {
        "year" => format!("{}-01-01", today.year()),
        "90d" => (today - Duration::days(90)).to_string(),
        "30d" => (today - Duration::days(30)).to_string(),
        _ => String::new(),
    }
}

/// 12345.6 -> "12 346" (narrow no-break space as thousands separator).
fn group_thousands(v: f64) -> String {
    let digits = format!("{:.0}", v.round());
    let mut out = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_hours(secs: i64) -> String {
    format!("{} h {:02}", secs / 3600, (secs % 3600) / 60)
}

fn distance_label(m: u32) -> String {
    match m {
        21_097 => "Semi (21,1 km)".to_string(),
        42_195 => "Marathon (42,2 km)".to_string(),
        m => format!("{} km", m / 1000),
    }
}

fn activity_link(b: &Best) -> String {
    format!(r#"<a href="/activity/{}">{}</a>"#, b.activity_id, escape_html(&format_datetime_fr(&b.started_at)))
}

/// One row of a distance-records table: time and speed (ride) or pace (run).
fn record_cells(b: Option<&Best>, meters: u32, run: bool) -> String {
    let Some(b) = b else { return "<td class=\"muted\">—</td>".to_string() };
    let km = meters as f64 / 1000.0;
    let pace_or_speed = if run {
        format!("{} /km", format_pace_secs(b.value / km))
    } else {
        format!("{:.1} km/h", km / (b.value / 3600.0))
    };
    format!(
        "<td><strong>{}</strong> · {pace_or_speed}<br><span class=\"hint\">{}</span></td>",
        super::format_zone_duration(b.value.round() as i64),
        activity_link(b)
    )
}

fn distance_table(title: &str, targets: &[u32], run: bool, all: &BTreeMap<(bool, u32), Best>, period: &BTreeMap<(bool, u32), Best>, show_period: bool) -> String {
    let rows: String = targets
        .iter()
        .map(|t| {
            let period_cell = if show_period { record_cells(period.get(&(run, *t)), *t, run) } else { String::new() };
            format!("<tr><th>{}</th>{}{period_cell}</tr>", distance_label(*t), record_cells(all.get(&(run, *t)), *t, run))
        })
        .collect();
    let period_head = if show_period { "<th>Sur la période</th>" } else { "" };
    format!(
        r#"<h3>{title}</h3><div class="table-wrap"><table class="records-table"><thead><tr><th>Distance</th><th>Record</th>{period_head}</tr></thead><tbody>{rows}</tbody></table></div>"#
    )
}

pub async fn page(State(state): State<AppState>, Query(q): Query<StatsQuery>) -> Html<String> {
    let period = q.period.as_deref().filter(|p| PERIODS.iter().any(|(k, _)| k == p)).unwrap_or("all");
    let since = since_for(period);

    let rows: Vec<(i64, String, String, f64, i64, f64, Option<String>)> = sqlx::query_as(
        "SELECT id, sport_type, started_at, distance_m, duration_s, elevation_gain_m, route_points_json \
         FROM activities ORDER BY started_at",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let acts: Vec<Act> = rows
        .into_iter()
        .map(|(id, sport, started_at, distance_m, duration_s, elevation_m, points)| Act {
            id,
            run: crate::metrics::is_run(&sport),
            started_at,
            distance_m,
            duration_s,
            elevation_m,
            points: points.filter(|p| !p.is_empty()),
        })
        .collect();
    let in_period: Vec<&Act> = acts.iter().filter(|a| a.started_at.as_str() >= since.as_str()).collect();

    // ---- totals -------------------------------------------------------
    let (mut ride_m, mut run_m, mut secs, mut elev) = (0.0, 0.0, 0i64, 0.0);
    let mut months: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for a in &in_period {
        if a.run {
            run_m += a.distance_m;
        } else {
            ride_m += a.distance_m;
        }
        secs += a.duration_s;
        elev += a.elevation_m;
        let m = months.entry(a.started_at.chars().take(7).collect()).or_default();
        if a.run {
            m.1 += a.distance_m / 1000.0;
        } else {
            m.0 += a.distance_m / 1000.0;
        }
    }
    let tile = |value: String, label: &str| format!(r#"<div class="stat"><div class="value">{value}</div><div class="label">{label}</div></div>"#);
    let tiles = [
        tile(format!("{} km", group_thousands((ride_m + run_m) / 1000.0)), "Distance totale"),
        tile(format!("{} km", group_thousands(ride_m / 1000.0)), "🚴 À vélo"),
        tile(format!("{} km", group_thousands(run_m / 1000.0)), "🏃 À pied"),
        tile(format_hours(secs), "Temps d'activité"),
        tile(format!("{} m", group_thousands(elev)), "Dénivelé positif"),
        tile(in_period.len().to_string(), "Sorties"),
    ]
    .concat();

    // ---- highlights ---------------------------------------------------
    let highlight = |label: &str, best: Option<&&Act>, value: &dyn Fn(&Act) -> String| match best {
        Some(a) => format!(
            r#"<li><span>{label}</span><strong>{}</strong><a href="/activity/{}">{}</a></li>"#,
            value(a),
            a.id,
            escape_html(&format_datetime_fr(&a.started_at))
        ),
        None => String::new(),
    };
    let by = |f: &dyn Fn(&&Act) -> f64, run: Option<bool>| {
        in_period
            .iter()
            .filter(|a| run.is_none_or(|r| a.run == r))
            .max_by(|a, b| f(a).total_cmp(&f(b)))
            .copied()
    };
    let longest_ride = by(&|a| a.distance_m, Some(false));
    let longest_run = by(&|a| a.distance_m, Some(true));
    let most_climb = by(&|a| a.elevation_m, None);
    let km_fmt = |a: &Act| format!("{:.1} km", a.distance_m / 1000.0);
    let highlights = [
        highlight("🚴 Plus longue sortie vélo", longest_ride.as_ref(), &km_fmt),
        highlight("🏃 Plus longue course", longest_run.as_ref(), &km_fmt),
        highlight("⛰ Plus gros dénivelé", most_climb.as_ref(), &|a| format!("{} m", group_thousands(a.elevation_m))),
    ]
    .concat();

    // ---- records ------------------------------------------------------
    let bests: Vec<(i64, String, i64, f64, String, String)> = sqlx::query_as(
        "SELECT b.activity_id, b.kind, b.key, b.value, a.started_at, a.sport_type \
         FROM activity_bests b JOIN activities a ON a.id = b.activity_id",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let mut power_all: BTreeMap<i64, Best> = BTreeMap::new();
    let mut power_period: BTreeMap<i64, Best> = BTreeMap::new();
    let mut dist_all: BTreeMap<(bool, u32), Best> = BTreeMap::new();
    let mut dist_period: BTreeMap<(bool, u32), Best> = BTreeMap::new();
    for (activity_id, kind, key, value, started_at, sport) in bests {
        let best = Best { value, activity_id, started_at };
        let is_period = best.started_at.as_str() >= since.as_str();
        if kind == "power" {
            let (a, p) = (&mut power_all, &mut power_period);
            for (map, ok) in [(a, true), (p, is_period)] {
                if ok && map.get(&key).is_none_or(|b| value > b.value) {
                    map.insert(key, best.clone());
                }
            }
        } else {
            let k = (crate::metrics::is_run(&sport), key as u32);
            let (a, p) = (&mut dist_all, &mut dist_period);
            for (map, ok) in [(a, true), (p, is_period)] {
                if ok && map.get(&k).is_none_or(|b| value < b.value) {
                    map.insert(k, best.clone());
                }
            }
        }
    }
    let show_period = period != "all";
    let mut records_html = String::new();
    if dist_all.keys().any(|(run, _)| !run) {
        records_html += &distance_table("🚴 Meilleures vitesses moyennes", &RIDE_DISTANCES, false, &dist_all, &dist_period, show_period);
    }
    if dist_all.keys().any(|(run, _)| *run) {
        records_html += &distance_table("🏃 Meilleures allures", &RUN_DISTANCES, true, &dist_all, &dist_period, show_period);
    }
    if records_html.is_empty() {
        records_html = r#"<p class="hint">Pas encore de records : ils sont calculés à partir des flux de distance de chaque sortie.</p>"#.to_string();
    }

    // ---- radar --------------------------------------------------------
    let duration_label = |s: usize| if s < 60 { format!("{s} s") } else { format!("{} min", s / 60) };
    let radar_labels: Vec<String> = POWER_DURATIONS.iter().map(|d| duration_label(*d)).collect();
    let series = |m: &BTreeMap<i64, Best>| -> Vec<serde_json::Value> {
        POWER_DURATIONS.iter().map(|d| m.get(&(*d as i64)).map_or(serde_json::Value::Null, |b| json!(b.value.round()))).collect()
    };
    let has_power = !power_all.is_empty();
    let power_chips: String = POWER_DURATIONS
        .iter()
        .filter_map(|d| power_all.get(&(*d as i64)).map(|b| (*d, b)))
        .map(|(d, b)| format!(r#"<span class="chip" title="{}">{} <strong>{:.0} W</strong></span>"#, escape_html(&format_datetime_fr(&b.started_at)), duration_label(d), b.value))
        .collect();

    // ---- map + chart data ---------------------------------------------
    let routes: Vec<serde_json::Value> = in_period
        .iter()
        .filter_map(|a| {
            let pts: serde_json::Value = serde_json::from_str(a.points.as_deref()?).ok()?;
            Some(json!({
                "id": a.id,
                "run": a.run,
                "label": format!("{} · {:.1} km", format_datetime_fr(&a.started_at), a.distance_m / 1000.0),
                "pts": pts,
            }))
        })
        .collect();
    let data = json!({
        "routes": routes,
        "months": {
            "labels": months.keys().collect::<Vec<_>>(),
            "ride": months.values().map(|m| (m.0 * 10.0).round() / 10.0).collect::<Vec<_>>(),
            "run": months.values().map(|m| (m.1 * 10.0).round() / 10.0).collect::<Vec<_>>(),
        },
        "radar": {
            "labels": radar_labels,
            "all": series(&power_all),
            "period": series(&power_period),
            "hasPeriod": show_period,
        },
    })
    .to_string()
    .replace("</", "<\\/");

    let tabs: String = PERIODS
        .iter()
        .map(|(k, label)| format!(r#"<a href="/stats?period={k}"{}>{label}</a>"#, if *k == period { r#" class="active""# } else { "" }))
        .collect();
    let map_section = if routes.is_empty() {
        r#"<p class="hint">Aucune trace GPS sur cette période.</p>"#.to_string()
    } else {
        format!(r#"<p class="hint">{} traces — survole pour mettre en avant, clique pour ouvrir la sortie. <span class="legend-dot" style="background:var(--accent)"></span> vélo <span class="legend-dot" style="background:#0d9488"></span> course</p><div id="stats-map" class="stats-map"></div>"#, routes.len())
    };
    let radar_section = if has_power {
        format!(
            r#"<section><h2>⚡ Puissance maximale par durée</h2><div class="radar-wrap"><canvas id="radar-chart"></canvas></div><div class="chip-row">{power_chips}</div></section>"#
        )
    } else {
        String::new()
    };

    let body = format!(
        r#"{header}
<div class="stats">{tiles}</div>
<div class="stats-grid">
<section><h2>Distance par mois</h2><div class="months-wrap"><canvas id="months-chart"></canvas></div></section>
<section><h2>Faits marquants</h2><ul class="highlights">{highlights}</ul></section>
</div>
<section><h2>🗺 Toutes les traces</h2>{map_section}</section>
<div class="stats-grid">
{radar_section}
<section><h2>🏁 Records sur distance</h2>{records_html}</section>
</div>
<script>{script}</script>"#,
        header = layout::page_header("Statistiques", "Distance, traces et records de toutes tes sorties.", &format!(r#"<div class="period-tabs">{tabs}</div>"#)),
        script = STATS_JS.replace("__DATA__", &data),
    );

    let head = r#"<link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css" crossorigin="" />
<script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js" crossorigin=""></script>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.4/dist/chart.umd.min.js"></script>"#;
    Html(layout::Page::new("Statistiques", layout::Nav::Stats, &body).class("wide").head(head).css(STATS_CSS).render())
}

const STATS_CSS: &str = r#"
.period-tabs { display: inline-flex; gap: 0.2rem; background: var(--card-bg); border: 1px solid var(--border); border-radius: 999px; padding: 0.2rem; }
.period-tabs a { padding: 0.3rem 0.85rem; border-radius: 999px; font-size: 0.85rem; color: var(--text-muted); font-weight: 500; }
.period-tabs a:hover { text-decoration: none; color: var(--text); }
.period-tabs a.active { background: var(--accent); color: var(--on-accent); }
.stats-grid { display: grid; gap: 1.25rem; grid-template-columns: minmax(0, 1fr); align-items: start; }
@media (min-width: 1000px) { .stats-grid { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); } }
.stats-map { height: 520px; border-radius: var(--radius-sm); }
.months-wrap { height: 260px; }
.radar-wrap { height: 340px; }
.chip-row { display: flex; flex-wrap: wrap; gap: 0.4rem; margin-top: 0.8rem; }
.highlights { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0.6rem; }
.highlights li { display: grid; grid-template-columns: 1fr auto; gap: 0.1rem 0.8rem; padding: 0.6rem 0.8rem; background: var(--bg); border-radius: var(--radius-sm); }
.highlights li span { color: var(--text-muted); font-size: 0.85rem; grid-column: 1 / -1; }
.highlights li a { font-size: 0.8rem; text-align: right; }
.legend-dot { display: inline-block; width: 10px; height: 10px; border-radius: 50%; margin: 0 0.2rem 0 0.6rem; vertical-align: -1px; }
.table-wrap { overflow-x: auto; }
.records-table { width: 100%; border-collapse: collapse; font-size: 0.85rem; margin-bottom: 0.8rem; }
.records-table th { text-align: left; color: var(--text-muted); font-weight: 600; font-size: 0.75rem; padding: 0.4rem 0.5rem; border-bottom: 1px solid var(--border); white-space: nowrap; }
.records-table td { padding: 0.45rem 0.5rem; border-bottom: 1px solid var(--border); vertical-align: top; }
.records-table tbody th { color: var(--text); font-size: 0.85rem; text-transform: none; letter-spacing: 0; }
.records-table .hint { margin: 0; font-size: 0.75rem; }
.muted { color: var(--text-faint); }
"#;

const STATS_JS: &str = r##"
(function() {
  var D = __DATA__;
  var accent = cssVar('--accent'), runColor = '#0d9488', muted = cssVar('--text-muted'), grid = cssVar('--border');

  if (D.routes.length && document.getElementById('stats-map')) {
    var map = L.map('stats-map');
    L.tileLayer('https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png', { maxZoom: 19, attribution: '&copy; OpenStreetMap contributors' }).addTo(map);
    var lines = D.routes.map(function(r) {
      var line = L.polyline(r.pts, { color: r.run ? runColor : accent, weight: 3, opacity: 0.5 }).addTo(map);
      line.bindTooltip(r.label, { sticky: true });
      line.on('mouseover', function() { line.setStyle({ weight: 5, opacity: 1 }); line.bringToFront(); });
      line.on('mouseout', function() { line.setStyle({ weight: 3, opacity: 0.5 }); });
      line.on('click', function() { window.location.href = '/activity/' + r.id; });
      return line;
    });
    map.fitBounds(L.featureGroup(lines).getBounds().pad(0.05));
  }

  if (D.months.labels.length) {
    new Chart(document.getElementById('months-chart'), {
      type: 'bar',
      data: { labels: D.months.labels, datasets: [
        { label: 'Vélo (km)', data: D.months.ride, backgroundColor: accent, borderRadius: 3 },
        { label: 'Course (km)', data: D.months.run, backgroundColor: runColor, borderRadius: 3 }
      ] },
      options: { responsive: true, maintainAspectRatio: false,
        scales: { x: { stacked: true, grid: { display: false } }, y: { stacked: true, beginAtZero: true } },
        plugins: { legend: { position: 'bottom' } } }
    });
  }

  var radarEl = document.getElementById('radar-chart');
  if (radarEl) {
    var datasets = [{ label: D.radar.hasPeriod ? 'Record' : 'Record (W)', data: D.radar.all, borderColor: accent, backgroundColor: accent + '33', pointBackgroundColor: accent, spanGaps: true }];
    if (D.radar.hasPeriod) datasets.push({ label: 'Sur la période', data: D.radar.period, borderColor: '#f97316', backgroundColor: '#f9731633', pointBackgroundColor: '#f97316', spanGaps: true });
    new Chart(radarEl, {
      type: 'radar',
      data: { labels: D.radar.labels, datasets: datasets },
      options: { responsive: true, maintainAspectRatio: false,
        scales: { r: { beginAtZero: true, grid: { color: grid }, angleLines: { color: grid }, pointLabels: { color: muted, font: { size: 12 } },
                       ticks: { backdropColor: 'transparent', color: muted, callback: function(v) { return v + ' W'; } } } },
        plugins: { legend: { display: D.radar.hasPeriod, position: 'bottom' },
                   tooltip: { callbacks: { label: function(c) { return c.dataset.label + ' : ' + c.parsed.r + ' W'; } } } } }
    });
  }
})();
"##;
