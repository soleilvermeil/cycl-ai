//! Garmin Connect Personal API (single-account key). Activities we keep are
//! downloaded as the original FIT file and parsed by `importer::parse_fit`,
//! which already produces the same streams shape as a Strava sync.
//!
//! The activity search materialises the whole result before paging and fails
//! (HTTP 500) somewhere around fifty activities, so history is walked in
//! short date windows that split themselves when a window is too busy.

use anyhow::Context;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;

const API_BASE: &str = "https://personal.connectapi.garmin.com";
const TOKEN_URL: &str =
    "https://personal.connectapi.garmin.com/personalapitoken-service/personal-api/personal-api-key/token";
const PAGE_LIMIT: i64 = 20;
/// Full-history walk stops after this many empty 30-day windows in a row
/// (~two years without an activity).
const EMPTY_WINDOWS_BEFORE_STOP: u32 = 24;

pub struct GarminClient {
    http: reqwest::Client,
    access_token: String,
}

#[derive(Debug)]
pub struct ListedActivity {
    pub id: i64,
    pub name: String,
    /// Unix seconds of `startTimeGMT`, when Garmin sent one.
    pub started_at_epoch: Option<i64>,
    /// CyclAI sport type (`Ride`, `VirtualRide`, `Run`, `TrailRun`).
    pub sport_type: String,
}

#[derive(Debug, Deserialize)]
struct RawActivity {
    #[serde(rename = "activityId")]
    activity_id: i64,
    #[serde(rename = "activityName")]
    activity_name: Option<String>,
    #[serde(rename = "startTimeGMT")]
    start_time_gmt: Option<String>,
    #[serde(rename = "activityType")]
    activity_type: ActivityType,
    #[serde(default, rename = "manualActivity")]
    manual: bool,
}

#[derive(Debug, Deserialize)]
struct ActivityType {
    #[serde(rename = "typeKey")]
    type_key: String,
}

enum Page {
    Items(Vec<RawActivity>),
    /// The window is too large for Garmin to build (HTTP 500, or a full page
    /// that may be truncated).
    TooLarge,
}

impl GarminClient {
    /// Exchanges the personal API key for a bearer token. The body is the raw
    /// JWT (not JSON). Tokens last 24 h and there is no refresh token.
    pub async fn authenticate(api_key: &str) -> anyhow::Result<Self> {
        let api_key = api_key.trim();
        anyhow::ensure!(!api_key.is_empty(), "clé API Garmin vide");
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("client HTTP Garmin")?;
        let resp = http
            .post(TOKEN_URL)
            .header("Authorization", format!("ApiKey {api_key}"))
            .header("Accept", "*/*")
            .send()
            .await
            .context("obtention du jeton Garmin")?
            .error_for_status()
            .context("clé API Garmin refusée (expirée ou révoquée)")?;
        let token = resp.text().await?.trim().trim_matches('"').to_string();
        anyhow::ensure!(
            token.split('.').count() == 3,
            "la réponse du jeton Garmin n'est pas un JWT"
        );
        Ok(Self { http, access_token: token })
    }

    /// Supported activities. `after_epoch` `None` (or 0) walks history backward
    /// until two quiet years; otherwise only windows from that instant to today.
    pub async fn list_supported(&self, after_epoch: Option<i64>) -> anyhow::Result<Vec<ListedActivity>> {
        let today = Utc::now().date_naive();
        let mut raw = Vec::new();
        if let Some(epoch) = after_epoch.filter(|epoch| *epoch > 0) {
            let mut cursor = DateTime::<Utc>::from_timestamp(epoch, 0)
                .map(|dt| dt.date_naive())
                .unwrap_or(today);
            while cursor <= today {
                let end = cursor
                    .checked_add_signed(Duration::days(29))
                    .unwrap_or(today)
                    .min(today);
                raw.extend(self.search_range(cursor, end).await?);
                cursor = match end.checked_add_signed(Duration::days(1)) {
                    Some(next) => next,
                    None => break,
                };
            }
            raw.retain(|activity| {
                parse_gmt(activity.start_time_gmt.as_deref().unwrap_or(""))
                    .map(|started| started >= epoch)
                    .unwrap_or(true)
            });
        } else {
            let mut window_end = today;
            let mut empty_windows = 0u32;
            let floor = NaiveDate::from_ymd_opt(2007, 1, 1).expect("valid date");
            while window_end >= floor && empty_windows < EMPTY_WINDOWS_BEFORE_STOP {
                let window_start = window_end.checked_sub_signed(Duration::days(29)).unwrap_or(floor);
                let batch = self.search_range(window_start, window_end).await?;
                if batch.is_empty() {
                    empty_windows += 1;
                } else {
                    empty_windows = 0;
                    raw.extend(batch);
                }
                window_end = match window_start.checked_sub_signed(Duration::days(1)) {
                    Some(prev) if prev >= floor => prev,
                    _ => break,
                };
            }
        }

        let mut listed = Vec::new();
        for activity in raw {
            if activity.manual {
                continue;
            }
            let Some(sport) = map_sport(&activity.activity_type.type_key) else {
                continue;
            };
            let name = activity.activity_name.unwrap_or_default().trim().to_string();
            listed.push(ListedActivity {
                id: activity.activity_id,
                name: if name.is_empty() { "Sortie".into() } else { name },
                started_at_epoch: activity.start_time_gmt.as_deref().and_then(parse_gmt),
                sport_type: sport.to_string(),
            });
        }
        listed.sort_by_key(|activity| activity.id);
        listed.dedup_by_key(|activity| activity.id);
        listed.sort_by_key(|activity| activity.started_at_epoch.unwrap_or(0));
        Ok(listed)
    }

    /// Original activity file, unzipped to the FIT bytes. `None` when Garmin
    /// has no file (manual entry, or a zip without a FIT).
    pub async fn download_fit(&self, activity_id: i64) -> anyhow::Result<Option<Vec<u8>>> {
        let url = format!("{API_BASE}/download-service/files/activity/{activity_id}");
        let resp = self
            .send_get(
                self.http.get(url).bearer_auth(&self.access_token).header(
                    "Accept",
                    "application/zip, application/x-zip-compressed, application/octet-stream",
                ),
            )
            .await
            .with_context(|| format!("téléchargement de l'activité {activity_id}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let bytes = resp.bytes().await?;
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            anyhow::bail!(
                "Garmin limite le débit pendant le téléchargement. Relance la synchro dans quelques minutes : elle reprend où elle s'est arrêtée."
            );
        }
        if !status.is_success() {
            anyhow::bail!("téléchargement FIT {activity_id} : HTTP {status}");
        }
        extract_fit(&bytes)
    }

    async fn search_range(&self, start: NaiveDate, end: NaiveDate) -> anyhow::Result<Vec<RawActivity>> {
        let mut pending = vec![(start, end)];
        let mut out = Vec::new();
        let mut steps = 0u32;
        while let Some((start, end)) = pending.pop() {
            steps += 1;
            anyhow::ensure!(steps < 500, "découpage de la recherche Garmin trop profond");
            if end < start {
                continue;
            }
            match self.search_page(start, end, 0).await? {
                Page::Items(items) if (items.len() as i64) < PAGE_LIMIT => out.extend(items),
                Page::Items(_) if start == end => out.extend(self.search_day_paged(start).await?),
                Page::TooLarge if start == end => {
                    anyhow::bail!("Garmin n'a pas pu lister les activités du {start}")
                }
                Page::Items(_) | Page::TooLarge => {
                    let days = (end - start).num_days().max(1);
                    let mid = start + Duration::days(days / 2);
                    if let Some(next) = mid.checked_add_signed(Duration::days(1)) {
                        if next <= end {
                            pending.push((next, end));
                        }
                    }
                    pending.push((start, mid));
                }
            }
        }
        Ok(out)
    }

    /// One calendar day, followed by offset pages. A single day is small
    /// enough that paging does not hit the "whole result set" failure.
    async fn search_day_paged(&self, day: NaiveDate) -> anyhow::Result<Vec<RawActivity>> {
        let mut out = Vec::new();
        let mut offset = 0i64;
        loop {
            match self.search_page(day, day, offset).await? {
                Page::TooLarge => anyhow::bail!("Garmin n'a pas pu lister les activités du {day}"),
                Page::Items(items) => {
                    let full = items.len() as i64 == PAGE_LIMIT;
                    out.extend(items);
                    if !full || offset > 400 {
                        break;
                    }
                    offset += PAGE_LIMIT;
                }
            }
        }
        Ok(out)
    }

    /// Replays a GET when Garmin answers 429. `Retry-After` is honored, otherwise
    /// the wait doubles from 2 s. The request must be cloneable (no streamed body).
    async fn send_get(&self, request: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        let mut wait = std::time::Duration::from_secs(2);
        for attempt in 0..6 {
            let resp = request
                .try_clone()
                .context("requête Garmin non rejouable")?
                .send()
                .await?;
            if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS || attempt == 5 {
                return Ok(resp);
            }
            let pause = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(std::time::Duration::from_secs)
                .unwrap_or(wait);
            tracing::info!(secs = pause.as_secs(), "Garmin rate limit, waiting");
            tokio::time::sleep(pause).await;
            wait = (wait * 2).min(std::time::Duration::from_secs(60));
        }
        unreachable!("retry loop returns on the last attempt")
    }

    async fn search_page(&self, start: NaiveDate, end: NaiveDate, offset: i64) -> anyhow::Result<Page> {
        let url = format!("{API_BASE}/activitylist-service/activities/search/activities");
        let resp = self
            .send_get(
                self.http
                    .get(url)
                    .bearer_auth(&self.access_token)
                    .header("Accept", "application/json")
                    .query(&[
                        ("start", offset.to_string()),
                        ("limit", PAGE_LIMIT.to_string()),
                        ("startDate", start.format("%Y-%m-%d").to_string()),
                        ("endDate", end.format("%Y-%m-%d").to_string()),
                    ]),
            )
            .await
            .context("recherche d'activités Garmin")?;
        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Page::Items(Vec::new()));
        }
        if status == reqwest::StatusCode::INTERNAL_SERVER_ERROR {
            return Ok(Page::TooLarge);
        }
        let bytes = resp.bytes().await?;
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            anyhow::bail!(
                "Garmin limite le débit. Relance la synchro dans quelques minutes : elle reprend où elle s'est arrêtée."
            );
        }
        if !status.is_success() {
            let body: String = String::from_utf8_lossy(&bytes).chars().take(180).collect();
            anyhow::bail!("recherche Garmin HTTP {status} : {body}");
        }
        if bytes.is_empty() {
            return Ok(Page::Items(Vec::new()));
        }
        let items: Vec<RawActivity> =
            serde_json::from_slice(&bytes).context("réponse de recherche Garmin illisible")?;
        Ok(Page::Items(items))
    }
}

fn extract_fit(bytes: &[u8]) -> anyhow::Result<Option<Vec<u8>>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .context("le fichier Garmin n'est pas une archive ZIP")?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        if !file.name().to_ascii_lowercase().ends_with(".fit") {
            continue;
        }
        let mut buf = Vec::new();
        std::io::copy(&mut file, &mut buf)?;
        return Ok(Some(buf));
    }
    Ok(None)
}

/// Garmin `activityType.typeKey` → CyclAI sport. Gravel, VTT and indoor
/// cardio stay out, same policy as the Strava filter (`GravelRide` /
/// `MountainBikeRide` excluded). Generic `cycling` is kept: an Edge that
/// records "vélo" (not "route") reports that key.
pub fn map_sport(type_key: &str) -> Option<&'static str> {
    match type_key {
        "cycling" | "road_biking" => Some("Ride"),
        "indoor_cycling" | "virtual_ride" => Some("VirtualRide"),
        "running" | "treadmill_running" | "track_running" => Some("Run"),
        "trail_running" | "ultra_run" => Some("TrailRun"),
        _ => None,
    }
}

fn parse_gmt(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S"))
        .or_else(|_| NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f"))
        .ok()?;
    Some(naive.and_utc().timestamp())
}

#[cfg(test)]
mod tests {
    use super::{map_sport, parse_gmt};
    use chrono::{DateTime, Utc};

    #[test]
    fn maps_the_sports_cyclai_keeps() {
        assert_eq!(map_sport("cycling"), Some("Ride"));
        assert_eq!(map_sport("road_biking"), Some("Ride"));
        assert_eq!(map_sport("indoor_cycling"), Some("VirtualRide"));
        assert_eq!(map_sport("virtual_ride"), Some("VirtualRide"));
        assert_eq!(map_sport("running"), Some("Run"));
        assert_eq!(map_sport("treadmill_running"), Some("Run"));
        assert_eq!(map_sport("trail_running"), Some("TrailRun"));
        assert_eq!(map_sport("ultra_run"), Some("TrailRun"));
        assert_eq!(map_sport("indoor_cardio"), None);
        assert_eq!(map_sport("gravel_cycling"), None);
        assert_eq!(map_sport("mountain_biking"), None);
    }

    #[test]
    fn parses_garmin_gmt_without_a_timezone() {
        let epoch = parse_gmt("2026-10-27 16:51:54").unwrap();
        let formatted = DateTime::<Utc>::from_timestamp(epoch, 0)
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert_eq!(formatted, "2026-10-27 16:51:54");
        assert_eq!(parse_gmt("2026-10-27T16:51:54.0"), Some(epoch));
        assert_eq!(parse_gmt(""), None);
    }
}
