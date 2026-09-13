use super::{escape_html, AppState};
use axum::extract::{Form, Path, State};
use axum::response::{Html, Redirect};
use crate::workout::{Duration, Target, Workout, WorkoutStep};
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
    garmin_last_error: Option<String>,
}

pub async fn list(State(state): State<AppState>) -> Html<String> {
    render_list_page(&state.pool, None).await
}

async fn render_list_page(pool: &SqlitePool, error: Option<&str>) -> Html<String> {
    let rows: Vec<WorkoutRow> = sqlx::query_as(
        "SELECT id, name, steps_json, source, created_at, garmin_last_error FROM workouts ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let items_html: String = if rows.is_empty() {
        "<p class=\"hint\">Aucune séance pour l'instant.</p>".to_string()
    } else {
        rows.iter().map(render_workout_card).collect()
    };

    let error_html = error
        .map(|e| format!(r#"<p class="form-error">{}</p>"#, escape_html(e)))
        .unwrap_or_default();
    let create_form = builder_form_html("/workouts", None);

    Html(page_shell(
        "Séances",
        &format!(
            r#"<a class="back-link" href="/">&larr; Retour au dashboard</a>
<section><h2>Mes séances</h2><ul class="workout-list">{items_html}</ul></section>
<section><h2>Nouvelle séance</h2>{error_html}{create_form}</section>"#
        ),
    ))
}

fn render_workout_card(w: &WorkoutRow) -> String {
    let parsed: Value = serde_json::from_str(&w.steps_json).unwrap_or(Value::Null);
    let distance = parsed
        .get("estimated_distance_km")
        .and_then(|d| d.as_f64())
        .map(|d| format!(" · ~{d:.1}km estimés"))
        .unwrap_or_default();
    let source_label = if w.source == "digest" { "généré par digest" } else { "créé manuellement" };
    let garmin_error = w
        .garmin_last_error
        .as_deref()
        .filter(|e| !e.is_empty())
        .map(|e| format!(r#"<p class="workout-garmin-error">Garmin: {}</p>"#, escape_html(e)))
        .unwrap_or_default();

    format!(
        r#"<li class="workout-card">
<div class="workout-card-head"><a href="/workouts/{id}">{name}</a><span class="workout-source">{source_label}</span></div>
<div class="hint">{created}{distance}</div>
{garmin_error}
</li>"#,
        id = w.id,
        name = escape_html(&w.name),
        created = escape_html(&w.created_at),
    )
}

pub async fn edit_form(State(state): State<AppState>, Path(id): Path<i64>) -> Html<String> {
    let row: Option<WorkoutRow> = sqlx::query_as(
        "SELECT id, name, steps_json, source, created_at, garmin_last_error FROM workouts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    let Some(row) = row else {
        return Html("<p>Séance introuvable. <a href=\"/workouts\">Retour</a></p>".to_string());
    };

    let form = builder_form_html(&format!("/workouts/{id}"), Some(&row.steps_json));
    let delete_form = format!(
        r#"<form method="post" action="/workouts/{id}/delete" onsubmit="return confirm('Supprimer cette séance ?');">
<button type="submit" class="danger">Supprimer</button>
</form>"#
    );

    Html(page_shell(
        "Modifier la séance",
        &format!(
            r#"<a class="back-link" href="/workouts">&larr; Retour aux séances</a>
<section><h2>Modifier: {name}</h2>{form}{delete_form}</section>"#,
            name = escape_html(&row.name)
        ),
    ))
}

#[derive(Deserialize)]
pub struct WorkoutForm {
    steps_json: String,
}

pub async fn create(State(state): State<AppState>, Form(form): Form<WorkoutForm>) -> Html<String> {
    save(&state, None, form).await
}

pub async fn update(State(state): State<AppState>, Path(id): Path<i64>, Form(form): Form<WorkoutForm>) -> Html<String> {
    save(&state, Some(id), form).await
}

async fn save(state: &AppState, id: Option<i64>, form: WorkoutForm) -> Html<String> {
    let workout: Workout = match serde_json::from_str(&form.steps_json) {
        Ok(w) => w,
        Err(err) => return error_page(id, &form.steps_json, &format!("JSON invalide : {err}")),
    };

    if let Some(err) = validate(&workout) {
        return error_page(id, &form.steps_json, &err);
    }

    let (ftp_watts,): (i64,) = sqlx::query_as("SELECT ftp_watts FROM athlete_config WHERE id = 1")
        .fetch_one(&state.pool)
        .await
        .unwrap_or((205,));
    let distance_km = workout.estimate_distance_km(ftp_watts as f64);
    let mut value = serde_json::to_value(&workout).unwrap_or(Value::Null);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("estimated_distance_km".to_string(), serde_json::json!((distance_km * 10.0).round() / 10.0));
    }
    let steps_json = value.to_string();

    match id {
        Some(id) => {
            let _ = sqlx::query("UPDATE workouts SET name = ?, steps_json = ?, updated_at = datetime('now') WHERE id = ?")
                .bind(&workout.name)
                .bind(&steps_json)
                .bind(id)
                .execute(&state.pool)
                .await;
        }
        None => {
            let _ = sqlx::query("INSERT INTO workouts (name, steps_json, source) VALUES (?, ?, 'manual')")
                .bind(&workout.name)
                .bind(&steps_json)
                .execute(&state.pool)
                .await;
        }
    }

    render_list_page(&state.pool, None).await
}

/// Re-renders the same create/edit form with the submitted (unparsed) JSON
/// preserved and a visible error banner — a deliberate exception to the
/// app's usual swallow-error-and-redirect convention, since a malformed
/// step tree must be diagnosable rather than silently dropped.
fn error_page(id: Option<i64>, submitted_steps_json: &str, error: &str) -> Html<String> {
    let action = id.map(|i| format!("/workouts/{i}")).unwrap_or_else(|| "/workouts".to_string());
    // builder_form_html tolerates invalid/partial JSON here (falls back to
    // an empty workout for any field it can't read) — the raw text is only
    // meant to survive round-tripping when it *was* structurally close.
    let form = builder_form_html(&action, Some(submitted_steps_json));
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
    let _ = sqlx::query("DELETE FROM workouts WHERE id = ?").bind(id).execute(&state.pool).await;
    Redirect::to("/workouts")
}

/// Minimal sanity checks — not a full training-science validator. Duration
/// strings that fail to parse fall back to 0 minutes in `Duration::minutes`
/// (matching digest.rs's existing permissive parser), so 0 is treated as
/// "didn't parse" here too.
fn validate(workout: &Workout) -> Option<String> {
    if workout.steps.is_empty() {
        return Some("La séance doit contenir au moins une étape.".to_string());
    }
    check_steps(&workout.steps)
}

fn check_steps(steps: &[WorkoutStep]) -> Option<String> {
    for step in steps {
        match step {
            WorkoutStep::Repeat { count, steps } => {
                if *count < 1 {
                    return Some("Le nombre de répétitions doit être au moins 1.".to_string());
                }
                if let Some(err) = check_steps(steps) {
                    return Some(err);
                }
            }
            WorkoutStep::Warmup(s) | WorkoutStep::Interval(s) | WorkoutStep::Recovery(s) | WorkoutStep::Cooldown(s) => {
                if s.duration.minutes() <= 0.0 {
                    let Duration::Time { value } = &s.duration;
                    return Some(format!("Durée invalide : « {value} »"));
                }
                if let Some(Target::PowerRange { low, high }) = &s.target {
                    if low > high {
                        return Some("La cible de puissance basse doit être inférieure ou égale à la haute.".to_string());
                    }
                }
            }
        }
    }
    None
}

const STEP_TYPE_OPTIONS: [(&str, &str); 4] = [
    ("warmup", "Échauffement"),
    ("interval", "Intervalle"),
    ("recovery", "Récupération"),
    ("cooldown", "Retour au calme"),
];

fn top_add_buttons_html() -> String {
    let mut html: String = STEP_TYPE_OPTIONS
        .iter()
        .map(|(v, label)| format!(r#"<button type="button" onclick="addTop('{v}')">+ {label}</button>"#))
        .collect();
    html += r#"<button type="button" onclick="addTop('repeat')">+ Bloc répété</button>"#;
    html
}

/// `</script` inside a note/text field would otherwise close the seed
/// `<script>` tag early — `\/` is valid JSON string escaping and browsers
/// parse it back to `/`, so this is a safe, lossless guard.
fn json_for_script(s: &str) -> String {
    s.replace("</", "<\\/")
}

fn builder_form_html(action: &str, initial_steps_json: Option<&str>) -> String {
    let initial_json = initial_steps_json.unwrap_or(r#"{"name":"","steps":[]}"#);
    let initial_value: Value = serde_json::from_str(initial_json).unwrap_or(Value::Null);
    let name = initial_value.get("name").and_then(|n| n.as_str()).unwrap_or("");

    format!(
        r#"<script>window.__initialWorkout = {initial_json};</script>
<form id="workout-form" method="post" action="{action}">
  <label>Nom de la séance
    <input type="text" id="workout-name" value="{name}" oninput="workout.name=this.value" style="max-width:100%">
  </label>
  <div id="step-builder"></div>
  <div class="step-row-add">{top_add_buttons}</div>
  <input type="hidden" name="steps_json" id="steps_json_input">
  <button type="submit">Enregistrer</button>
</form>
{builder_script}"#,
        initial_json = json_for_script(initial_json),
        action = action,
        name = escape_html(name),
        top_add_buttons = top_add_buttons_html(),
        builder_script = BUILDER_SCRIPT,
    )
}

/// Vanilla JS (no framework/build step, matching the app's existing combo
/// chart / lap-zoom style) maintaining a plain object tree and re-rendering
/// `#step-builder` on structural changes only (add/remove/move) — field
/// edits mutate `workout` directly via inline `oninput`/`onchange` so text
/// inputs never lose focus mid-edit. Deliberately caps at one level of
/// "repeat" nesting for the hand-built editor (the `Workout`/`WorkoutStep`
/// data model and `render_workout_steps` both support arbitrary nesting —
/// this is a UI scope choice for v1, not a data-model limitation).
const BUILDER_SCRIPT: &str = r#"<script>
(function() {
  var workout = window.__initialWorkout;
  if (!workout || typeof workout !== 'object') workout = {name: '', steps: []};
  if (!Array.isArray(workout.steps)) workout.steps = [];

  function newLeaf(type) {
    return {type: type, duration: {type: 'time', value: '10min'}, target: {type: 'power_range', low: 100, high: 150}, terrain: 'plat', note: ''};
  }

  function addTop(type) {
    workout.steps.push(type === 'repeat' ? {type: 'repeat', count: 3, steps: []} : newLeaf(type));
    render();
  }
  function removeTop(i) { workout.steps.splice(i, 1); render(); }
  function moveTop(i, dir) {
    var j = i + dir;
    if (j < 0 || j >= workout.steps.length) return;
    var t = workout.steps[i]; workout.steps[i] = workout.steps[j]; workout.steps[j] = t;
    render();
  }
  function addInner(topIdx, type) {
    if (!Array.isArray(workout.steps[topIdx].steps)) workout.steps[topIdx].steps = [];
    workout.steps[topIdx].steps.push(newLeaf(type));
    render();
  }
  function removeInner(topIdx, i) { workout.steps[topIdx].steps.splice(i, 1); render(); }
  function moveInner(topIdx, i, dir) {
    var arr = workout.steps[topIdx].steps;
    var j = i + dir;
    if (j < 0 || j >= arr.length) return;
    var t = arr[i]; arr[i] = arr[j]; arr[j] = t;
    render();
  }
  window.addTop = addTop;
  window.removeTop = removeTop;
  window.moveTop = moveTop;
  window.addInner = addInner;
  window.removeInner = removeInner;
  window.moveInner = moveInner;
  window.workout = workout;

  function esc(s) {
    return (s || '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }

  var STEP_TYPES = [['warmup', 'Échauffement'], ['interval', 'Intervalle'], ['recovery', 'Récupération'], ['cooldown', 'Retour au calme']];
  var TERRAINS = [['plat', 'Plat'], ['vallonné', 'Vallonné'], ['montée', 'Montée']];

  function optionsHtml(options, current) {
    return options.map(function(o) {
      return '<option value="' + o[0] + '"' + (o[0] === current ? ' selected' : '') + '>' + o[1] + '</option>';
    }).join('');
  }

  function leafFieldsHtml(step, pathExpr) {
    if (!step.target) step.target = {type: 'power_range', low: 0, high: 0};
    if (!step.duration) step.duration = {type: 'time', value: ''};
    return (
      '<select onchange="' + pathExpr + '.type=this.value">' + optionsHtml(STEP_TYPES, step.type) + '</select>' +
      '<input type="text" value="' + esc(step.duration.value) + '" placeholder="15min" oninput="' + pathExpr + '.duration.value=this.value" style="width:5.5em">' +
      '<input type="number" value="' + step.target.low + '" oninput="' + pathExpr + '.target.low=parseFloat(this.value)||0" style="width:4.5em">' +
      '<span>&ndash;</span>' +
      '<input type="number" value="' + step.target.high + '" oninput="' + pathExpr + '.target.high=parseFloat(this.value)||0" style="width:4.5em">' +
      '<span>W</span>' +
      '<select onchange="' + pathExpr + '.terrain=this.value">' + optionsHtml(TERRAINS, step.terrain) + '</select>' +
      '<input type="text" value="' + esc(step.note) + '" placeholder="note (optionnel)" oninput="' + pathExpr + '.note=this.value" style="width:9em">'
    );
  }

  function moveButtonsHtml(moveFn, removeFn, args) {
    return (
      '<span class="step-row-move">' +
      '<button type="button" onclick="' + moveFn + '(' + args + ',-1)">&uarr;</button>' +
      '<button type="button" onclick="' + moveFn + '(' + args + ',1)">&darr;</button>' +
      '<button type="button" onclick="' + removeFn + '(' + args + ')">&times;</button>' +
      '</span>'
    );
  }

  function innerRowHtml(step, topIdx, i) {
    var pathExpr = 'workout.steps[' + topIdx + '].steps[' + i + ']';
    return '<div class="step-row step-row-nested">' + leafFieldsHtml(step, pathExpr) +
      moveButtonsHtml('moveInner', 'removeInner', topIdx + ',' + i) + '</div>';
  }

  function innerAddButtonsHtml(topIdx) {
    return STEP_TYPES.map(function(t) {
      return '<button type="button" onclick="addInner(' + topIdx + ',\'' + t[0] + '\')">+ ' + t[1] + '</button>';
    }).join('');
  }

  function topRowHtml(step, i) {
    if (step.type === 'repeat') {
      if (!Array.isArray(step.steps)) step.steps = [];
      var inner = step.steps.map(function(s, j) { return innerRowHtml(s, i, j); }).join('');
      return (
        '<div class="step-row step-row-repeat">' +
        '<div class="step-row-head"><strong>Répéter</strong> x' +
        '<input type="number" min="1" value="' + step.count + '" oninput="workout.steps[' + i + '].count=parseInt(this.value)||1" style="width:3.5em">' +
        moveButtonsHtml('moveTop', 'removeTop', String(i)) +
        '</div>' +
        '<div class="step-row-inner">' + inner + '<div class="step-row-add">' + innerAddButtonsHtml(i) + '</div></div>' +
        '</div>'
      );
    }
    return '<div class="step-row">' + leafFieldsHtml(step, 'workout.steps[' + i + ']') +
      moveButtonsHtml('moveTop', 'removeTop', String(i)) + '</div>';
  }

  function render() {
    var html = workout.steps.map(function(s, i) { return topRowHtml(s, i); }).join('');
    document.getElementById('step-builder').innerHTML = html || '<p class="hint">Aucune étape — ajoute-en une ci-dessous.</p>';
  }

  var nameInput = document.getElementById('workout-name');
  if (nameInput) nameInput.addEventListener('input', function() { workout.name = this.value; });

  var form = document.getElementById('workout-form');
  if (form) {
    form.addEventListener('submit', function() {
      document.getElementById('steps_json_input').value = JSON.stringify(workout);
    });
  }

  render();
})();
</script>"#;

fn page_shell(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="fr">
<head>
<meta charset="utf-8">
<title>CyclAI — {title}</title>
<style>{base_css}
.workout-list {{ list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 0.6rem; }}
.workout-card {{ border: 1px solid var(--border); border-radius: 8px; padding: 0.7rem 0.9rem; }}
.workout-card-head {{ display: flex; justify-content: space-between; align-items: center; gap: 0.6rem; }}
.workout-card-head a {{ font-weight: 600; color: var(--text); text-decoration: none; }}
.workout-card-head a:hover {{ color: var(--accent); }}
.workout-source {{ font-size: 0.75rem; color: var(--text-muted); background: var(--bg); border-radius: 4px; padding: 0.1rem 0.5rem; white-space: nowrap; }}
.workout-garmin-error {{ color: #dc2626; font-size: 0.82rem; margin: 0.3rem 0 0; }}
.step-row {{ display: flex; flex-wrap: wrap; align-items: center; gap: 0.4rem; padding: 0.5rem; border: 1px solid var(--border); border-radius: 6px; margin-bottom: 0.5rem; }}
.step-row select {{ border: 1px solid var(--border); border-radius: 6px; padding: 0.25rem 0.4rem; font-size: 0.85rem; font-family: inherit; }}
.step-row input[type="text"] {{ max-width: none; }}
.step-row input[type="number"] {{ max-width: none; }}
.step-row-move {{ margin-left: auto; display: flex; gap: 0.2rem; }}
.step-row-move button, .step-row-add button {{ padding: 0.2rem 0.5rem; font-size: 0.78rem; margin: 0; }}
.step-row-repeat {{ flex-direction: column; align-items: stretch; background: var(--bg); }}
.step-row-head {{ display: flex; align-items: center; gap: 0.4rem; }}
.step-row-inner {{ margin-top: 0.5rem; padding-left: 1rem; border-left: 2px solid var(--border); }}
.step-row-nested {{ background: var(--card-bg); }}
.step-row-add {{ display: flex; flex-wrap: wrap; gap: 0.4rem; margin: 0.4rem 0 0.8rem; }}
.form-error {{ color: #b91c1c; background: #fef2f2; border: 1px solid #fecaca; border-radius: 6px; padding: 0.6rem 0.8rem; margin-bottom: 0.8rem; }}
.danger {{ background: #dc2626; }}
.danger:hover {{ background: #b91c1c; }}
</style>
</head>
<body class="workout-page">
<h1>CyclAI</h1>
{body}
</body>
</html>"#,
        base_css = super::BASE_CSS,
    )
}
