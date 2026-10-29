//! Pages to schedule a session on a calendar day: `/planned/new` (blank, or
//! pre-filled from a template with `?from=<id>`, optionally for `?date=`),
//! `/planned/{id}` to edit/delete. Reuses the template designer; the planned
//! session keeps its own copy of the steps (see `planned`).

use super::workout::{builder_form_html, fetch_athlete, page_shell, sport_select_html};
use super::{AppState, escape_html};
use crate::planned::Sport;
use crate::workout::Athlete;
use axum::extract::{Form, Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub struct NewQuery {
    date: Option<String>,
    from: Option<i64>,
}

/// Everything the form pages need to (re)render, including after an error.
struct FormView<'a> {
    heading: &'a str,
    action: String,
    steps_json: Option<&'a str>,
    date: &'a str,
    notes: &'a str,
    sport: Sport,
    from_workout_id: Option<i64>,
    offer_template_checkbox: bool,
    delete_id: Option<i64>,
    error: Option<&'a str>,
}

fn render_form(v: &FormView, ath: &Athlete) -> Html<String> {
    let template_checkbox = if v.offer_template_checkbox {
        r#"<label class="inline-check"><input type="checkbox" name="save_template" value="1"> Enregistrer aussi comme modèle de séance réutilisable</label>"#
    } else {
        ""
    };
    let from_field = v
        .from_workout_id
        .map(|id| format!(r#"<input type="hidden" name="from_workout_id" value="{id}">"#))
        .unwrap_or_default();
    let extra = format!(
        r#"{sport_select}
  <label>Date <input type="date" name="planned_date" value="{date}" required></label>
  <label>Notes (optionnel)
    <textarea name="notes" rows="2" placeholder="ex: sortie avec le club, à faire si la météo le permet...">{notes}</textarea>
  </label>
  {from_field}{template_checkbox}"#,
        sport_select = sport_select_html(v.sport),
        date = escape_html(v.date),
        notes = escape_html(v.notes),
    );
    let form = builder_form_html(&v.action, v.steps_json, ath, &extra);
    let error_html = v
        .error
        .map(|e| format!(r#"<p class="form-error">{}</p>"#, escape_html(e)))
        .unwrap_or_default();
    let delete_html = v
        .delete_id
        .map(|id| {
            format!(
                r#"<form method="post" action="/planned/{id}/delete" onsubmit="return confirm('Retirer cette séance du calendrier ?');">
<button type="submit" class="danger">Retirer du calendrier</button>
</form>"#
            )
        })
        .unwrap_or_default();
    Html(page_shell(
        "Séance planifiée",
        &format!(
            r#"<a class="back-link" href="/">&larr; Retour au calendrier</a>
<section><h2>{heading}</h2>{error_html}{form}{delete_html}</section>"#,
            heading = escape_html(v.heading),
        ),
    ))
}

pub async fn new_form(State(state): State<AppState>, Query(q): Query<NewQuery>) -> Html<String> {
    let ath = fetch_athlete(&state.pool).await;
    let date = q
        .date
        .as_deref()
        .and_then(crate::planned::parse_date)
        .unwrap_or_else(|| chrono::Utc::now().date_naive())
        .to_string();

    let template: Option<(i64, String, String)> = match q.from {
        Some(id) => sqlx::query_as("SELECT id, steps_json, sport FROM workouts WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let (from_id, steps, sport) = match &template {
        Some((id, steps, sport)) => (Some(*id), Some(steps.as_str()), Sport::from_db(sport)),
        None => (None, None, Sport::default()),
    };
    render_form(
        &FormView {
            heading: if template.is_some() {
                "Planifier une séance (copie du modèle)"
            } else {
                "Nouvelle séance du jour"
            },
            action: "/planned".to_string(),
            steps_json: steps,
            date: &date,
            notes: "",
            sport,
            from_workout_id: from_id,
            offer_template_checkbox: template.is_none(),
            delete_id: None,
            error: None,
        },
        &ath,
    )
}

#[derive(Deserialize)]
pub struct PlannedForm {
    steps_json: String,
    planned_date: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    sport: String,
    #[serde(default)]
    save_template: String,
    #[serde(default)]
    from_workout_id: String,
}

/// Validates the posted form into (date, prepared workout), or an error message.
fn read_form(
    form: &PlannedForm,
    ath: &Athlete,
) -> Result<(chrono::NaiveDate, crate::planned::Prepared, Sport), String> {
    let date = crate::planned::parse_date(&form.planned_date)
        .ok_or_else(|| "Date invalide.".to_string())?;
    let value: Value =
        serde_json::from_str(&form.steps_json).map_err(|e| format!("JSON invalide : {e}"))?;
    let sport = Sport::parse(&form.sport).unwrap_or_default();
    let prepared = crate::planned::prepare(&value, ath, sport)?;
    Ok((date, prepared, sport))
}

pub async fn create(State(state): State<AppState>, Form(form): Form<PlannedForm>) -> Response {
    let ath = fetch_athlete(&state.pool).await;
    let (date, prepared, sport) = match read_form(&form, &ath) {
        Ok(ok) => ok,
        Err(err) => {
            let from_id = form.from_workout_id.parse().ok();
            return render_form(
                &FormView {
                    heading: "Corriger la séance",
                    action: "/planned".to_string(),
                    steps_json: Some(&form.steps_json),
                    date: &form.planned_date,
                    notes: &form.notes,
                    sport: Sport::parse(&form.sport).unwrap_or_default(),
                    from_workout_id: from_id,
                    offer_template_checkbox: from_id.is_none(),
                    delete_id: None,
                    error: Some(&err),
                },
                &ath,
            )
            .into_response();
        }
    };

    let mut template_id: Option<i64> = form.from_workout_id.parse().ok();
    if !form.save_template.is_empty() {
        template_id = sqlx::query(
            "INSERT INTO workouts (name, steps_json, source, sport) VALUES (?, ?, 'manual', ?)",
        )
        .bind(&prepared.name)
        .bind(&prepared.steps_json)
        .bind(sport.as_str())
        .execute(&state.pool)
        .await
        .ok()
        .map(|r| r.last_insert_rowid());
    }
    if let Err(err) = crate::planned::insert(
        &state.pool,
        date,
        &prepared,
        Some(form.notes.as_str()),
        template_id,
        "manual",
        sport,
    )
    .await
    {
        tracing::error!(?err, "failed to store planned workout");
    }
    Redirect::to("/").into_response()
}

pub async fn edit_form(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let ath = fetch_athlete(&state.pool).await;
    let Ok(Some(p)) = crate::planned::get(&state.pool, id).await else {
        return Html("<p>Séance planifiée introuvable. <a href=\"/\">Retour</a></p>".to_string())
            .into_response();
    };
    render_form(
        &FormView {
            heading: &format!("Séance du {}", p.planned_date),
            action: format!("/planned/{id}"),
            steps_json: Some(&p.steps_json),
            date: &p.planned_date,
            notes: p.notes.as_deref().unwrap_or(""),
            sport: Sport::from_db(&p.sport),
            from_workout_id: None,
            offer_template_checkbox: false,
            delete_id: Some(id),
            error: None,
        },
        &ath,
    )
    .into_response()
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Form(form): Form<PlannedForm>,
) -> Response {
    let ath = fetch_athlete(&state.pool).await;
    match read_form(&form, &ath) {
        Ok((date, prepared, sport)) => {
            if let Err(err) = crate::planned::update(
                &state.pool,
                id,
                date,
                &prepared,
                Some(form.notes.as_str()),
                sport,
            )
            .await
            {
                tracing::error!(?err, planned_id = id, "failed to update planned workout");
            }
            Redirect::to("/").into_response()
        }
        Err(err) => render_form(
            &FormView {
                heading: "Corriger la séance",
                action: format!("/planned/{id}"),
                steps_json: Some(&form.steps_json),
                date: &form.planned_date,
                notes: &form.notes,
                sport: Sport::parse(&form.sport).unwrap_or_default(),
                from_workout_id: None,
                offer_template_checkbox: false,
                delete_id: Some(id),
                error: Some(&err),
            },
            &ath,
        )
        .into_response(),
    }
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> Redirect {
    if let Err(err) = crate::planned::delete(&state.pool, id).await {
        tracing::error!(?err, planned_id = id, "failed to delete planned workout");
    }
    Redirect::to("/")
}
