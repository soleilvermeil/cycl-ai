//! `/segments`: the reference segments synced from Strava favourites, with a
//! button to (re)import them. Selecting one (`?id=`) shows its latest
//! efforts (time, power, …), its position on a map and its elevation
//! profile. Segment geometry isn't stored — only start/end points are — so
//! the track and profile are sliced from the streams of a recent effort.

use super::format::format_datetime_fr;
use super::{escape_html, layout, AppState};
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize, Default)]
pub struct SegmentsQuery {
    id: Option<i64>,
}

const EFFORTS_SHOWN: i64 = 20;

pub async fn page(State(state): State<AppState>, Query(q): Query<SegmentsQuery>) -> Html<String> {
    Html(render(&state, q.id, None).await)
}

pub async fn import(State(state): State<AppState>) -> Response {
    match crate::segments::import_starred(&state.pool).await {
        Ok(()) => Redirect::to("/segments").into_response(),
        Err(err) => {
            tracing::warn!(?err, "starred segments import failed");
            Html(render(&state, None, Some(&format!("Import impossible : {err}"))).await).into_response()
        }
    }
}

struct SegmentRow {
    id: i64,
    name: String,
    distance_m: f64,
    elevation_gain_m: f64,
    efforts: i64,
    best_power_w: Option<f64>,
    last_at: Option<String>,
}

struct EffortRow {
    activity_id: i64,
    started_at: String,
    title: Option<String>,
    start_s: Option<f64>,
    end_s: Option<f64>,
    avg_power_w: Option<f64>,
    avg_speed_ms: Option<f64>,
    avg_hr: Option<f64>,
    delta_vs_pr_pct: Option<f64>,
}

impl EffortRow {
    fn duration_s(&self) -> Option<f64> {
        Some(self.end_s? - self.start_s?).filter(|d| *d > 0.0)
    }
}

/// "3:07" / "12:41" (m:ss) — efforts last minutes, `format_duration_hms`
/// (H:MM) would be useless here.
fn format_effort_time(secs: f64) -> String {
    let s = secs.round() as i64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn stat(label: &str, value: String) -> String {
    format!(r#"<div class="stat"><div class="value">{value}</div><div class="label">{label}</div></div>"#)
}

/// Segment track and profile, sliced from one activity's streams.
struct Geometry {
    latlng: Vec<[f64; 2]>,
    dist_km: Vec<f64>,
    ele_m: Vec<f64>,
    grade_pct: Vec<f64>,
}

/// Slices the [start_s, end_s] window out of an activity's streams, thins it
/// to a point every ~10 m, smooths the altitude and estimates the gradient
/// over ~100 m (GPS altitude noise would otherwise make it oscillate).
fn extract_geometry(streams_json: &str, start_s: f64, end_s: f64) -> Option<Geometry> {
    let v: serde_json::Value = serde_json::from_str(streams_json).ok()?;
    let latlng = crate::segments::latlng_array(&v)?;
    let distance = crate::segments::f64_array(&v, "distance")?;
    let time = crate::segments::f64_array(&v, "time")?;
    let altitude = crate::segments::f64_array(&v, "altitude");

    let n = latlng.len().min(distance.len()).min(time.len());
    let idx: Vec<usize> = (0..n).filter(|&i| time[i] >= start_s && time[i] <= end_s).collect();
    let (&first, _) = (idx.first()?, idx.last()?);

    let mut kept: Vec<usize> = vec![first];
    for &i in &idx[1..] {
        if distance[i] - distance[*kept.last().unwrap()] >= 10.0 {
            kept.push(i);
        }
    }
    let last = *idx.last().unwrap();
    if *kept.last().unwrap() != last {
        kept.push(last);
    }
    if kept.len() < 2 {
        return None;
    }

    let d0 = distance[first];
    let dist_m: Vec<f64> = kept.iter().map(|&i| distance[i] - d0).collect();
    let pts: Vec<[f64; 2]> = kept.iter().map(|&i| latlng[i]).collect();

    let (ele, grade) = match altitude.filter(|a| a.len() >= n) {
        Some(alt) => {
            let raw: Vec<f64> = kept.iter().map(|&i| alt[i]).collect();
            let smooth: Vec<f64> = (0..raw.len())
                .map(|i| {
                    let lo = i.saturating_sub(2);
                    let hi = (i + 2).min(raw.len() - 1);
                    raw[lo..=hi].iter().sum::<f64>() / (hi - lo + 1) as f64
                })
                .collect();
            let grade: Vec<f64> = (0..smooth.len())
                .map(|i| {
                    let mut j = i;
                    while j > 0 && dist_m[i] - dist_m[j] < 50.0 {
                        j -= 1;
                    }
                    let mut k = i;
                    while k + 1 < smooth.len() && dist_m[k] - dist_m[i] < 50.0 {
                        k += 1;
                    }
                    let dd = dist_m[k] - dist_m[j];
                    if dd > 10.0 {
                        ((smooth[k] - smooth[j]) / dd * 100.0).clamp(-30.0, 30.0)
                    } else {
                        0.0
                    }
                })
                .collect();
            (smooth, grade)
        }
        None => (Vec::new(), Vec::new()),
    };

    Some(Geometry { latlng: pts, dist_km: dist_m.iter().map(|d| d / 1000.0).collect(), ele_m: ele, grade_pct: grade })
}

async fn render(state: &AppState, selected: Option<i64>, error: Option<&str>) -> String {
    let rows: Vec<(i64, String, f64, f64, i64, Option<f64>, Option<String>)> = sqlx::query_as(
        "SELECT r.id, r.name, r.distance_m, r.elevation_gain_m, COUNT(e.id), MAX(e.avg_power_w), MAX(a.started_at) \
         FROM reference_segments r \
         LEFT JOIN segment_efforts e ON e.reference_segment_id = r.id \
         LEFT JOIN activities a ON a.id = e.activity_id \
         GROUP BY r.id ORDER BY r.name COLLATE NOCASE",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let segments: Vec<SegmentRow> = rows
        .into_iter()
        .map(|(id, name, distance_m, elevation_gain_m, efforts, best_power_w, last_at)| SegmentRow {
            id,
            name,
            distance_m,
            elevation_gain_m,
            efforts,
            best_power_w,
            last_at,
        })
        .collect();

    let error_html = error
        .map(|e| format!(r#"<p class="form-error">{}</p>"#, escape_html(e)))
        .unwrap_or_default();

    let list_html = if segments.is_empty() {
        r#"<p class="hint">Aucun segment synchronisé. Mets tes segments en favoris sur Strava puis clique sur « Importer depuis Strava ».</p>"#
            .to_string()
    } else {
        let body: String = segments
            .iter()
            .map(|s| {
                let active = if Some(s.id) == selected { r#" class="seg-active""# } else { "" };
                format!(
                    r#"<tr{active}><td><a href="/segments?id={id}">{name}</a></td><td>{km:.2} km</td><td>{d:.0} m</td><td>{n}</td><td>{p}</td><td>{last}</td></tr>"#,
                    id = s.id,
                    name = escape_html(&s.name),
                    km = s.distance_m / 1000.0,
                    d = s.elevation_gain_m,
                    n = s.efforts,
                    p = s.best_power_w.map(|p| format!("{p:.0} W")).unwrap_or_else(|| "–".into()),
                    last = s.last_at.as_deref().map(format_datetime_fr).unwrap_or_else(|| "–".into()),
                )
            })
            .collect();
        format!(
            r#"<div class="table-wrap"><table class="records-table seg-table"><thead><tr><th>Segment</th><th>Distance</th><th>D+</th><th>Passages</th><th>Meilleure puissance</th><th>Dernier passage</th></tr></thead><tbody>{body}</tbody></table></div>"#
        )
    };

    let detail_html = match selected.and_then(|id| segments.iter().find(|s| s.id == id)) {
        Some(seg) => render_detail(state, seg).await,
        None if selected.is_some() => r#"<section><p class="hint">Segment introuvable.</p></section>"#.to_string(),
        None if !segments.is_empty() => {
            r#"<section><p class="hint">Sélectionne un segment pour voir ses derniers passages, sa carte et son profil.</p></section>"#.to_string()
        }
        None => String::new(),
    };

    let actions = r#"<form method="post" action="/segments/import" onsubmit="var b=this.querySelector('button');b.disabled=true;b.textContent='Import en cours…'"><button type="submit">Importer depuis Strava</button></form>"#;
    let body = format!(
        r#"{header}
{error_html}
<section>
<h2>Segments synchronisés</h2>
{list_html}
<p class="hint">Seuls les segments en favoris sur Strava sont importés (coordonnées de départ et d'arrivée) ; les passages sont détectés par CyclAI sur tes sorties vélo avec capteur de puissance. L'import relance la détection sur toutes les sorties et peut prendre un moment.</p>
</section>
{detail_html}"#,
        header = layout::page_header("Segments", "Tes segments favoris et tes derniers passages dessus.", actions),
    );

    let head = r#"<link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css" crossorigin="" />
<script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js" crossorigin=""></script>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.4/dist/chart.umd.min.js"></script>"#;
    layout::Page::new("Segments", layout::Nav::Segments, &body).head(head).css(CSS).render()
}

async fn render_detail(state: &AppState, seg: &SegmentRow) -> String {
    let rows: Vec<(i64, String, Option<String>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>)> =
        sqlx::query_as(
            "SELECT a.id, a.started_at, a.title, e.start_time_s, e.end_time_s, e.avg_power_w, e.avg_speed_ms, e.avg_hr, e.delta_vs_pr_pct \
             FROM segment_efforts e JOIN activities a ON a.id = e.activity_id \
             WHERE e.reference_segment_id = ? ORDER BY a.started_at DESC LIMIT ?",
        )
        .bind(seg.id)
        .bind(EFFORTS_SHOWN)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    let efforts: Vec<EffortRow> = rows
        .into_iter()
        .map(|(activity_id, started_at, title, start_s, end_s, avg_power_w, avg_speed_ms, avg_hr, delta_vs_pr_pct)| EffortRow {
            activity_id,
            started_at,
            title,
            start_s,
            end_s,
            avg_power_w,
            avg_speed_ms,
            avg_hr,
            delta_vs_pr_pct,
        })
        .collect();

    // Track + profile from the most recent effort whose streams have altitude
    // (falling back to the most recent one with a usable track).
    let mut geometry: Option<Geometry> = None;
    for e in efforts.iter().take(5) {
        let (Some(s), Some(en)) = (e.start_s, e.end_s) else { continue };
        let streams: Option<String> = sqlx::query_scalar("SELECT streams_json FROM activity_streams WHERE activity_id = ?")
            .bind(e.activity_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
        let Some(g) = streams.and_then(|j| extract_geometry(&j, s, en)) else { continue };
        let has_ele = !g.ele_m.is_empty();
        if geometry.is_none() || has_ele {
            geometry = Some(g);
        }
        if has_ele {
            break;
        }
    }

    let mut stats = stat("Distance", format!("{:.2} km", seg.distance_m / 1000.0));
    stats += &stat("D+", format!("{:.0} m", seg.elevation_gain_m));
    if seg.distance_m > 0.0 {
        stats += &stat("Pente moy.", format!("{:.1} %", seg.elevation_gain_m / seg.distance_m * 100.0));
    }
    stats += &stat("Passages", seg.efforts.to_string());
    if let Some(p) = seg.best_power_w {
        stats += &stat("Meilleure puissance", format!("{p:.0} W"));
    }
    let best_time = efforts.iter().filter_map(|e| e.duration_s()).fold(None::<f64>, |a, d| Some(a.map_or(d, |b| b.min(d))));
    if let Some(t) = best_time {
        stats += &stat("Meilleur temps", format_effort_time(t));
    }

    let efforts_html = if efforts.is_empty() {
        r#"<p class="hint">Aucun passage détecté pour ce segment (il faut une sortie vélo avec puissance et GPS passant à moins de 15 m du départ puis de l'arrivée).</p>"#
            .to_string()
    } else {
        let best = best_time.unwrap_or(f64::MAX);
        let body: String = efforts
            .iter()
            .map(|e| {
                let name = e.title.as_deref().filter(|t| !t.is_empty()).map(escape_html).unwrap_or_else(|| "Sortie".into());
                let time = e
                    .duration_s()
                    .map(|d| {
                        let t = format_effort_time(d);
                        if (d - best).abs() < 0.5 { format!("{t} 🏆") } else { t }
                    })
                    .unwrap_or_else(|| "–".into());
                let delta = e
                    .delta_vs_pr_pct
                    .map(|d| format!("{d:+.1} %"))
                    .unwrap_or_else(|| "–".into());
                format!(
                    r#"<tr><td>{date}</td><td>{time}</td><td>{power}</td><td>{speed}</td><td>{hr}</td><td>{delta}</td><td><a href="/activity/{id}">{name}</a></td></tr>"#,
                    date = format_datetime_fr(&e.started_at),
                    power = e.avg_power_w.map(|p| format!("{p:.0} W")).unwrap_or_else(|| "–".into()),
                    speed = e.avg_speed_ms.map(|s| format!("{:.1} km/h", s * 3.6)).unwrap_or_else(|| "–".into()),
                    hr = e.avg_hr.map(|h| format!("{h:.0} bpm")).unwrap_or_else(|| "–".into()),
                    id = e.activity_id,
                )
            })
            .collect();
        format!(
            r#"<div class="table-wrap"><table class="records-table"><thead><tr><th>Date</th><th>Temps</th><th>Puissance</th><th>Vitesse</th><th>FC</th><th>Puiss. vs record</th><th>Sortie</th></tr></thead><tbody>{body}</tbody></table></div>
<p class="hint">{n} derniers passages. « Puiss. vs record » compare la puissance moyenne à la meilleure obtenue avant ce passage.</p>"#,
            n = efforts.len().min(EFFORTS_SHOWN as usize),
        )
    };

    let round = |v: &[f64], digits: i32| -> Vec<f64> {
        let k = 10f64.powi(digits);
        v.iter().map(|x| (x * k).round() / k).collect()
    };
    let (map_html, profile_html, script) = match &geometry {
        Some(g) => {
            let data = json!({
                "latlng": g.latlng.iter().map(|[a, b]| [(a * 1e6).round() / 1e6, (b * 1e6).round() / 1e6]).collect::<Vec<_>>(),
                "dist": round(&g.dist_km, 3),
                "ele": round(&g.ele_m, 1),
                "grade": round(&g.grade_pct, 1),
            })
            .to_string()
            .replace("</", "<\\/");
            let profile = if g.ele_m.is_empty() {
                r#"<p class="hint">Pas d'altitude dans les données de la sortie : pas de profil.</p>"#
            } else {
                r#"<h3>Profil et pente</h3><div class="grade-legend" id="seg-legend"></div><div class="seg-chart-wrap"><canvas id="seg-chart"></canvas></div>"#
            };
            (
                r#"<div id="seg-map" class="seg-map"></div>"#.to_string(),
                profile.to_string(),
                format!("<script>{}</script>", SEGMENT_JS.replace("__DATA__", &data)),
            )
        }
        None => (String::new(), String::new(), String::new()),
    };

    format!(
        r#"<section>
<h2>{name}</h2>
<div class="stats">{stats}</div>
{map_html}
{profile_html}
</section>
<section>
<h2>Derniers passages</h2>
{efforts_html}
</section>
{script}"#,
        name = escape_html(&seg.name),
    )
}

const CSS: &str = r#".seg-map { height: 380px; border-radius: 8px; margin-bottom: 1rem; }
.seg-chart-wrap { height: 240px; }
.seg-table tr.seg-active td { background: var(--accent-soft); }
.grade-legend { display: flex; flex-wrap: wrap; gap: 0.8rem; font-size: 0.75rem; color: var(--text-muted); margin-bottom: 0.6rem; }
.grade-legend i { display: inline-block; width: 12px; height: 12px; border-radius: 3px; margin-right: 4px; vertical-align: -2px; }
.table-wrap { overflow-x: auto; }
.records-table { width: 100%; border-collapse: collapse; font-size: 0.85rem; margin-bottom: 0.8rem; }
.records-table th { text-align: left; color: var(--text-muted); font-weight: 600; font-size: 0.75rem; padding: 0.4rem 0.5rem; border-bottom: 1px solid var(--border); white-space: nowrap; }
.records-table td { padding: 0.45rem 0.5rem; border-bottom: 1px solid var(--border); vertical-align: top; }
"#;

const SEGMENT_JS: &str = r##"
(function() {
  var D = __DATA__;
  var map = L.map('seg-map');
  L.tileLayer('https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png', { maxZoom: 19, attribution: '&copy; OpenStreetMap contributors' }).addTo(map);
  var line = L.polyline(D.latlng, { color: cssVar('--accent'), weight: 5 }).addTo(map);
  map.fitBounds(line.getBounds(), { padding: [30, 30] });
  L.circleMarker(D.latlng[0], { radius: 7, color: '#fff', weight: 2, fillColor: '#16a34a', fillOpacity: 1 }).addTo(map).bindTooltip('Départ');
  L.circleMarker(D.latlng[D.latlng.length - 1], { radius: 7, color: '#fff', weight: 2, fillColor: '#dc2626', fillOpacity: 1 }).addTo(map).bindTooltip('Arrivée');
  var cursor = L.circleMarker(D.latlng[0], { radius: 6, color: '#fff', weight: 2, fillColor: cssVar('--text'), fillOpacity: 1 });

  if (!D.ele.length) return;
  var BANDS = [[-99, -8, '#1d4ed8'], [-8, -3, '#60a5fa'], [-3, 3, '#22c55e'], [3, 6, '#eab308'], [6, 9, '#f97316'], [9, 12, '#dc2626'], [12, 99, '#7f1d1d']];
  var LABELS = ['< −8 %', '−8 à −3 %', '−3 à 3 %', '3 à 6 %', '6 à 9 %', '9 à 12 %', '> 12 %'];
  function gradeColor(g) {
    for (var i = 0; i < BANDS.length; i++) if (g < BANDS[i][1]) return BANDS[i][2];
    return BANDS[BANDS.length - 1][2];
  }
  document.getElementById('seg-legend').innerHTML = BANDS.map(function(b, i) {
    return '<span><i style="background:' + b[2] + '"></i>' + LABELS[i] + '</span>';
  }).join('');

  var eleData = D.dist.map(function(x, i) { return { x: x, y: D.ele[i] }; });
  var minE = Math.min.apply(null, D.ele), maxE = Math.max.apply(null, D.ele);
  var pad = Math.max(5, (maxE - minE) * 0.1);
  var el = document.getElementById('seg-chart');
  new Chart(el.getContext('2d'), {
    type: 'line',
    data: { datasets: [{ label: 'Altitude (m)', data: eleData, fill: 'origin', borderWidth: 2, pointRadius: 0, tension: 0,
      borderColor: '#6b7280', backgroundColor: '#9ca3af',
      segment: { borderColor: function(c) { return gradeColor(D.grade[c.p1DataIndex]); },
                 backgroundColor: function(c) { return gradeColor(D.grade[c.p1DataIndex]) + '99'; } } }] },
    options: {
      responsive: true, maintainAspectRatio: false, animation: false, parsing: false,
      interaction: { mode: 'nearest', axis: 'x', intersect: false },
      plugins: { legend: { display: false }, tooltip: { callbacks: {
        title: function(items) { return items.length ? items[0].parsed.x.toFixed(2) + ' km' : ''; },
        label: function(item) { var i = item.dataIndex; return ['Altitude : ' + Math.round(D.ele[i]) + ' m', 'Pente : ' + D.grade[i].toFixed(1) + ' %']; }
      } } },
      onHover: function(evt, els) {
        if (!els.length) { map.removeLayer(cursor); return; }
        cursor.setLatLng(D.latlng[els[0].index]);
        if (!map.hasLayer(cursor)) cursor.addTo(map);
      },
      scales: {
        x: { type: 'linear', min: 0, max: D.dist[D.dist.length - 1], ticks: { callback: function(v) { return (Math.round(v * 10) / 10) + ' km'; } }, grid: { display: false } },
        y: { min: Math.floor((minE - pad) / 5) * 5, max: Math.ceil((maxE + pad) / 5) * 5, title: { display: true, text: 'm' } }
      }
    }
  });
  el.addEventListener('mouseleave', function() { map.removeLayer(cursor); });
})();
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_slices_window_and_estimates_grade() {
        // 0..=600 m at 10 m / 2 s, climbing 5 % (0.5 m per 10 m).
        let n = 61;
        let latlng: Vec<[f64; 2]> = (0..n).map(|i| [49.0 + i as f64 * 1e-4, 6.0]).collect();
        let distance: Vec<f64> = (0..n).map(|i| i as f64 * 10.0).collect();
        let time: Vec<f64> = (0..n).map(|i| i as f64 * 2.0).collect();
        let altitude: Vec<f64> = (0..n).map(|i| 100.0 + i as f64 * 0.5).collect();
        let json = serde_json::json!({
            "latlng": {"data": latlng}, "distance": {"data": distance},
            "time": {"data": time}, "altitude": {"data": altitude},
        })
        .to_string();

        let g = extract_geometry(&json, 20.0, 40.0).unwrap();
        assert_eq!(g.latlng.len(), 11);
        assert!((g.dist_km[0]).abs() < 1e-9);
        assert!((g.dist_km.last().unwrap() - 0.1).abs() < 1e-9);
        assert!((g.grade_pct[5] - 5.0).abs() < 1.5); // edges are biased by the smoothing window
        let g = extract_geometry(&json, 0.0, 120.0).unwrap();
        assert!((g.grade_pct[30] - 5.0).abs() < 0.2);
        assert!(extract_geometry(&json, 1000.0, 2000.0).is_none());
    }
}
