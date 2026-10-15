//! `/route`: upload a planned-route GPX and see it on a map next to its
//! elevation profile, colored by estimated gradient. Stateless — the file is
//! parsed per request and nothing is stored (see `route_profile` for how the
//! gradient is estimated).

use super::escape_html;
use axum::extract::Multipart;
use axum::response::Html;
use serde_json::json;

use crate::route_profile::RouteProfile;

pub async fn page() -> Html<String> {
    Html(render(None, None))
}

pub async fn analyze(mut multipart: Multipart) -> Html<String> {
    let mut file_name = None;
    let mut bytes = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            file_name = field.file_name().map(|s| s.to_string());
            bytes = field.bytes().await.ok();
        }
    }
    let Some(bytes) = bytes.filter(|b| !b.is_empty()) else {
        return Html(render(None, Some("Aucun fichier reçu.")));
    };
    match crate::route_profile::parse_gpx(&bytes) {
        Ok(profile) => Html(render(Some((&profile, file_name.as_deref())), None)),
        Err(err) => {
            tracing::warn!(?err, "route GPX could not be analyzed");
            Html(render(None, Some(&format!("Fichier GPX illisible : {err}"))))
        }
    }
}

fn stat(label: &str, value: String) -> String {
    format!(r#"<div class="stat"><div class="value">{value}</div><div class="label">{label}</div></div>"#)
}

fn render(profile: Option<(&RouteProfile, Option<&str>)>, error: Option<&str>) -> String {
    let error_html = error
        .map(|e| format!(r#"<p class="form-error">{}</p>"#, escape_html(e)))
        .unwrap_or_default();

    let result_html = match profile {
        None => String::new(),
        Some((p, file_name)) => {
            let title = p
                .name
                .as_deref()
                .or(file_name)
                .map(escape_html)
                .unwrap_or_else(|| "Itinéraire".to_string());
            let mut stats = stat("Distance", format!("{:.1} km", p.total_km));
            if p.has_elevation() {
                stats += &stat("D+", format!("{:.0} m", p.ascent_m));
                stats += &stat("D−", format!("{:.0} m", p.descent_m));
                stats += &stat("Alt. min / max", format!("{:.0} / {:.0} m", p.min_ele_m, p.max_ele_m));
                stats += &stat("Pente max", format!("{:.1} %", p.max_grade_pct));
                stats += &stat("Descente max", format!("{:.1} %", p.min_grade_pct));
            }
            let round = |v: &[f64], digits: i32| -> Vec<f64> {
                let k = 10f64.powi(digits);
                v.iter().map(|x| (x * k).round() / k).collect()
            };
            let data = json!({
                "latlng": p.latlng.iter().map(|[a, b]| [(a * 1e6).round() / 1e6, (b * 1e6).round() / 1e6]).collect::<Vec<_>>(),
                "dist": round(&p.dist_km, 3),
                "ele": round(&p.ele_m, 1),
                "grade": round(&p.grade_pct, 1),
            })
            .to_string()
            .replace("</", "<\\/");
            let profile_html = if p.has_elevation() {
                r#"<section>
<h2>Profil et pente estimée</h2>
<div class="grade-legend" id="grade-legend"></div>
<div class="route-chart-wrap"><canvas id="route-chart"></canvas></div>
<label class="route-toggle"><input type="checkbox" id="show-grade-line"> Afficher aussi la courbe de pente (axe de droite)</label>
<p class="hint">Pente estimée sur ~100 m après lissage de l'altitude (le bruit GPS ferait sinon osciller la pente) ; D+/D− calculés sur l'altitude lissée.</p>
</section>"#
            } else {
                r#"<section><p class="hint">Ce fichier ne contient pas d'altitude : pas de profil ni de pente, seulement le tracé.</p></section>"#
            };
            format!(
                r##"<section>
<h2>{title}</h2>
<div class="stats">{stats}</div>
<div id="route-map" class="route-map"></div>
</section>
{profile_html}
<script>
(function() {{
  var D = {data};
  var map = L.map('route-map');
  L.tileLayer('https://{{s}}.tile.openstreetmap.org/{{z}}/{{x}}/{{y}}.png', {{
    maxZoom: 19, attribution: '&copy; OpenStreetMap contributors'
  }}).addTo(map);
  var line = L.polyline(D.latlng, {{color: cssVar('--accent'), weight: 4}}).addTo(map);
  map.fitBounds(line.getBounds(), {{padding: [20, 20]}});
  L.circleMarker(D.latlng[0], {{radius: 7, color: '#fff', weight: 2, fillColor: '#16a34a', fillOpacity: 1}}).addTo(map).bindTooltip('Départ');
  L.circleMarker(D.latlng[D.latlng.length - 1], {{radius: 7, color: '#fff', weight: 2, fillColor: '#dc2626', fillOpacity: 1}}).addTo(map).bindTooltip('Arrivée');
  var cursor = L.circleMarker(D.latlng[0], {{radius: 6, color: '#fff', weight: 2, fillColor: cssVar('--text'), fillOpacity: 1}});

  if (!D.ele.length) return;

  // Gradient color scale (descents blue, flat green, climbs yellow to dark red).
  var BANDS = [[-99, -8, '#1d4ed8'], [-8, -3, '#60a5fa'], [-3, 3, '#22c55e'], [3, 6, '#eab308'], [6, 9, '#f97316'], [9, 12, '#dc2626'], [12, 99, '#7f1d1d']];
  var LABELS = ['< −8 %', '−8 à −3 %', '−3 à 3 %', '3 à 6 %', '6 à 9 %', '9 à 12 %', '> 12 %'];
  function gradeColor(g) {{
    for (var i = 0; i < BANDS.length; i++) if (g < BANDS[i][1]) return BANDS[i][2];
    return BANDS[BANDS.length - 1][2];
  }}
  document.getElementById('grade-legend').innerHTML = BANDS.map(function(b, i) {{
    return '<span><i style="background:' + b[2] + '"></i>' + LABELS[i] + '</span>';
  }}).join('');

  var eleData = D.dist.map(function(x, i) {{ return {{x: x, y: D.ele[i]}}; }});
  var gradeData = D.dist.map(function(x, i) {{ return {{x: x, y: D.grade[i]}}; }});
  // A segment between samples i-1 and i takes the gradient at i, for both
  // the line and the fill under it.
  function segColor(ctx) {{ return gradeColor(D.grade[ctx.p1DataIndex]); }}
  function nearestIndex(km) {{
    var lo = 0, hi = D.dist.length - 1;
    while (lo < hi) {{ var mid = (lo + hi) >> 1; if (D.dist[mid] < km) lo = mid + 1; else hi = mid; }}
    return lo;
  }}
  var minE = Math.min.apply(null, D.ele), maxE = Math.max.apply(null, D.ele);
  var pad = Math.max(10, (maxE - minE) * 0.1);

  var chart = new Chart(document.getElementById('route-chart').getContext('2d'), {{
    type: 'line',
    data: {{ datasets: [
      {{ label: 'Altitude (m)', data: eleData, yAxisID: 'y', fill: 'origin', borderWidth: 2, pointRadius: 0, tension: 0,
         borderColor: '#6b7280', backgroundColor: '#9ca3af',
         segment: {{ borderColor: segColor, backgroundColor: function(ctx) {{ return gradeColor(D.grade[ctx.p1DataIndex]) + '99'; }} }} }},
      {{ label: 'Pente (%)', data: gradeData, yAxisID: 'yGrade', hidden: true, borderWidth: 1, pointRadius: 0, borderColor: cssVar('--text'), tension: 0.2 }}
    ] }},
    options: {{
      responsive: true, maintainAspectRatio: false, animation: false, parsing: false,
      interaction: {{ mode: 'nearest', axis: 'x', intersect: false }},
      plugins: {{
        legend: {{ display: false }},
        tooltip: {{
          callbacks: {{
            title: function(items) {{ return items.length ? items[0].parsed.x.toFixed(2) + ' km' : ''; }},
            label: function(item) {{
              var i = item.dataIndex;
              return item.datasetIndex === 0
                ? ['Altitude : ' + Math.round(D.ele[i]) + ' m', 'Pente : ' + D.grade[i].toFixed(1) + ' %']
                : 'Pente : ' + D.grade[i].toFixed(1) + ' %';
            }}
          }}
        }}
      }},
      onHover: function(evt, elements) {{
        if (!elements.length) {{ map.removeLayer(cursor); return; }}
        var i = elements[0].index;
        cursor.setLatLng(D.latlng[i]);
        if (!map.hasLayer(cursor)) cursor.addTo(map);
      }},
      scales: {{
        x: {{ type: 'linear', min: 0, max: D.dist[D.dist.length - 1], ticks: {{ callback: function(v) {{ return (Math.round(v * 10) / 10) + ' km'; }} }}, grid: {{ display: false }} }},
        y: {{ min: Math.floor((minE - pad) / 10) * 10, max: Math.ceil((maxE + pad) / 10) * 10, title: {{ display: true, text: 'm' }} }},
        yGrade: {{ display: false, position: 'right', min: -20, max: 20, grid: {{ drawOnChartArea: false }}, title: {{ display: true, text: '%' }} }}
      }}
    }}
  }});
  document.getElementById('route-chart').addEventListener('mouseleave', function() {{ map.removeLayer(cursor); }});
  document.getElementById('show-grade-line').addEventListener('change', function() {{
    chart.setDatasetVisibility(1, this.checked);
    chart.options.scales.yGrade.display = this.checked;
    chart.update();
  }});
}})();
</script>"##,
                data = data,
            )
        }
    };

    let body = format!(
        r#"{header}
<section>
<h2>Charger un itinéraire planifié</h2>
{error_html}
<form method="post" action="/route" enctype="multipart/form-data" class="toolbar">
  <input type="file" name="file" accept=".gpx" required>
  <button type="submit">Afficher</button>
</form>
<p class="hint">Fichier GPX exporté d'un planificateur (trace ou itinéraire). Rien n'est enregistré.</p>
</section>
{result_html}"#,
        header = super::layout::page_header("Itinéraire", "Carte, profil d'altitude et pentes d'un GPX planifié.", ""),
    );
    let head = r#"<link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css" crossorigin="" />
<script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js" crossorigin=""></script>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.4/dist/chart.umd.min.js"></script>"#;
    let css = r#".route-map { height: 420px; border-radius: 8px; }
.route-chart-wrap { height: 280px; }
.route-toggle { display: flex; align-items: center; gap: 0.4rem; margin-top: 0.6rem; }
.grade-legend { display: flex; flex-wrap: wrap; gap: 0.8rem; font-size: 0.75rem; color: var(--text-muted); margin-bottom: 0.6rem; }
.grade-legend i { display: inline-block; width: 12px; height: 12px; border-radius: 3px; margin-right: 4px; vertical-align: -2px; }"#;
    super::layout::Page::new("Itinéraire", super::layout::Nav::Route, &body)
        .head(head)
        .css(css)
        .render()
}
