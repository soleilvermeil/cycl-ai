//! Planned (scheduled) workouts: a session placed on a calendar day.
//!
//! Shared by the web pages (`web::planned`, the calendar, the designer) and
//! the JSON API, so every entry point validates and stores the same way. A
//! planned session owns a snapshot of its steps — it may come from a template
//! (`workouts`, tracked by `workout_id` for information only) or be unique to
//! its day.

use crate::workout::{Athlete, Workout};
use chrono::NaiveDate;
use serde_json::Value;
use sqlx::SqlitePool;

/// Sport of a session. Stored as its `as_str` in `workouts.sport` /
/// `planned_workouts.sport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sport {
    #[default]
    Cycling,
    Running,
    Other,
}

impl Sport {
    pub const ALL: [Sport; 3] = [Sport::Cycling, Sport::Running, Sport::Other];

    pub fn as_str(self) -> &'static str {
        match self {
            Sport::Cycling => "cycling",
            Sport::Running => "running",
            Sport::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sport::Cycling => "Cyclisme",
            Sport::Running => "Course à pied",
            Sport::Other => "Autre",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Sport::Cycling => "🚴",
            Sport::Running => "🏃",
            Sport::Other => "📅",
        }
    }

    pub fn parse(s: &str) -> Option<Sport> {
        Sport::ALL.into_iter().find(|sp| sp.as_str() == s.trim())
    }

    /// Lenient read of a stored value (unknown -> cycling).
    pub fn from_db(s: &str) -> Sport {
        Sport::parse(s).unwrap_or_default()
    }
}

#[derive(sqlx::FromRow, Clone)]
pub struct Planned {
    pub id: i64,
    pub planned_date: String,
    pub name: String,
    pub steps_json: String,
    pub notes: Option<String>,
    pub workout_id: Option<i64>,
    pub source: String,
    pub sport: String,
}

/// A validated workout ready to store: name + the full Workout JSON with the
/// estimated distance filled in (same shape as `workouts.steps_json`).
#[derive(Debug)]
pub struct Prepared {
    pub name: String,
    pub steps_json: String,
}

/// `YYYY-MM-DD` only — the one format stored and compared as text.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// Parses + validates a Workout JSON value (from the designer or the API)
/// and computes its distance estimate (see below). The error is a user-facing
/// message.
pub fn prepare(workout: &Value, athlete: &Athlete, sport: Sport) -> Result<Prepared, String> {
    let workout: Workout =
        serde_json::from_value(workout.clone()).map_err(|e| format!("JSON invalide : {e}"))?;
    if workout.name.trim().is_empty() {
        return Err("La séance doit avoir un nom.".to_string());
    }
    if let Some(err) = workout.validate() {
        return Err(err);
    }
    // Cycling: speed model from power/zone targets. Running: from the pace
    // targets. Anything else has no distance.
    let distance_km = match sport {
        Sport::Cycling => Some(workout.estimate_distance_km(athlete)),
        Sport::Running => workout.estimate_run_distance_km(athlete),
        Sport::Other => None,
    };
    let mut value = serde_json::to_value(&workout).map_err(|e| e.to_string())?;
    if let Some(obj) = value.as_object_mut() {
        match distance_km {
            Some(km) => {
                obj.insert(
                    "estimated_distance_km".to_string(),
                    serde_json::json!((km * 10.0).round() / 10.0),
                );
            }
            None => {
                obj.remove("estimated_distance_km");
            }
        }
    }
    Ok(Prepared {
        name: workout.name,
        steps_json: value.to_string(),
    })
}

pub async fn insert(
    pool: &SqlitePool,
    date: NaiveDate,
    prepared: &Prepared,
    notes: Option<&str>,
    workout_id: Option<i64>,
    source: &str,
    sport: Sport,
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO planned_workouts (planned_date, name, steps_json, notes, workout_id, source, sport) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(date.to_string())
    .bind(&prepared.name)
    .bind(&prepared.steps_json)
    .bind(notes.map(str::trim).filter(|n| !n.is_empty()))
    .bind(workout_id)
    .bind(source)
    .bind(sport.as_str())
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

pub async fn update(
    pool: &SqlitePool,
    id: i64,
    date: NaiveDate,
    prepared: &Prepared,
    notes: Option<&str>,
    sport: Sport,
) -> anyhow::Result<bool> {
    let result = sqlx::query(
        "UPDATE planned_workouts SET planned_date = ?, name = ?, steps_json = ?, notes = ?, sport = ?, \
         updated_at = datetime('now') WHERE id = ?",
    )
    .bind(date.to_string())
    .bind(&prepared.name)
    .bind(&prepared.steps_json)
    .bind(notes.map(str::trim).filter(|n| !n.is_empty()))
    .bind(sport.as_str())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete(pool: &SqlitePool, id: i64) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM planned_workouts WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn get(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Planned>> {
    Ok(sqlx::query_as(
        "SELECT id, planned_date, name, steps_json, notes, workout_id, source, sport FROM planned_workouts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Planned sessions with `from <= date < to` (end exclusive), oldest first.
pub async fn list_between(
    pool: &SqlitePool,
    from: NaiveDate,
    to: NaiveDate,
) -> anyhow::Result<Vec<Planned>> {
    Ok(sqlx::query_as(
        "SELECT id, planned_date, name, steps_json, notes, workout_id, source, sport FROM planned_workouts \
         WHERE planned_date >= ? AND planned_date < ? ORDER BY planned_date, id",
    )
    .bind(from.to_string())
    .bind(to.to_string())
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn athlete() -> Athlete {
        Athlete {
            ftp_watts: 205.0,
            hr_rest: 53.0,
            hr_max: 192.0,
            run_threshold_pace_s: 0.0,
        }
    }

    fn sample() -> Value {
        json!({"name": "Seuil 2x10", "steps": [
            {"type": "warmup", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 120, "high": 150}},
            {"type": "repeat", "count": 2, "steps": [
                {"type": "interval", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 195, "high": 205}},
                {"type": "recovery", "duration": {"type": "time", "value": "5min"}, "target": {"type": "power_range", "low": 100, "high": 120}}
            ]}
        ]})
    }

    #[test]
    fn sport_parses_known_values_only_and_distance_is_cycling_only() {
        assert_eq!(Sport::parse("running"), Some(Sport::Running));
        assert_eq!(Sport::parse(" cycling "), Some(Sport::Cycling));
        assert_eq!(Sport::parse("swimming"), None);
        assert_eq!(Sport::from_db("???"), Sport::Cycling);

        let ride = prepare(&sample(), &athlete(), Sport::Cycling).unwrap();
        let run = prepare(&sample(), &athlete(), Sport::Running).unwrap();
        assert!(ride.steps_json.contains("estimated_distance_km"));
        assert!(!run.steps_json.contains("estimated_distance_km"));
    }

    #[test]
    fn prepare_validates_and_adds_distance_estimate() {
        let p = prepare(&sample(), &athlete(), Sport::Cycling).unwrap();
        let v: Value = serde_json::from_str(&p.steps_json).unwrap();
        assert_eq!(p.name, "Seuil 2x10");
        assert!(v["estimated_distance_km"].as_f64().unwrap() > 0.0);

        assert!(
            prepare(
                &json!({"name": "", "steps": []}),
                &athlete(),
                Sport::Cycling
            )
            .is_err()
        );
        assert!(
            prepare(
                &json!({"name": "x", "steps": []}),
                &athlete(),
                Sport::Cycling
            )
            .unwrap_err()
            .contains("au moins une étape")
        );
        assert!(
            prepare(&json!({"nope": 1}), &athlete(), Sport::Cycling)
                .unwrap_err()
                .contains("JSON invalide")
        );
    }

    #[tokio::test]
    async fn planned_session_is_independent_of_its_template() {
        let pool = test_pool().await;
        let prepared = prepare(&sample(), &athlete(), Sport::Cycling).unwrap();
        let tpl = sqlx::query("INSERT INTO workouts (name, steps_json) VALUES (?, ?)")
            .bind(&prepared.name)
            .bind(&prepared.steps_json)
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
        let day = parse_date("2026-10-12").unwrap();
        let id = insert(
            &pool,
            day,
            &prepared,
            Some("  jambes lourdes ? "),
            Some(tpl),
            "manual",
            Sport::Cycling,
        )
        .await
        .unwrap();
        // A second, template-less session on the same day is fine.
        insert(&pool, day, &prepared, None, None, "api", Sport::Running)
            .await
            .unwrap();

        // Editing / deleting the template leaves the planned copy alone.
        sqlx::query("UPDATE workouts SET name = 'renamed', steps_json = '{}' WHERE id = ?")
            .bind(tpl)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM workouts WHERE id = ?")
            .bind(tpl)
            .execute(&pool)
            .await
            .unwrap();

        let planned = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(planned.name, "Seuil 2x10");
        assert_eq!(
            planned.workout_id, None,
            "link cleared when the template is deleted"
        );
        assert_eq!(planned.notes.as_deref(), Some("jambes lourdes ?"));
        assert!(planned.steps_json.contains("interval"));

        let in_window = list_between(
            &pool,
            parse_date("2026-10-12").unwrap(),
            parse_date("2026-10-13").unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(in_window.len(), 2);
        let outside = list_between(
            &pool,
            parse_date("2026-10-13").unwrap(),
            parse_date("2026-10-20").unwrap(),
        )
        .await
        .unwrap();
        assert!(outside.is_empty());

        assert_eq!(
            in_window
                .iter()
                .map(|p| p.sport.as_str())
                .collect::<Vec<_>>(),
            vec!["cycling", "running"]
        );

        assert!(delete(&pool, id).await.unwrap());
        assert!(!delete(&pool, id).await.unwrap());
    }
}
