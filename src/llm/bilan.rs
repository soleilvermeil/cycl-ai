use super::LlmClient;
use sqlx::SqlitePool;

/// Seeded into `settings.bilan_system_prompt` on first boot — editable
/// afterwards from the dashboard's /settings page, this constant is no
/// longer read at request time (see `generate_activity_bilans`'s
/// `system_prompt` parameter).
pub(crate) const DEFAULT_SYSTEM_PROMPT: &str = "Tu es un coach cyclisme qui commente une sortie de vélo pour un cycliste dont tu connais le profil complet. \
Donne un bilan qualitatif court (2-4 phrases), concret, en te basant sur les deltas chiffrés fournis (comparaisons vs record perso, \
moyenne récente, dernier passage). Style direct, factuel, pas de généralités. Exemple de ton attendu: \
\"départ trop rapide sur la première partie de la côte (+9.5% vs ta moyenne perso)\", \
\"amélioration de 4W sur le segment vs ton dernier passage\".\n\n\
Aucune séance prévue ne t'est transmise (pas de plan d'entraînement connu) : ne suppose jamais ce que la sortie était \"censée\" être \
(endurance, seuil, etc.) ni son objectif visé — décris uniquement ce qui s'est réellement passé, à partir des chiffres fournis.\n\
Pour interpréter la variabilité de puissance (VI = NP/moyenne), croise-la avec le D+ réel fourni : un VI élevé avec un D+ faible \
suggère un travail par intervalles ou une variation d'effort volontaire, pas un profil de terrain accidenté. Ne parle de terrain \
accidenté / relances liées au relief que si le D+ le confirme réellement (ex: >15m/km sur la sortie).\n\
Si des laps (splits manuels) sont fournis, c'est le signe d'une séance structurée par intervalles : compare les laps entre eux \
(régularité de puissance/FC d'un lap à l'autre, dérive sur les derniers laps, lap le plus faible ou le plus fort) plutôt que de \
commenter uniquement la moyenne globale de la sortie.";

struct ActivityDeltas {
    activity_id: i64,
    is_run: bool,
    ftp_watts: f64,
    distance_km: f64,
    duration_min: f64,
    elevation_gain_m: f64,
    avg_power_w: Option<f64>,
    normalized_power_w: Option<f64>,
    avg_hr: Option<f64>,
    tss: Option<f64>,
    intensity_factor: Option<f64>,
    zone_secs: [i64; 7],
    segment_efforts: Vec<SegmentEffortRow>,
    laps: Vec<LapRow>,
    title: Option<String>,
    description: Option<String>,
    user_context: Option<String>,
}

struct SegmentEffortRow {
    name: String,
    avg_power_w: f64,
    delta_vs_pr_pct: Option<f64>,
    delta_vs_recent_avg_pct: Option<f64>,
    delta_vs_last_pct: Option<f64>,
}

struct LapRow {
    lap_index: i64,
    elapsed_time_s: i64,
    avg_power_w: Option<f64>,
    avg_hr: Option<f64>,
}

#[derive(sqlx::FromRow)]
struct ActivityRow {
    sport_type: String,
    distance_m: f64,
    duration_s: i64,
    elevation_gain_m: f64,
    avg_power_w: Option<f64>,
    normalized_power_w: Option<f64>,
    avg_hr: Option<f64>,
    tss: Option<f64>,
    intensity_factor: Option<f64>,
    zone1_s: i64,
    zone2_s: i64,
    zone3_s: i64,
    zone4_s: i64,
    zone5_s: i64,
    zone6_s: i64,
    zone7_s: i64,
    user_context: Option<String>,
    title: Option<String>,
    description: Option<String>,
}

async fn fetch_activity_deltas(pool: &SqlitePool, activity_id: i64) -> anyhow::Result<ActivityDeltas> {
    let (ftp_watts,): (i64,) = sqlx::query_as("SELECT ftp_watts FROM athlete_config WHERE id = 1")
        .fetch_one(pool)
        .await?;

    let row: ActivityRow = sqlx::query_as(
        r#"
        SELECT sport_type, distance_m, duration_s, elevation_gain_m, avg_power_w, normalized_power_w,
               avg_hr, tss, intensity_factor,
               zone1_s, zone2_s, zone3_s, zone4_s, zone5_s, zone6_s, zone7_s,
               user_context, title, description
        FROM activities WHERE id = ?
        "#,
    )
    .bind(activity_id)
    .fetch_one(pool)
    .await?;

    let segment_efforts: Vec<(String, f64, Option<f64>, Option<f64>, Option<f64>)> = sqlx::query_as(
        r#"
        SELECT rs.name, se.avg_power_w, se.delta_vs_pr_pct, se.delta_vs_recent_avg_pct, se.delta_vs_last_pct
        FROM segment_efforts se
        JOIN reference_segments rs ON rs.id = se.reference_segment_id
        WHERE se.activity_id = ?
        "#,
    )
    .bind(activity_id)
    .fetch_all(pool)
    .await?;

    let laps: Vec<(i64, i64, Option<f64>, Option<f64>)> = sqlx::query_as(
        "SELECT lap_index, elapsed_time_s, avg_power_w, avg_hr FROM laps WHERE activity_id = ? ORDER BY lap_index",
    )
    .bind(activity_id)
    .fetch_all(pool)
    .await?;

    Ok(ActivityDeltas {
        activity_id,
        is_run: crate::metrics::is_run(&row.sport_type),
        ftp_watts: ftp_watts as f64,
        distance_km: row.distance_m / 1000.0,
        duration_min: row.duration_s as f64 / 60.0,
        elevation_gain_m: row.elevation_gain_m,
        avg_power_w: row.avg_power_w,
        normalized_power_w: row.normalized_power_w,
        avg_hr: row.avg_hr,
        tss: row.tss,
        intensity_factor: row.intensity_factor,
        zone_secs: [row.zone1_s, row.zone2_s, row.zone3_s, row.zone4_s, row.zone5_s, row.zone6_s, row.zone7_s],
        segment_efforts: segment_efforts
            .into_iter()
            .map(|(name, avg_power_w, delta_vs_pr_pct, delta_vs_recent_avg_pct, delta_vs_last_pct)| {
                SegmentEffortRow {
                    name,
                    avg_power_w,
                    delta_vs_pr_pct,
                    delta_vs_recent_avg_pct,
                    delta_vs_last_pct,
                }
            })
            .collect(),
        laps: laps
            .into_iter()
            .map(|(lap_index, elapsed_time_s, avg_power_w, avg_hr)| LapRow {
                lap_index,
                elapsed_time_s,
                avg_power_w,
                avg_hr,
            })
            .collect(),
        user_context: row.user_context,
        title: row.title,
        description: row.description,
    })
}

fn format_deltas(d: &ActivityDeltas) -> String {
    let dplus_per_km = if d.distance_km > 0.0 {
        d.elevation_gain_m / d.distance_km
    } else {
        0.0
    };
    let mut out = if d.is_run {
        // No power, no FTP-relative metrics for runs: pace and HR only.
        let pace = crate::web::format::format_pace(d.distance_km * 1000.0, (d.duration_min * 60.0) as i64)
            .map(|p| format!(", allure moyenne {p}/km"))
            .unwrap_or_default();
        format!(
            "Course à pied: {:.1}km, {:.0}min, {:.0}m D+ ({dplus_per_km:.0}m/km){pace}\n",
            d.distance_km, d.duration_min, d.elevation_gain_m
        )
    } else {
        format!(
            "Sortie: {:.1}km, {:.0}min, {:.0}m D+ ({dplus_per_km:.0}m/km — pour référence, plat < 10m/km, vallonné 10-20m/km, montagneux > 20m/km), FTP de référence {:.0}W\n",
            d.distance_km, d.duration_min, d.elevation_gain_m, d.ftp_watts
        )
    };
    if let Some(p) = d.avg_power_w {
        out += &format!("Puissance moy: {p:.0}W");
        if let Some(np) = d.normalized_power_w {
            out += &format!(", NP: {np:.0}W");
        }
        out += "\n";
    }
    if let Some(hr) = d.avg_hr {
        out += &format!("FC moy: {hr:.0}bpm\n");
    }
    if let (Some(tss), Some(if_)) = (d.tss, d.intensity_factor) {
        out += &format!("TSS: {tss:.0}, IF: {if_:.2}\n");
    } else if let (true, Some(tss)) = (d.is_run, d.tss) {
        out += &format!("hrTSS (charge estimée via la FC): {tss:.0}\n");
    }
    let total_secs: i64 = d.zone_secs.iter().sum();
    if total_secs > 0 {
        out += "Répartition zones (Z1-Z7): ";
        out += &d
            .zone_secs
            .iter()
            .map(|s| format!("{:.0}%", *s as f64 / total_secs as f64 * 100.0))
            .collect::<Vec<_>>()
            .join(" / ");
        out += "\n";
    }
    if !d.segment_efforts.is_empty() {
        out += "Segments de référence:\n";
        for seg in &d.segment_efforts {
            out += &format!("- {}: {:.0}W", seg.name, seg.avg_power_w);
            let mut deltas = Vec::new();
            if let Some(v) = seg.delta_vs_pr_pct {
                deltas.push(format!("{v:+.1}% vs record perso"));
            }
            if let Some(v) = seg.delta_vs_recent_avg_pct {
                deltas.push(format!("{v:+.1}% vs moyenne récente"));
            }
            if let Some(v) = seg.delta_vs_last_pct {
                deltas.push(format!("{v:+.1}% vs dernier passage"));
            }
            if !deltas.is_empty() {
                out += &format!(" ({})", deltas.join(", "));
            }
            out += "\n";
        }
    }
    // A Strava activity always has at least 1 lap (the whole ride) even
    // with no manual laps pressed — only worth showing when there's an
    // actual structured breakdown to analyze.
    if d.laps.len() > 1 {
        out += "Laps (structure manuelle pendant la sortie):\n";
        for lap in &d.laps {
            out += &format!(
                "- Lap {}: {}",
                lap.lap_index,
                crate::web::format_zone_duration(lap.elapsed_time_s)
            );
            if let Some(p) = lap.avg_power_w {
                out += &format!(", {p:.0}W");
            }
            if let Some(hr) = lap.avg_hr {
                out += &format!(", {hr:.0}bpm");
            }
            out += "\n";
        }
    }
    out
}

fn build_user_message(d: &ActivityDeltas, profile: &str) -> String {
    let mut out = format!(
        "Profil du cycliste:\n{profile}\n\nDeltas de la sortie du jour:\n{}",
        format_deltas(d)
    );
    if d.is_run {
        out += "\nNote: cette sortie est une course à pied, pas du vélo. Il n'y a ni puissance, ni FTP, ni segments, ni TSS \
                pour les sorties à pied : commente l'allure (min/km), la régularité, la FC et le dénivelé, et ignore les \
                références au vélo de la consigne.\n";
    }
    // Strava's own title is often meaningful even when generic ("Morning
    // Ride" tells you nothing, but the athlete sometimes renames it e.g.
    // "Sortie difficile, jambes lourdes") — always non-empty from Strava,
    // so no blank-filter needed unlike description/user_context.
    if let Some(title) = d.title.as_deref() {
        out += &format!("\nTitre Strava de la sortie: \"{title}\"\n");
    }
    if let Some(desc) = d.description.as_deref().filter(|c| !c.trim().is_empty()) {
        out += &format!("\nDescription Strava écrite par le cycliste:\n{}\n", desc.trim());
    }
    if let Some(ctx) = d.user_context.as_deref().filter(|c| !c.trim().is_empty()) {
        out += &format!(
            "\nContexte additionnel fourni par le cycliste pour cette sortie (prioritaire sur les suppositions, à prendre en compte pour affiner l'analyse):\n{}\n",
            ctx.trim()
        );
    }
    out
}

/// The user message the LLM receives for one activity (profile + deltas +
/// Strava title/description + user context). Used by the downloadable
/// no-API report so a chat assistant is fed exactly what the API would be.
pub async fn build_activity_message(pool: &SqlitePool, activity_id: i64, profile: &str) -> anyhow::Result<String> {
    let deltas = fetch_activity_deltas(pool, activity_id).await?;
    Ok(build_user_message(&deltas, profile))
}

/// Generates and stores a bilan for each given activity id, replacing any
/// existing one — called both for newly-synced activities (no existing
/// bilan, the DELETE is a no-op) and for a manual regeneration after the
/// user adds context to an already-bilanned activity (see
/// `web::detail::regenerate_bilan`).
pub async fn generate_activity_bilans<C: LlmClient>(
    pool: &SqlitePool,
    client: &C,
    activity_ids: &[i64],
    profile: &str,
    system_prompt: &str,
) -> anyhow::Result<()> {
    for &activity_id in activity_ids {
        let deltas = fetch_activity_deltas(pool, activity_id).await?;
        let user_message = build_user_message(&deltas, profile);
        let text = client.complete(system_prompt, &user_message).await?;

        sqlx::query("DELETE FROM activity_bilans WHERE activity_id = ?")
            .bind(deltas.activity_id)
            .execute(pool)
            .await?;
        sqlx::query("INSERT INTO activity_bilans (activity_id, text) VALUES (?, ?)")
            .bind(deltas.activity_id)
            .bind(text)
            .execute(pool)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeClient {
        calls: Mutex<Vec<(String, String)>>,
        response: String,
    }

    impl LlmClient for FakeClient {
        async fn complete(&self, system: &str, user_message: &str) -> anyhow::Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push((system.to_string(), user_message.to_string()));
            Ok(self.response.clone())
        }
    }

    const TEST_PROFILE: &str = "- Cycliste amateur route, profil de test";

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn generates_and_stores_bilan_with_deltas_in_prompt() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 205, 192, 53)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO activities \
             (id, started_at, duration_s, distance_m, elevation_gain_m, avg_power_w, normalized_power_w, avg_hr, tss, intensity_factor, zone1_s, zone2_s) \
             VALUES (1, '2026-08-19T10:00:00Z', 3600, 30000, 300, 180, 200, 150, 65, 0.97, 1800, 1800)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO reference_segments (name, start_lat, start_lng, end_lat, end_lng, distance_m, elevation_gain_m) \
             VALUES ('cote-test', 49.1, 6.2, 49.11, 6.21, 2420, 131)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO segment_efforts (activity_id, reference_segment_id, avg_power_w, delta_vs_pr_pct, delta_vs_recent_avg_pct, delta_vs_last_pct) \
             VALUES (1, 1, 220, 9.5, 9.5, 9.5)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let client = FakeClient {
            calls: Mutex::new(Vec::new()),
            response: "Bon effort.".to_string(),
        };

        generate_activity_bilans(&pool, &client, &[1], TEST_PROFILE, DEFAULT_SYSTEM_PROMPT).await.unwrap();

        let (stored_text,): (String,) =
            sqlx::query_as("SELECT text FROM activity_bilans WHERE activity_id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored_text, "Bon effort.");

        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (system, user_message) = &calls[0];
        assert!(system.contains("coach cyclisme"));
        assert!(user_message.contains("profil de test"));
        assert!(user_message.contains("cote-test"));
        assert!(user_message.contains("+9.5% vs record perso"));
        assert!(user_message.contains("205W")); // FTP pulled live from athlete_config
    }

    #[tokio::test]
    async fn includes_strava_title_and_description_when_present() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 205, 192, 53)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m, title, description) \
             VALUES (1, '2026-08-19T10:00:00Z', 1200, 10000, 50, 'Sortie difficile, jambes lourdes', 'Grosse chaleur aujourd''hui, 34°C')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let client = FakeClient { calls: Mutex::new(Vec::new()), response: "ok".to_string() };
        generate_activity_bilans(&pool, &client, &[1], TEST_PROFILE, DEFAULT_SYSTEM_PROMPT).await.unwrap();
        let msg = &client.calls.lock().unwrap()[0].1;
        assert!(msg.contains("Titre Strava de la sortie: \"Sortie difficile, jambes lourdes\""));
        assert!(msg.contains("Grosse chaleur aujourd'hui, 34°C"));
    }

    #[tokio::test]
    async fn regeneration_includes_user_context_and_replaces_previous_bilan() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 205, 192, 53)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m) \
             VALUES (1, '2026-08-19T10:00:00Z', 1200, 10000, 50)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let client = FakeClient { calls: Mutex::new(Vec::new()), response: "Premier bilan.".to_string() };
        generate_activity_bilans(&pool, &client, &[1], TEST_PROFILE, DEFAULT_SYSTEM_PROMPT).await.unwrap();
        assert!(!client.calls.lock().unwrap()[0].1.contains("Contexte additionnel"));

        sqlx::query("UPDATE activities SET user_context = ? WHERE id = 1")
            .bind("j'étais malade, grosse fatigue avant le départ")
            .execute(&pool)
            .await
            .unwrap();

        let client2 = FakeClient { calls: Mutex::new(Vec::new()), response: "Bilan tenant compte de la maladie.".to_string() };
        generate_activity_bilans(&pool, &client2, &[1], TEST_PROFILE, DEFAULT_SYSTEM_PROMPT).await.unwrap();
        assert!(client2.calls.lock().unwrap()[0].1.contains("Contexte additionnel"));
        assert!(client2.calls.lock().unwrap()[0].1.contains("j'étais malade"));

        // Regeneration replaces the old bilan rather than adding a second row.
        let rows: Vec<(String,)> = sqlx::query_as("SELECT text FROM activity_bilans WHERE activity_id = 1")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "Bilan tenant compte de la maladie.");
    }

    #[tokio::test]
    async fn includes_lap_breakdown_only_when_multiple_laps() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO athlete_config (id, ftp_watts, hr_max, hr_rest) VALUES (1, 205, 192, 53)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m) \
             VALUES (1, '2026-08-19T10:00:00Z', 1200, 10000, 50)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Single lap (whole ride, no manual splits) — should NOT trigger the lap section.
        sqlx::query("INSERT INTO laps (activity_id, lap_index, elapsed_time_s, distance_m) VALUES (1, 1, 1200, 10000)")
            .execute(&pool)
            .await
            .unwrap();

        let client = FakeClient { calls: Mutex::new(Vec::new()), response: "ok".to_string() };
        generate_activity_bilans(&pool, &client, &[1], TEST_PROFILE, DEFAULT_SYSTEM_PROMPT).await.unwrap();
        assert!(!client.calls.lock().unwrap()[0].1.contains("Laps"));

        // Now add a real structured breakdown (3 laps).
        sqlx::query(
            "INSERT INTO activities (id, started_at, duration_s, distance_m, elevation_gain_m) \
             VALUES (2, '2026-08-19T11:00:00Z', 1200, 10000, 50)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO laps (activity_id, lap_index, elapsed_time_s, distance_m, avg_power_w, avg_hr) VALUES (2, 1, 300, 2500, 210, 150)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO laps (activity_id, lap_index, elapsed_time_s, distance_m, avg_power_w, avg_hr) VALUES (2, 2, 300, 2500, 195, 155)")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO laps (activity_id, lap_index, elapsed_time_s, distance_m, avg_power_w, avg_hr) VALUES (2, 3, 300, 2500, 180, 158)")
            .execute(&pool).await.unwrap();

        let client2 = FakeClient { calls: Mutex::new(Vec::new()), response: "ok".to_string() };
        generate_activity_bilans(&pool, &client2, &[2], TEST_PROFILE, DEFAULT_SYSTEM_PROMPT).await.unwrap();
        let msg = &client2.calls.lock().unwrap()[0].1;
        assert!(msg.contains("Laps"));
        assert!(msg.contains("Lap 1: 0:05:00, 210W, 150bpm"));
        assert!(msg.contains("Lap 3: 0:05:00, 180W, 158bpm"));
    }
}
