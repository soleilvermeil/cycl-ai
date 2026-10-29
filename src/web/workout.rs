use super::workout_chart::{CHART_CSS, CHART_JS, chart_div};
use super::{AppState, escape_html};
use crate::planned::Sport;
use crate::workout::Athlete;
use axum::extract::{Form, Path, State};
use axum::response::{Html, Redirect};
use serde::Deserialize;
use serde_json::Value;
use sqlx::SqlitePool;

#[derive(sqlx::FromRow)]
struct WorkoutRow {
    id: i64,
    name: String,
    steps_json: String,
    source: String,
    created_at: String,
    sport: String,
}

pub(super) async fn fetch_athlete(pool: &SqlitePool) -> Athlete {
    Athlete::load(pool).await
}

pub async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<super::WarnQuery>,
) -> Html<String> {
    render_list_page(&state.pool, None, q.warn.as_deref()).await
}

async fn render_list_page(
    pool: &SqlitePool,
    error: Option<&str>,
    warn: Option<&str>,
) -> Html<String> {
    let rows: Vec<WorkoutRow> = sqlx::query_as(
        "SELECT id, name, steps_json, source, created_at, sport FROM workouts ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let ath = fetch_athlete(pool).await;

    let items_html: String = if rows.is_empty() {
        "<p class=\"hint\">Aucune séance pour l'instant.</p>".to_string()
    } else {
        rows.iter().map(|w| render_workout_card(w, &ath)).collect()
    };

    let error_html = error
        .map(|e| format!(r#"<p class="form-error">{}</p>"#, escape_html(e)))
        .unwrap_or_default();
    let create_form = builder_form_html(
        "/workouts",
        None,
        &ath,
        &template_extra_fields(Sport::Cycling),
    );
    let digest_html = super::digest_section(pool, warn).await;

    Html(page_shell(
        "Séances",
        &format!(
            r#"{header}
{digest_html}
<section><h2>Mes séances</h2><ul class="workout-list">{items_html}</ul></section>
<section><h2>Nouvelle séance</h2>{error_html}{create_form}</section>"#,
            header = super::layout::page_header(
                "Séances",
                "Modèles réutilisables et concepteur d'échelons.",
                ""
            ),
        ),
    ))
}

fn render_workout_card(w: &WorkoutRow, ath: &Athlete) -> String {
    let parsed: Value = serde_json::from_str(&w.steps_json).unwrap_or(Value::Null);
    let distance = parsed
        .get("estimated_distance_km")
        .and_then(|d| d.as_f64())
        .map(|d| format!(" · ~{d:.1}km estimés"))
        .unwrap_or_default();
    let source_label = if w.source == "digest" {
        "généré par digest"
    } else {
        "créé manuellement"
    };

    format!(
        r#"<li class="workout-card">
<div class="workout-card-head"><a href="/workouts/{id}">{name}</a><span class="workout-source">{sport_icon} {sport_label} · {source_label}</span></div>
{chart}
<div class="hint">{created}{distance} · <a href="/planned/new?from={id}">Planifier &rarr;</a></div>
</li>"#,
        id = w.id,
        sport_icon = Sport::from_db(&w.sport).icon(),
        sport_label = Sport::from_db(&w.sport).label(),
        name = escape_html(&w.name),
        chart = chart_div(&w.steps_json, ath),
        created = escape_html(&w.created_at),
    )
}

pub async fn edit_form(State(state): State<AppState>, Path(id): Path<i64>) -> Html<String> {
    let row: Option<WorkoutRow> = sqlx::query_as(
        "SELECT id, name, steps_json, source, created_at, sport FROM workouts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    let Some(row) = row else {
        return Html("<p>Séance introuvable. <a href=\"/workouts\">Retour</a></p>".to_string());
    };
    let ath = fetch_athlete(&state.pool).await;

    let form = builder_form_html(
        &format!("/workouts/{id}"),
        Some(&row.steps_json),
        &ath,
        &template_extra_fields(Sport::from_db(&row.sport)),
    );
    let delete_form = format!(
        r#"<form method="post" action="/workouts/{id}/delete" onsubmit="return confirm('Supprimer cette séance ?');">
<button type="submit" class="danger">Supprimer</button>
</form>"#
    );

    let zwo_link = if Sport::from_db(&row.sport) == Sport::Cycling {
        format!(
            r#"<p><a class="secondary-action" href="/workouts/{id}/zwo" download>Exporter pour Zwift (.zwo)</a></p>"#
        )
    } else {
        String::new()
    };

    Html(page_shell(
        "Modifier la séance",
        &format!(
            r#"<a class="back-link" href="/workouts">&larr; Retour aux séances</a>
<section><h2>Modifier: {name}</h2>{form}{zwo_link}{delete_form}</section>"#,
            name = escape_html(&row.name)
        ),
    ))
}

/// `GET /workouts/{id}/zwo` — the saved session as a Zwift workout file.
pub async fn export_zwo(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT name, steps_json FROM workouts WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    let Some((name, steps_json)) = row else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(mut workout) = serde_json::from_str::<crate::workout::Workout>(&steps_json) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    if workout.name.trim().is_empty() {
        workout.name = name.clone();
    }
    let ath = fetch_athlete(&state.pool).await;
    let file: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    (
        [
            (
                header::CONTENT_TYPE,
                "application/xml; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file}.zwo\""),
            ),
        ],
        crate::zwo::to_zwo(&workout, &ath),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct WorkoutForm {
    steps_json: String,
    /// Optional: also schedule a copy of this session on that day.
    #[serde(default)]
    planned_date: String,
    #[serde(default)]
    sport: String,
}

pub async fn create(State(state): State<AppState>, Form(form): Form<WorkoutForm>) -> Html<String> {
    save(&state, None, form).await
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<WorkoutForm>,
) -> Html<String> {
    save(&state, Some(id), form).await
}

async fn save(state: &AppState, id: Option<i64>, form: WorkoutForm) -> Html<String> {
    let ath = fetch_athlete(&state.pool).await;
    let sport = Sport::parse(&form.sport).unwrap_or_default();

    let value: Value = match serde_json::from_str(&form.steps_json) {
        Ok(v) => v,
        Err(err) => {
            return error_page(
                id,
                &form.steps_json,
                &format!("JSON invalide : {err}"),
                &ath,
                sport,
            );
        }
    };
    let prepared = match crate::planned::prepare(&value, &ath, sport) {
        Ok(p) => p,
        Err(err) => return error_page(id, &form.steps_json, &err, &ath, sport),
    };

    let planned_date = form.planned_date.trim();
    let planned_date = if planned_date.is_empty() {
        None
    } else {
        match crate::planned::parse_date(planned_date) {
            Some(d) => Some(d),
            None => {
                return error_page(
                    id,
                    &form.steps_json,
                    "Date de planification invalide.",
                    &ath,
                    sport,
                );
            }
        }
    };

    let template_id = match id {
        Some(id) => {
            let _ = sqlx::query(
                "UPDATE workouts SET name = ?, steps_json = ?, sport = ?, updated_at = datetime('now') WHERE id = ?",
            )
            .bind(&prepared.name)
            .bind(&prepared.steps_json)
            .bind(sport.as_str())
            .bind(id)
            .execute(&state.pool)
            .await;
            Some(id)
        }
        None => sqlx::query(
            "INSERT INTO workouts (name, steps_json, source, sport) VALUES (?, ?, 'manual', ?)",
        )
        .bind(&prepared.name)
        .bind(&prepared.steps_json)
        .bind(sport.as_str())
        .execute(&state.pool)
        .await
        .ok()
        .map(|r| r.last_insert_rowid()),
    };

    // The planned entry gets its own copy of the steps: later edits to this
    // template don't move sessions already on the calendar.
    if let Some(date) = planned_date {
        if let Err(err) = crate::planned::insert(
            &state.pool,
            date,
            &prepared,
            None,
            template_id,
            "manual",
            sport,
        )
        .await
        {
            tracing::error!(?err, "failed to schedule workout from the designer");
        }
    }

    render_list_page(&state.pool, None, None).await
}

/// Re-renders the same create/edit form with the submitted (unparsed) JSON
/// preserved and a visible error banner — a deliberate exception to the
/// app's usual swallow-error-and-redirect convention, since a malformed
/// step tree must be diagnosable rather than silently dropped.
fn error_page(
    id: Option<i64>,
    submitted_steps_json: &str,
    error: &str,
    ath: &Athlete,
    sport: Sport,
) -> Html<String> {
    let action = id
        .map(|i| format!("/workouts/{i}"))
        .unwrap_or_else(|| "/workouts".to_string());
    // builder_form_html tolerates invalid/partial JSON here (falls back to
    // an empty workout for any field it can't read) — the raw text is only
    // meant to survive round-tripping when it *was* structurally close.
    let form = builder_form_html(
        &action,
        Some(submitted_steps_json),
        ath,
        &template_extra_fields(sport),
    );
    let error_html = format!(r#"<p class="form-error">{}</p>"#, escape_html(error));
    Html(page_shell(
        "Séances",
        &format!(
            r#"<a class="back-link" href="/workouts">&larr; Retour aux séances</a>
<section><h2>Corriger la séance</h2>{error_html}{form}</section>"#
        ),
    ))
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> Redirect {
    let _ = sqlx::query("DELETE FROM workouts WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await;
    Redirect::to("/workouts")
}

/// `</script` inside a note/text field would otherwise close the seed
/// `<script>` tag early — `\/` is valid JSON string escaping and browsers
/// parse it back to `/`, so this is a safe, lossless guard.
pub(super) fn json_for_script(s: &str) -> String {
    s.replace("</", "<\\/")
}

/// `<select name="sport">` shared by the template and planned-session forms.
pub(super) fn sport_select_html(selected: Sport) -> String {
    let options: String = Sport::ALL
        .iter()
        .map(|s| {
            format!(
                r#"<option value="{}"{}>{} {}</option>"#,
                s.as_str(),
                if *s == selected { " selected" } else { "" },
                s.icon(),
                s.label()
            )
        })
        .collect();
    format!(r#"<label>Sport <select name="sport">{options}</select></label>"#)
}

/// Extra fields of the template designer: the sport, and an optional date to
/// also put the session on the calendar.
fn template_extra_fields(sport: Sport) -> String {
    format!(
        r#"{}
  <label>Planifier aussi cette séance le (optionnel)
    <input type="date" name="planned_date">
  </label>"#,
        sport_select_html(sport)
    )
}

/// The step designer (name, live chart, step rows). `extra_fields_html` is
/// inserted before the save button — the template designer and the planned
/// session pages each add their own fields (dates, notes...).
pub(super) fn builder_form_html(
    action: &str,
    initial_steps_json: Option<&str>,
    ath: &Athlete,
    extra_fields_html: &str,
) -> String {
    let initial_json = initial_steps_json.unwrap_or(r#"{"name":"","steps":[]}"#);
    let initial_value: Value = serde_json::from_str(initial_json).unwrap_or(Value::Null);
    let name = initial_value
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("");

    format!(
        r#"<script>window.__initialWorkout = {initial_json}; window.__workoutFtp = {ftp}; window.__workoutHr = {{rest: {hr_rest}, max: {hr_max}}}; window.__workoutRunPace = {run_pace};</script>
<form id="workout-form" method="post" action="{action}">
  <label>Nom de la séance
    <input type="text" id="workout-name" value="{name}" style="max-width:100%">
  </label>
  <div id="workout-preview" class="workout-chart workout-preview"></div>
  <p id="workout-summary" class="workout-summary"></p>
  <div id="step-builder"></div>
  <div id="top-add" class="step-row-add"></div>
  <input type="hidden" name="steps_json" id="steps_json_input">
  {extra_fields_html}
  <button type="submit">Enregistrer</button>
</form>
{builder_script}"#,
        initial_json = json_for_script(initial_json),
        ftp = ath.ftp_watts.round(),
        hr_rest = ath.hr_rest.round(),
        hr_max = ath.hr_max.round(),
        run_pace = ath.run_threshold_pace_s.round(),
        action = action,
        name = escape_html(name),
        builder_script = BUILDER_SCRIPT,
        extra_fields_html = extra_fields_html,
    )
}

/// Vanilla JS (no framework/build step) maintaining a plain object tree.
/// Rows are rendered recursively (repeats can nest to any depth, matching the
/// `Workout` data model, so LLM-generated nested sessions stay editable) and
/// wired with event delegation. Field edits mutate the tree in place and only
/// redraw the chart + summary, so text inputs never lose focus mid-edit;
/// structural changes (add/remove/move/duplicate) re-render the rows too.
/// Drawing and the duration grammar come from `workout_chart::CHART_JS`.
const BUILDER_SCRIPT: &str = r####"<script>
(function() {
  var FTP = window.__workoutFtp || 200;
  var HR = window.__workoutHr || {rest: 50, max: 190};
  // The threshold pace (s/km, 0 = unset) rides along with the HR settings into the chart helpers.
  HR.runPace = window.__workoutRunPace || 0;
  var sportEl = document.querySelector('select[name="sport"]');
  function sport() { return sportEl ? sportEl.value : 'cycling'; }
  var workout = window.__initialWorkout;
  if (!workout || typeof workout !== 'object') workout = {name: '', steps: []};
  if (!Array.isArray(workout.steps)) workout.steps = [];

  var TYPES = [['warmup', 'Échauffement'], ['interval', 'Intervalle'], ['recovery', 'Récupération'], ['cooldown', 'Retour au calme']];
  var TERRAINS = [['plat', 'Plat'], ['vallonné', 'Vallonné'], ['montée', 'Montée']];
  // New steps start at a sensible intensity for their type (fraction of FTP)
  // so the chart looks right as soon as a step is added.
  var DEFAULT_FRAC = {warmup: [0.55, 0.70], interval: [0.95, 1.05], recovery: [0.45, 0.55], cooldown: [0.45, 0.55]};
  // Running defaults: pace as a multiple of the threshold pace (low = fast end),
  // or a HR zone when no threshold pace is set.
  var RUN_PACE_MULT = {warmup: [1.28, 1.38], interval: [0.98, 1.02], recovery: [1.35, 1.50], cooldown: [1.28, 1.38]};
  var RUN_HR_ZONE = {warmup: 2, interval: 4, recovery: 1, cooldown: 1};
  var DEFAULT_DUR = {warmup: '10min', interval: '5min', recovery: '3min', cooldown: '10min'};

  var builderEl = document.getElementById('step-builder');
  var previewEl = document.getElementById('workout-preview');
  var summaryEl = document.getElementById('workout-summary');
  var topAddEl = document.getElementById('top-add');

  function num(x) { var v = parseFloat(x); return isFinite(v) ? v : 0; }
  function esc(s) {
    return String(s == null ? '' : s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }

  function newLeaf(type) {
    var f = DEFAULT_FRAC[type];
    var step = {type: type, duration: {type: 'time', value: DEFAULT_DUR[type]},
                target: {type: 'power_range', low: Math.round(FTP * f[0]), high: Math.round(FTP * f[1])}, terrain: 'plat', note: ''};
    // A running session starts from a pace target (or a HR zone when no
    // threshold pace is set), not watts.
    if (sport() === 'running') {
      if (HR.runPace > 0) {
        var m = RUN_PACE_MULT[type];
        step.target = {type: 'pace_range', low: Math.round(HR.runPace * m[0] / 5) * 5, high: Math.round(HR.runPace * m[1] / 5) * 5};
      } else {
        step.target = {type: 'hr_zone', zone: RUN_HR_ZONE[type]};
      }
    }
    return step;
  }
  function newRepeat() { return {type: 'repeat', count: 3, steps: [newLeaf('interval'), newLeaf('recovery')]}; }

  // Paths address a step as "2" (top level) or "2.1.0" (nested in repeats).
  function nodeAt(path) {
    var parts = path.split('.').map(Number);
    var node = workout.steps[parts[0]];
    for (var i = 1; i < parts.length; i++) node = node.steps[parts[i]];
    return node;
  }
  function listAt(listPath) {
    if (listPath === '') return workout.steps;
    var node = nodeAt(listPath);
    if (!Array.isArray(node.steps)) node.steps = [];
    return node.steps;
  }
  function splitPath(path) {
    var i = path.lastIndexOf('.');
    return {list: i < 0 ? '' : path.slice(0, i), idx: parseInt(i < 0 ? path : path.slice(i + 1), 10)};
  }

  function optionsHtml(options, current) {
    return options.map(function(o) {
      return '<option value="' + o[0] + '"' + (o[0] === current ? ' selected' : '') + '>' + o[1] + '</option>';
    }).join('');
  }

  var KIND_LABELS = {power_range: 'Puissance (W)', power_zone: 'Zone puissance', pace_range: 'Allure (min/km)', hr_range: 'FC (bpm)', hr_zone: 'Zone FC'};
  var KINDS_BY_SPORT = {
    cycling: ['power_range', 'power_zone', 'hr_range', 'hr_zone'],
    running: ['pace_range', 'hr_range', 'hr_zone'],
    other: ['power_range', 'power_zone', 'pace_range', 'hr_range', 'hr_zone']
  };
  // Target kinds offered for the current sport (a step keeps its own kind
  // even if the sport changed afterwards).
  function kindOptions(current) {
    var allowed = KINDS_BY_SPORT[sport()] || KINDS_BY_SPORT.cycling;
    var list = allowed.indexOf(current) >= 0 ? allowed : allowed.concat([current]);
    return list.map(function(k) { return [k, KIND_LABELS[k]]; });
  }

  // Short description shown next to the target fields.
  function targetMeta(step) {
    var t = step.target, lo = num(t.low), hi = num(t.high);
    if (t.type === 'hr_zone') {
      var b = window.workoutHrZoneBpm(parseInt(t.zone, 10) || 1, HR);
      return b[0] + '–' + b[1] + ' bpm';
    }
    if (t.type === 'hr_range') {
      var z = window.workoutHrZoneOf((lo + hi) / 2, HR);
      return '≈ Z' + z + ' ' + window.workoutHrZoneNames[z - 1];
    }
    if (t.type === 'power_zone') {
      var w = window.workoutPowerZoneRange(parseInt(t.zone, 10) || 1, FTP);
      return w[0] + '–' + w[1] + ' W';
    }
    if (t.type === 'pace_range') {
      return HR.runPace > 0 ? Math.round(HR.runPace / ((lo + hi) / 2 || 1) * 100) + ' % allure seuil' : 'allure seuil non réglée';
    }
    return Math.round(lo / FTP * 100) + (lo !== hi ? '–' + Math.round(hi / FTP * 100) : '') + ' % FTP';
  }
  function stepColor(step) {
    var it = window.workoutLeafIntensity(step.target, FTP, HR);
    return window.workoutZoneColor((it.lo + it.hi) / 2 / FTP);
  }

  // Switching the target kind keeps the step's intensity roughly the same
  // instead of resetting it, going through "fraction of FTP" as the common
  // scale (HR zones, pace and power zones all map onto it).
  function convertTarget(step, kind) {
    var cur = step.target.type;
    if (kind === cur) return;
    var it = window.workoutLeafIntensity(step.target, FTP, HR);
    var f = (it.lo + it.hi) / 2 / FTP;
    var hrZone = cur === 'hr_zone' ? step.target.zone
      : cur === 'hr_range' ? window.workoutHrZoneOf((num(step.target.low) + num(step.target.high)) / 2, HR)
      : (f < 0.58 ? 1 : f < 0.75 ? 2 : f < 0.90 ? 3 : f < 1.03 ? 4 : 5);
    if (kind === 'power_range') {
      step.target = {type: 'power_range', low: Math.round(FTP * Math.max(0.4, f - 0.05)), high: Math.round(FTP * (f + 0.05))};
    } else if (kind === 'power_zone') {
      step.target = {type: 'power_zone', zone: window.workoutPowerZoneOf(f)};
    } else if (kind === 'hr_zone') {
      step.target = {type: 'hr_zone', zone: hrZone};
    } else if (kind === 'hr_range') {
      var bpm = window.workoutHrZoneBpm(hrZone, HR);
      step.target = {type: 'hr_range', low: bpm[0], high: bpm[1]};
    } else if (kind === 'pace_range') {
      var p = HR.runPace > 0 ? HR.runPace / Math.max(f, 0.4) : 360;
      step.target = {type: 'pace_range', low: Math.round(p * 0.97 / 5) * 5, high: Math.round(p * 1.03 / 5) * 5};
    }
  }

  function targetFieldsHtml(step) {
    var t = step.target;
    var kindSelect = '<select data-f="tkind" aria-label="Type de cible">' + optionsHtml(kindOptions(t.type), t.type) + '</select>';
    if (t.type === 'hr_zone') {
      var zones = [1, 2, 3, 4, 5].map(function(z) {
        var b = window.workoutHrZoneBpm(z, HR);
        return [String(z), 'Z' + z + ' ' + window.workoutHrZoneNames[z - 1] + ' (' + b[0] + '–' + b[1] + ')'];
      });
      return kindSelect + '<select data-f="zone" aria-label="Zone FC">' + optionsHtml(zones, String(t.zone)) + '</select>';
    }
    if (t.type === 'power_zone') {
      var pzones = [1, 2, 3, 4, 5, 6, 7].map(function(z) {
        var w = window.workoutPowerZoneRange(z, FTP);
        return [String(z), 'Z' + z + ' ' + window.workoutPowerZoneNames[z - 1] + ' (' + w[0] + '–' + w[1] + ')'];
      });
      return kindSelect + '<select data-f="zone" aria-label="Zone de puissance">' + optionsHtml(pzones, String(t.zone)) + '</select>';
    }
    if (t.type === 'pace_range') {
      // low = fastest end of the range (fewer seconds per km).
      return kindSelect +
        '<input data-f="plow" type="text" class="in-pace" value="' + window.workoutFmtPace(num(t.low)) + '" placeholder="5:00" aria-label="Allure la plus rapide">' +
        '<span>&ndash;</span>' +
        '<input data-f="phigh" type="text" class="in-pace" value="' + window.workoutFmtPace(num(t.high)) + '" placeholder="5:30" aria-label="Allure la plus lente">' +
        '<span>/km</span>';
    }
    var unit = t.type === 'hr_range' ? 'bpm' : 'W';
    return kindSelect +
      '<input data-f="low" type="number" min="0" class="in-pw" value="' + esc(t.low) + '" aria-label="Cible basse">' +
      '<span>&ndash;</span>' +
      '<input data-f="high" type="number" min="0" class="in-pw" value="' + esc(t.high) + '" aria-label="Cible haute">' +
      '<span>' + unit + '</span>';
  }

  function moveButtonsHtml(path) {
    return '<span class="step-row-move">' +
      '<button type="button" data-act="up" data-path="' + path + '" title="Monter">&uarr;</button>' +
      '<button type="button" data-act="down" data-path="' + path + '" title="Descendre">&darr;</button>' +
      '<button type="button" data-act="dup" data-path="' + path + '" title="Dupliquer">&#10697;</button>' +
      '<button type="button" data-act="del" data-path="' + path + '" title="Supprimer">&times;</button></span>';
  }

  function addButtonsHtml(listPath) {
    return TYPES.map(function(t) {
      return '<button type="button" data-add="' + t[0] + '" data-list="' + listPath + '">+ ' + t[1] + '</button>';
    }).join('') + '<button type="button" data-add="repeat" data-list="' + listPath + '">+ Bloc répété</button>';
  }

  function stepHtml(step, path) {
    if (step.type === 'repeat') {
      if (!Array.isArray(step.steps)) step.steps = [];
      return '<div class="step-row step-repeat" data-path="' + path + '">' +
        '<div class="step-row-head"><strong>Répéter</strong> ×' +
        '<input data-f="count" type="number" min="1" value="' + esc(step.count) + '" style="width:3.5em">' +
        moveButtonsHtml(path) + '</div>' +
        '<div class="step-row-inner">' + stepsHtml(step.steps, path) +
        '<div class="step-row-add">' + addButtonsHtml(path) + '</div></div></div>';
    }
    if (!step.target) step.target = {type: 'power_range', low: 0, high: 0};
    if (!step.duration) step.duration = {type: 'time', value: ''};
    return '<div class="step-row step-leaf" data-path="' + path + '" style="border-left-color:' + stepColor(step) + '">' +
      '<select data-f="type">' + optionsHtml(TYPES, step.type) + '</select>' +
      '<input data-f="dur" type="text" class="in-dur" value="' + esc(step.duration.value) + '" placeholder="10min" aria-label="Durée">' +
      targetFieldsHtml(step) +
      '<span class="step-pct">' + targetMeta(step) + '</span>' +
      '<details class="step-more"><summary>détails</summary>' +
      '<select data-f="terrain">' + optionsHtml(TERRAINS, step.terrain || 'plat') + '</select>' +
      '<input data-f="note" type="text" value="' + esc(step.note) + '" placeholder="note (optionnel)"></details>' +
      moveButtonsHtml(path) + '</div>';
  }

  function stepsHtml(list, basePath) {
    return list.map(function(s, i) { return stepHtml(s, basePath === '' ? String(i) : basePath + '.' + i); }).join('');
  }

  function renderRows() {
    builderEl.innerHTML = stepsHtml(workout.steps, '') ||
      '<p class="hint">Aucune étape — ajoute-en une ci-dessous.</p>';
    topAddEl.innerHTML = addButtonsHtml('');
    refresh();
  }

  function refresh() {
    var st = window.renderWorkoutChart(previewEl, workout, FTP, HR);
    if (!st.blocks.length) { summaryEl.textContent = ''; return; }
    summaryEl.innerHTML = 'Durée <strong>' + window.workoutFmtDur(st.minutes) + '</strong> · TSS ≈ <strong>' +
      Math.round(st.tss) + '</strong> · IF ≈ <strong>' + st.intensity.toFixed(2) + '</strong>' +
      (st.hasInvalidRange ? ' · <span class="form-error-inline">une cible basse dépasse la haute</span>' : '');
  }

  function updateLeafMeta(row, step) {
    row.querySelector('.step-pct').textContent = targetMeta(step);
    row.style.borderLeftColor = stepColor(step);
    row.querySelector('[data-f="dur"]').classList.toggle('invalid', window.parseWorkoutMinutes(step.duration.value) <= 0);
  }

  builderEl.addEventListener('input', onField);
  builderEl.addEventListener('change', onField);
  function onField(e) {
    var field = e.target.getAttribute('data-f');
    var row = e.target.closest('[data-path]');
    if (!field || !row) return;
    var step = nodeAt(row.getAttribute('data-path'));
    var v = e.target.value;
    if (field === 'type') step.type = v;
    else if (field === 'dur') step.duration.value = v;
    else if (field === 'tkind') { convertTarget(step, v); return renderRows(); }
    else if (field === 'zone') step.target.zone = parseInt(v, 10) || 1;
    else if (field === 'plow') step.target.low = window.workoutParsePace(v);
    else if (field === 'phigh') step.target.high = window.workoutParsePace(v);
    else if (field === 'low') step.target.low = num(v);
    else if (field === 'high') step.target.high = num(v);
    else if (field === 'terrain') step.terrain = v;
    else if (field === 'note') step.note = v;
    else if (field === 'count') step.count = Math.max(1, parseInt(v, 10) || 1);
    if (step.type !== 'repeat') updateLeafMeta(row, step);
    refresh();
  }

  function onClick(e) {
    var add = e.target.closest('[data-add]');
    if (add) {
      var type = add.getAttribute('data-add');
      listAt(add.getAttribute('data-list')).push(type === 'repeat' ? newRepeat() : newLeaf(type));
      return renderRows();
    }
    var btn = e.target.closest('[data-act]');
    if (!btn) return;
    var path = btn.getAttribute('data-path'), act = btn.getAttribute('data-act');
    var loc = splitPath(path), list = listAt(loc.list);
    if (act === 'del') list.splice(loc.idx, 1);
    else if (act === 'dup') list.splice(loc.idx + 1, 0, JSON.parse(JSON.stringify(list[loc.idx])));
    else {
      var j = loc.idx + (act === 'up' ? -1 : 1);
      if (j < 0 || j >= list.length) return;
      var t = list[loc.idx]; list[loc.idx] = list[j]; list[j] = t;
    }
    renderRows();
  }
  builderEl.addEventListener('click', onClick);
  topAddEl.addEventListener('click', onClick);

  var nameInput = document.getElementById('workout-name');
  workout.name = workout.name || '';
  if (nameInput) nameInput.addEventListener('input', function() { workout.name = this.value; });

  var form = document.getElementById('workout-form');
  if (form) {
    form.addEventListener('submit', function() {
      workout.name = nameInput ? nameInput.value : workout.name;
      document.getElementById('steps_json_input').value = JSON.stringify(workout);
    });
  }

  if (sportEl) sportEl.addEventListener('change', renderRows);

  renderRows();
})();
</script>"####;

const BUILDER_CSS: &str = r#"
.workout-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 0.8rem; }
.workout-card { border: 1px solid var(--border); border-radius: var(--radius-sm); padding: 0.8rem 1rem; background: var(--card-bg); transition: box-shadow 0.15s, border-color 0.15s; }
.workout-card:hover { box-shadow: var(--shadow-hover); border-color: var(--border-strong); }
.workout-card-head { display: flex; justify-content: space-between; align-items: center; gap: 0.6rem; margin-bottom: 0.3rem; }
.workout-card-head a { font-weight: 600; color: var(--text); text-decoration: none; }
.workout-card-head a:hover { color: var(--accent); }
.workout-source { font-size: 0.75rem; color: var(--text-muted); background: var(--bg); border-radius: 999px; padding: 0.1rem 0.6rem; white-space: nowrap; }
.workout-preview { margin: 0.8rem 0 0.2rem; border: 1px solid var(--border); border-radius: 8px; padding: 0.4rem 0.6rem; background: var(--card-bg); }
.workout-summary { font-size: 0.85rem; color: var(--text-muted); margin: 0.2rem 0 0.9rem; min-height: 1.2em; }
.workout-summary strong { color: var(--text); }
.step-row { display: flex; flex-wrap: wrap; align-items: center; gap: 0.4rem; padding: 0.45rem 0.5rem; border: 1px solid var(--border); border-left: 5px solid var(--border); border-radius: 6px; margin-bottom: 0.45rem; background: var(--card-bg); }
.step-row input, .step-row select { margin-top: 0; }
.step-row select { border: 1px solid var(--border); border-radius: 6px; padding: 0.25rem 0.4rem; font-size: 0.85rem; font-family: inherit; }
.step-row input[type="text"], .step-row input[type="number"] { max-width: none; padding: 0.3rem 0.4rem; }
.step-row input.in-dur { width: 5.5em; }
.step-row input.in-pw { width: 4.5em; }
.step-row input.in-pace { width: 4em; }
.step-row input.invalid { border-color: var(--danger); background: var(--danger-soft); }
.step-pct { font-size: 0.78rem; color: var(--text-muted); min-width: 6.5em; }
.step-more { font-size: 0.78rem; color: var(--text-muted); }
.step-more summary { cursor: pointer; }
.step-more select, .step-more input { margin: 0.3rem 0.3rem 0 0; }
.step-row-move { margin-left: auto; display: flex; gap: 0.2rem; }
.step-row-move button, .step-row-add button { padding: 0.2rem 0.5rem; font-size: 0.78rem; margin: 0; }
.step-repeat { flex-direction: column; align-items: stretch; background: var(--bg); border-left-color: var(--accent); }
.step-row-head { display: flex; align-items: center; gap: 0.4rem; }
.step-row-inner { margin-top: 0.5rem; padding-left: 0.8rem; border-left: 2px solid var(--border); }
.step-row-add { display: flex; flex-wrap: wrap; gap: 0.4rem; margin: 0.4rem 0 0.8rem; }
"#;

pub(super) fn page_shell(title: &str, body: &str) -> String {
    let css = format!("{}{}", CHART_CSS, BUILDER_CSS);
    super::layout::Page::new(title, super::layout::Nav::Workouts, body)
        .class("workout-page")
        .css(&css)
        .head(CHART_JS)
        .render()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> crate::workout::Workout {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn accepts_nested_repeats_and_clock_style_durations() {
        let w = parse(
            r#"{"name":"x","steps":[
                {"type":"warmup","duration":{"type":"time","value":"1:30"},"target":{"type":"power_range","low":100,"high":130}},
                {"type":"repeat","count":2,"steps":[
                    {"type":"repeat","count":3,"steps":[
                        {"type":"interval","duration":{"type":"time","value":"1h"},"target":{"type":"power_range","low":200,"high":210}}
                    ]}
                ]}
            ]}"#,
        );
        assert_eq!(w.validate(), None);
    }

    #[test]
    fn rejects_empty_repeat_bad_duration_and_inverted_range() {
        let empty_repeat =
            parse(r#"{"name":"x","steps":[{"type":"repeat","count":2,"steps":[]}]}"#);
        assert!(
            empty_repeat
                .validate()
                .unwrap()
                .contains("au moins une étape")
        );

        let bad_duration = parse(
            r#"{"name":"x","steps":[{"type":"interval","duration":{"type":"time","value":"soon"},"target":{"type":"power_range","low":1,"high":2}}]}"#,
        );
        assert!(bad_duration.validate().unwrap().contains("Durée invalide"));

        let inverted = parse(
            r#"{"name":"x","steps":[{"type":"interval","duration":{"type":"time","value":"5min"},"target":{"type":"power_range","low":300,"high":200}}]}"#,
        );
        assert!(inverted.validate().unwrap().contains("basse"));
    }

    #[test]
    fn chart_div_escapes_json_for_a_double_quoted_attribute() {
        let ath = Athlete {
            ftp_watts: 205.0,
            hr_rest: 53.0,
            hr_max: 192.0,
            run_threshold_pace_s: 0.0,
        };
        let html = chart_div(r#"{"name":"a\"b<c"}"#, &ath);
        assert!(
            !html.contains("\"name\""),
            "raw quotes would end the attribute early"
        );
        assert!(html.contains("&quot;name&quot;"));
        assert!(html.contains("&lt;"));
    }
}
