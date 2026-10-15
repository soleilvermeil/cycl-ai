use crate::llm::LlmClient;
use serde_json::{json, Value};
use sqlx::SqlitePool;

/// Seeded into `settings.digest_system_prompt` on first boot — editable
/// afterwards from the dashboard's /settings page, this constant is no
/// longer read at request time (see `generate`'s `system_prompt` parameter).
pub(crate) const DEFAULT_SYSTEM_PROMPT: &str = r#"Tu es un coach cyclisme. Tu reçois un résumé structuré de la semaine d'entraînement d'un cycliste (charge, CTL/ATL/TSB, répartition zones, éventuellement une note libre) ainsi que son profil complet.

Réponds UNIQUEMENT avec un objet JSON valide (aucun texte avant ou après), avec exactement cette forme :
{
  "workout": {
    "name": "string",
    "steps": [
      {"type": "warmup"|"interval"|"recovery"|"cooldown", "duration": {"type": "time", "value": "Xmin"}, "target": {"type": "power_range", "low": N, "high": N}, "terrain": "plat"|"vallonné"|"montée", "note": "string optionnel"},
      {"type": "repeat", "count": N, "steps": [ ... steps imbriqués, y compris repeat imbriqués ... ]}
    ]
  },
  "notes": "bilan qualitatif en français, 2-4 phrases, sur pourquoi cette séance est adaptée à la semaine et au profil"
}

Les puissances cibles (low/high) sont en watts, calibrées sur le FTP donné dans le profil. Le champ "terrain" par défaut est "plat" si non précisé."#;

fn structured_summary(
    ftp_watts: f64,
    tss_28d: f64,
    ctl: f64,
    atl: f64,
    tsb: f64,
    zone_secs: [i64; 7],
    free_text: Option<&str>,
) -> Value {
    let total: i64 = zone_secs.iter().sum();
    let zone_pct: Vec<f64> = zone_secs
        .iter()
        .map(|s| if total > 0 { *s as f64 / total as f64 * 100.0 } else { 0.0 })
        .collect();

    json!({
        "ftp_watts": ftp_watts,
        "tss_28d": tss_28d,
        "ctl": ctl,
        "atl": atl,
        "tsb": tsb,
        "zone_pct_z1_to_z7": zone_pct,
        "free_text": free_text,
    })
}

/// Models sometimes wrap "JSON only" responses in a ```json ... ``` fence
/// despite instructions not to. Strip it before parsing rather than fail.
fn strip_code_fence(s: &str) -> &str {
    let s = s.trim();
    let Some(rest) = s.strip_prefix("```") else {
        return s;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    let rest = rest.trim_start_matches(['\n', '\r']);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

/// Same duration grammar as the session designer and planner (15min, 90s,
/// 1h, 1h30, 1:30), so a digest workout is measured exactly like one that is
/// later planned from it.
fn parse_minutes(duration: &Value) -> f64 {
    crate::workout::parse_minutes(duration.get("value").and_then(|v| v.as_str()).unwrap_or("0min"))
}

/// Rough speed model calibrated on typical road-cycling paces: ~30km/h
/// Z2-Z3 on flat, 12-15km/h climbing. Climbing speed is dominated by
/// gradient, not power zone, so it's flattened to one value.
fn speed_kmh(power_frac_ftp: Option<f64>, terrain: &str) -> f64 {
    if terrain == "montée" {
        return 13.5;
    }
    match power_frac_ftp {
        Some(f) if f < 0.55 => 22.0,
        Some(f) if f < 0.75 => 28.0,
        Some(f) if f < 0.90 => 30.0,
        Some(f) if f < 1.05 => 32.0,
        Some(f) if f < 1.20 => 34.0,
        Some(_) => 36.0,
        None => 25.0,
    }
}

fn step_power_frac(step: &Value, ftp: f64) -> Option<f64> {
    let target = step.get("target")?;
    let low = target.get("low")?.as_f64()?;
    let high = target.get("high").and_then(|h| h.as_f64()).unwrap_or(low);
    Some((low + high) / 2.0 / ftp)
}

/// Sums estimated distance across steps, expanding "repeat" nodes by their
/// count. Kept deterministic in Rust rather than trusted from the LLM,
/// since arithmetic over nested repeats is exactly the kind of thing an
/// LLM gets subtly wrong.
fn estimate_distance_km(steps: &[Value], ftp: f64) -> f64 {
    let mut total = 0.0;
    for step in steps {
        let step_type = step.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if step_type == "repeat" {
            let count = step.get("count").and_then(|c| c.as_u64()).unwrap_or(1);
            let inner = step
                .get("steps")
                .and_then(|s| s.as_array())
                .cloned()
                .unwrap_or_default();
            total += count as f64 * estimate_distance_km(&inner, ftp);
        } else {
            let Some(duration) = step.get("duration") else {
                continue;
            };
            let dur_min = parse_minutes(duration);
            let terrain = step.get("terrain").and_then(|t| t.as_str()).unwrap_or("plat");
            let frac = step_power_frac(step, ftp);
            total += dur_min / 60.0 * speed_kmh(frac, terrain);
        }
    }
    total
}

async fn fetch_context(pool: &SqlitePool) -> anyhow::Result<(f64, f64, f64, f64, f64, [i64; 7])> {
    let (ftp_watts,): (i64,) = sqlx::query_as("SELECT ftp_watts FROM athlete_config WHERE id = 1")
        .fetch_one(pool)
        .await?;

    let latest: Option<(f64, f64, f64)> =
        sqlx::query_as("SELECT ctl, atl, tsb FROM daily_load ORDER BY date DESC LIMIT 1")
            .fetch_optional(pool)
            .await?;
    let (ctl, atl, tsb) = latest.unwrap_or((0.0, 0.0, 0.0));

    let (tss_28d,): (f64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(tss), 0.0) FROM daily_load WHERE date >= date('now', '-27 days')",
    )
    .fetch_one(pool)
    .await?;

    let zones: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT COALESCE(SUM(zone1_s),0), COALESCE(SUM(zone2_s),0), COALESCE(SUM(zone3_s),0),
               COALESCE(SUM(zone4_s),0), COALESCE(SUM(zone5_s),0), COALESCE(SUM(zone6_s),0), COALESCE(SUM(zone7_s),0)
        FROM activities WHERE date(started_at) >= date('now', '-27 days')
        "#,
    )
    .fetch_one(pool)
    .await?;

    Ok((
        ftp_watts as f64,
        tss_28d,
        ctl,
        atl,
        tsb,
        [zones.0, zones.1, zones.2, zones.3, zones.4, zones.5, zones.6],
    ))
}

/// The user message sent to the LLM for a digest (profile + structured
/// weekly summary), plus the summary itself for storage and the FTP used for
/// the distance estimate. Shared by `generate` (API) and the downloadable
/// report (`report::render`), so both feed an assistant the exact same data.
async fn build_prompt(
    pool: &SqlitePool,
    free_text: Option<&str>,
    profile: &str,
) -> anyhow::Result<(f64, Value, String)> {
    let (ftp_watts, tss_28d, ctl, atl, tsb, zone_secs) = fetch_context(pool).await?;
    let summary = structured_summary(ftp_watts, tss_28d, ctl, atl, tsb, zone_secs, free_text);
    let user_message = format!(
        "Profil du cycliste:\n{profile}\n\nRésumé structuré de la semaine:\n{}",
        serde_json::to_string_pretty(&summary)?
    );
    Ok((ftp_watts, summary, user_message))
}

/// User message only — see `build_prompt`; used by the no-API report download.
pub async fn build_user_message(
    pool: &SqlitePool,
    free_text: Option<&str>,
    profile: &str,
) -> anyhow::Result<String> {
    Ok(build_prompt(pool, free_text, profile).await?.2)
}

/// Generates a weekly digest: bundles the current training-load context
/// (deterministic, from DB) with the athlete's full profile, calls the LLM
/// for a structured workout recommendation, then augments the response
/// with a deterministically-computed estimated distance before storing it.
pub async fn generate<C: LlmClient>(
    pool: &SqlitePool,
    client: &C,
    free_text: Option<&str>,
    profile: &str,
    system_prompt: &str,
) -> anyhow::Result<()> {
    let (ftp_watts, summary, user_message) = build_prompt(pool, free_text, profile).await?;

    let raw = client.complete(system_prompt, &user_message).await?;
    let raw = strip_code_fence(&raw);

    // Captured when the LLM's "workout" object also parses as a valid
    // `workout::Workout` — persisted into `workouts` below (additive: does
    // not change what's stored in/read from `digests`) so AI-generated
    // sessions show up on the /workouts page like hand-built ones. A workout that's valid JSON but doesn't match the
    // typed schema is skipped here (logged), not a hard failure — the
    // digest itself still stores and displays fine either way.
    let mut workout_to_persist: Option<Value> = None;

    let (response_json, response_text) = match serde_json::from_str::<Value>(raw) {
        Ok(mut parsed) => {
            if let Some(steps) = parsed
                .get("workout")
                .and_then(|w| w.get("steps"))
                .and_then(|s| s.as_array())
                .cloned()
            {
                let distance_km = estimate_distance_km(&steps, ftp_watts);
                if let Some(workout) = parsed.get_mut("workout").and_then(|w| w.as_object_mut()) {
                    workout.insert(
                        "estimated_distance_km".to_string(),
                        json!((distance_km * 10.0).round() / 10.0),
                    );
                }
            }
            if let Some(workout_value) = parsed.get("workout").cloned() {
                match serde_json::from_value::<crate::workout::Workout>(workout_value.clone()) {
                    // Same checks as the designer / planner: a workout that
                    // can't be edited or planned later isn't saved as a template.
                    Ok(w) => match w.validate() {
                        None => workout_to_persist = Some(workout_value),
                        Some(reason) => tracing::warn!(reason, "digest workout failed validation, skipping workouts insert"),
                    },
                    Err(err) => {
                        tracing::warn!(?err, "digest workout JSON didn't match the Workout schema, skipping workouts insert");
                    }
                }
            }
            let text = parsed
                .get("notes")
                .and_then(|n| n.as_str())
                .unwrap_or(raw)
                .to_string();
            (serde_json::to_string(&parsed)?, text)
        }
        Err(_) => (
            json!({"parse_error": true, "raw": raw}).to_string(),
            raw.to_string(),
        ),
    };

    let insert_result = sqlx::query(
        r#"
        INSERT INTO digests (free_text, structured_summary_json, llm_response_json, llm_response_text)
        VALUES (?, ?, ?, ?)
        "#,
    )
    .bind(free_text)
    .bind(summary.to_string())
    .bind(response_json)
    .bind(response_text)
    .execute(pool)
    .await?;

    if let Some(workout_value) = workout_to_persist {
        let name = workout_value.get("name").and_then(|n| n.as_str()).unwrap_or("Séance").to_string();
        sqlx::query("INSERT INTO workouts (name, steps_json, source, digest_id) VALUES (?, ?, 'digest', ?)")
            .bind(name)
            .bind(workout_value.to_string())
            .bind(insert_result.last_insert_rowid())
            .execute(pool)
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_workout_is_valid_for_the_planner_and_accepts_long_durations() {
        let w = json!({"name": "Sweet spot", "steps": [
            {"type": "warmup", "duration": {"type": "time", "value": "15min"}, "target": {"type": "power_range", "low": 120, "high": 150}, "terrain": "plat"},
            {"type": "repeat", "count": 2, "steps": [
                {"type": "interval", "duration": {"type": "time", "value": "1h"}, "target": {"type": "power_range", "low": 185, "high": 195}, "terrain": "vallonné", "note": "régulier"},
                {"type": "recovery", "duration": {"type": "time", "value": "5min"}, "target": {"type": "power_range", "low": 100, "high": 120}}
            ]}
        ]});
        // What the digest stores must go through the planner unchanged...
        let ath = crate::workout::Athlete { ftp_watts: 205.0, hr_rest: 53.0, hr_max: 192.0, run_threshold_pace_s: 0.0 };
        let prepared = crate::planned::prepare(&w, &ath, crate::planned::Sport::Cycling).expect("planner accepts the digest format");
        assert_eq!(prepared.name, "Sweet spot");
        // ...and "1h" must count (the old digest parser read it as 0 minutes).
        let km = estimate_distance_km(w["steps"].as_array().unwrap(), 205.0);
        assert!(km > 60.0, "2x1h at ~30 km/h should be well over 60 km, got {km}");
    }

    #[test]
    fn distance_estimate_matches_readme_example() {
        // 2x(3x(3min sous-seuil + 1min sur-seuil) + 3min recup), warmup 15min, cooldown 10min
        let ftp = 205.0;
        let steps = json!([
            {"type": "warmup", "duration": {"type": "time", "value": "15min"}, "target": {"type": "power_range", "low": 123, "high": 154}},
            {"type": "repeat", "count": 2, "steps": [
                {"type": "repeat", "count": 3, "steps": [
                    {"type": "interval", "duration": {"type": "time", "value": "3min"}, "target": {"type": "power_range", "low": 195, "high": 195}},
                    {"type": "interval", "duration": {"type": "time", "value": "1min"}, "target": {"type": "power_range", "low": 215, "high": 220}}
                ]},
                {"type": "recovery", "duration": {"type": "time", "value": "3min"}, "target": {"type": "power_range", "low": 100, "high": 123}}
            ]},
            {"type": "cooldown", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 100, "high": 123}}
        ]);
        let steps = steps.as_array().unwrap();

        // Total duration: 15 + 2*(3*(3+1)+3) + 10 = 15 + 2*15 + 10 = 55min
        let distance = estimate_distance_km(steps, ftp);
        assert!(distance > 0.0);
        // Sanity: 55min at ~25-32km/h average should land well under 30km.
        assert!(distance < 30.0);
    }

    #[test]
    fn strips_markdown_code_fence_around_json() {
        assert_eq!(strip_code_fence("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_code_fence("```\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_code_fence("{\"a\": 1}"), "{\"a\": 1}");
    }

    #[test]
    fn climbing_terrain_uses_flat_climb_speed_regardless_of_power() {
        let steps = json!([
            {"type": "interval", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 300, "high": 300}, "terrain": "montée"}
        ]);
        let distance = estimate_distance_km(steps.as_array().unwrap(), 205.0);
        // 10min at 13.5km/h = 2.25km
        assert!((distance - 2.25).abs() < 1e-9);
    }

    #[test]
    fn parses_minute_and_second_durations() {
        assert!((parse_minutes(&json!({"value": "15min"})) - 15.0).abs() < 1e-9);
        assert!((parse_minutes(&json!({"value": "90sec"})) - 1.5).abs() < 1e-9);
        assert!((parse_minutes(&json!({"value": "30s"})) - 0.5).abs() < 1e-9);
    }

    struct FakeClient {
        response: String,
    }

    impl crate::llm::LlmClient for FakeClient {
        async fn complete(&self, _system: &str, _user_message: &str) -> anyhow::Result<String> {
            Ok(self.response.clone())
        }
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn successful_workout_response_also_persists_to_workouts_table() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 205, 192, 53)")
            .execute(&pool)
            .await
            .unwrap();

        let response = json!({
            "workout": {
                "name": "Séance seuil",
                "steps": [
                    {"type": "warmup", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 100, "high": 130}},
                    {"type": "interval", "duration": {"type": "time", "value": "20min"}, "target": {"type": "power_range", "low": 180, "high": 190}},
                    {"type": "cooldown", "duration": {"type": "time", "value": "10min"}, "target": {"type": "power_range", "low": 100, "high": 130}}
                ]
            },
            "notes": "Séance seuil classique."
        })
        .to_string();
        let client = FakeClient { response };

        generate(&pool, &client, None, "profil de test", DEFAULT_SYSTEM_PROMPT).await.unwrap();

        let (name, source, digest_id): (String, String, Option<i64>) =
            sqlx::query_as("SELECT name, source, digest_id FROM workouts")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "Séance seuil");
        assert_eq!(source, "digest");
        assert!(digest_id.is_some());
    }

    #[tokio::test]
    async fn workout_json_not_matching_schema_skips_workouts_insert_but_digest_still_stores() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 205, 192, 53)")
            .execute(&pool)
            .await
            .unwrap();

        // Valid JSON, but "steps" is missing entirely — fails Workout's schema.
        let response = json!({
            "workout": {"name": "Séance cassée"},
            "notes": "notes"
        })
        .to_string();
        let client = FakeClient { response };

        generate(&pool, &client, None, "profil de test", DEFAULT_SYSTEM_PROMPT).await.unwrap();

        let (digest_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM digests")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(digest_count, 1, "digest must still store even when workout schema mismatches");

        let (workout_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM workouts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(workout_count, 0);
    }
}
