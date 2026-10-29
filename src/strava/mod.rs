mod models;

use models::{DetailedActivity, TokenResponse};
pub use models::{Lap, StarredSegment, SummaryActivity};

const OAUTH_URL: &str = "https://www.strava.com/oauth/token";
const API_BASE: &str = "https://www.strava.com/api/v3";
const STREAM_KEYS: &str = "latlng,distance,watts,heartrate,altitude,cadence,time";

pub struct StravaClient {
    http: reqwest::Client,
    access_token: String,
    /// Refresh token returned by this authentication call (Strava rotates it
    /// on every use) — the caller is responsible for persisting it.
    pub refresh_token: String,
}

impl StravaClient {
    /// Exchanges a refresh token for a fresh access token. Strava returns a
    /// new refresh_token on every call; the caller must persist it (env vars
    /// are only the seed for the very first run).
    pub async fn authenticate(
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::new();
        let resp: TokenResponse = http
            .post(OAUTH_URL)
            .form(&[
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(Self {
            http,
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
        })
    }

    /// Lists activities started after `after_epoch` (Strava's `after` param,
    /// unix seconds), paginating until exhausted.
    pub async fn list_activities_after(
        &self,
        after_epoch: i64,
    ) -> anyhow::Result<Vec<SummaryActivity>> {
        let mut all = Vec::new();
        let mut page = 1u32;
        loop {
            let batch: Vec<SummaryActivity> = self
                .http
                .get(format!("{API_BASE}/athlete/activities"))
                .bearer_auth(&self.access_token)
                .query(&[
                    ("after", after_epoch.to_string()),
                    ("page", page.to_string()),
                    ("per_page", "200".to_string()),
                ])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            let done = batch.len() < 200;
            all.extend(batch);
            if done {
                break;
            }
            page += 1;
        }
        Ok(all)
    }

    /// One-time coordinate seeding for `reference_segments`: the user stars
    /// segments manually on Strava, we just pull their coordinates.
    pub async fn list_starred_segments(&self) -> anyhow::Result<Vec<StarredSegment>> {
        let segments: Vec<StarredSegment> = self
            .http
            .get(format!("{API_BASE}/segments/starred"))
            .bearer_auth(&self.access_token)
            .query(&[("per_page", "200")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(segments)
    }

    /// Fetches manual/auto laps for an activity — how the athlete's device
    /// split a structured workout (each interval, typically), separate from
    /// our own local segment matching.
    pub async fn get_laps(&self, activity_id: i64) -> anyhow::Result<Vec<Lap>> {
        let laps: Vec<Lap> = self
            .http
            .get(format!("{API_BASE}/activities/{activity_id}/laps"))
            .bearer_auth(&self.access_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(laps)
    }

    /// Fetches the athlete-written free-text description for an activity —
    /// not part of the summary list, so this costs one extra API call per
    /// newly-synced activity.
    pub async fn get_activity_description(
        &self,
        activity_id: i64,
    ) -> anyhow::Result<Option<String>> {
        let detail: DetailedActivity = self
            .http
            .get(format!("{API_BASE}/activities/{activity_id}"))
            .bearer_auth(&self.access_token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(detail.description)
    }

    /// Fetches raw GPS/power/HR streams for an activity as opaque JSON —
    /// parsed later by the segment-matching and metrics code.
    pub async fn get_streams(&self, activity_id: i64) -> anyhow::Result<serde_json::Value> {
        let value = self
            .http
            .get(format!("{API_BASE}/activities/{activity_id}/streams"))
            .bearer_auth(&self.access_token)
            .query(&[("keys", STREAM_KEYS), ("key_by_type", "true")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(value)
    }
}
