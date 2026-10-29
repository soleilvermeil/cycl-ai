use super::AppState;
use axum::extract::{Multipart, State};
use axum::response::Redirect;
use sqlx::SqlitePool;

/// Manual counterpart to `sync::run` for a single ride uploaded as a `.fit`
/// or `.gpx` file, without depending on any external API. Parses the file into the same shape a Strava sync
/// produces (`importer::ParsedActivity`) and writes it through the same
/// tables, so everything downstream — metrics, zones, route thumbnail,
/// bilan generation — works identically regardless of the activity's
/// origin.
pub async fn import_activity(State(state): State<AppState>, mut multipart: Multipart) -> Redirect {
    let mut file_name = None;
    let mut bytes = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            file_name = field.file_name().map(|s| s.to_string());
            bytes = field.bytes().await.ok();
        }
    }

    let (Some(file_name), Some(bytes)) = (file_name, bytes) else {
        tracing::warn!("activity import: no file in upload");
        return Redirect::to("/");
    };

    let lower = file_name.to_lowercase();
    let parsed = if lower.ends_with(".fit") {
        crate::importer::parse_fit(&bytes)
    } else if lower.ends_with(".gpx") {
        crate::importer::parse_gpx(&bytes)
    } else {
        tracing::warn!(
            file_name,
            "activity import: unsupported file extension (expected .fit or .gpx)"
        );
        return Redirect::to("/");
    };

    let parsed = match parsed {
        Ok(p) => p,
        Err(err) => {
            tracing::error!(?err, file_name, "activity import: failed to parse file");
            return Redirect::to("/");
        }
    };

    let title = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(&file_name)
        .to_string();

    match insert_imported_activity(&state.pool, &title, &parsed).await {
        Ok(id) => {
            if let Err(err) = crate::metrics::run(&state.pool).await {
                tracing::error!(
                    ?err,
                    activity_id = id,
                    "activity import: metrics recompute failed"
                );
            }
            Redirect::to(&format!("/activity/{id}"))
        }
        Err(err) => {
            tracing::error!(?err, file_name, "activity import: failed to store activity");
            Redirect::to("/")
        }
    }
}

/// Imported activities get a negative id (Strava's are always positive) so
/// the two id spaces can never collide.
async fn next_imported_id(pool: &SqlitePool) -> anyhow::Result<i64> {
    crate::db::next_negative_activity_id(pool).await
}

async fn insert_imported_activity(
    pool: &SqlitePool,
    title: &str,
    parsed: &crate::importer::ParsedActivity,
) -> anyhow::Result<i64> {
    let id = next_imported_id(pool).await?;

    sqlx::query(
        r#"
        INSERT INTO activities
            (id, started_at, sport_type, duration_s, distance_m, elevation_gain_m,
             avg_power_w, normalized_power_w, avg_hr, max_hr, title, source)
        VALUES (?, ?, 'Ride', ?, ?, ?, ?, ?, ?, ?, ?, 'file')
        "#,
    )
    .bind(id)
    .bind(&parsed.started_at)
    .bind(parsed.duration_s)
    .bind(parsed.distance_m)
    .bind(parsed.elevation_gain_m)
    .bind(parsed.avg_power_w)
    .bind(parsed.normalized_power_w)
    .bind(parsed.avg_hr)
    .bind(parsed.max_hr)
    .bind(title)
    .execute(pool)
    .await?;

    let streams_json = parsed.streams_json.to_string();
    let streams_json_for_points = streams_json.clone();
    let route_thumb_svg = crate::route_thumb::compute(&streams_json).unwrap_or_default();
    sqlx::query("INSERT INTO activity_streams (activity_id, streams_json) VALUES (?, ?)")
        .bind(id)
        .bind(streams_json)
        .execute(pool)
        .await?;
    let route_points =
        crate::route_thumb::compute_points(&streams_json_for_points).unwrap_or_default();
    sqlx::query("UPDATE activities SET route_thumb_svg = ?, route_points_json = ? WHERE id = ?")
        .bind(route_thumb_svg)
        .bind(route_points)
        .bind(id)
        .execute(pool)
        .await?;
    crate::bests::store(pool, id, false, &parsed.streams_json).await?;

    for lap in &parsed.laps {
        sqlx::query(
            "INSERT INTO laps (activity_id, lap_index, elapsed_time_s, distance_m, avg_power_w, avg_hr, max_hr) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(lap.lap_index)
        .bind(lap.elapsed_time_s)
        .bind(lap.distance_m)
        .bind(lap.avg_power_w)
        .bind(lap.avg_hr)
        .bind(lap.max_hr)
        .execute(pool)
        .await?;
    }

    Ok(id)
}
