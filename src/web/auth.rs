//! Bearer-JWT guard for the exposed API router (`web::serve_api`).

use super::AppState;
use crate::apikeys::{authenticate, AuthError};
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

fn deny(status: StatusCode, msg: &str) -> Response {
    let mut res = (status, Json(json!({ "error": msg }))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        res.headers_mut().insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
    }
    res
}

pub(super) async fn require_api_key(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(token) = token else {
        return deny(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let is_write = !matches!(*req.method(), Method::GET | Method::HEAD);
    match authenticate(&state.pool, token, is_write).await {
        Ok(_) => next.run(req).await,
        Err(AuthError::Forbidden) => deny(StatusCode::FORBIDDEN, "this key is read-only"),
        Err(AuthError::Invalid(reason)) => {
            // The reason stays in the logs; the client only learns "invalid".
            tracing::warn!(reason, "api request rejected");
            deny(StatusCode::UNAUTHORIZED, "invalid token")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::api_router;
    use super::*;
    use crate::apikeys::{add, mint_token, Scope};
    use axum::body::Body;
    use axum::http::Request;
    use sqlx::sqlite::SqlitePoolOptions;
    use tower::ServiceExt;

    const PRIVATE: &str = include_str!("../../tests/fixtures/test_rsa_private.pem");
    const PUBLIC: &str = include_str!("../../tests/fixtures/test_rsa_public.pem");

    async fn app(scope: Scope) -> axum::Router {
        let pool = SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        crate::db::seed_athlete_config(&pool, &test_config()).await.unwrap();
        add(&pool, "k", PUBLIC, scope).await.unwrap();
        api_router(AppState { pool })
    }

    fn test_config() -> crate::config::Config {
        crate::config::Config {
            database_url: "sqlite::memory:".into(),
            strava_client_id: String::new(),
            strava_client_secret: String::new(),
            strava_refresh_token: String::new(),
            anthropic_api_key: String::new(),
            ftp_watts: 200,
            hr_max: 190,
            hr_rest: 50,
            athlete_profile: String::new(),
            garmin_api_key: String::new(),
        }
    }

    async fn status(app: &axum::Router, method: &str, uri: &str, auth: Option<String>) -> StatusCode {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(a) = auth {
            req = req.header("authorization", a);
        }
        app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap().status()
    }

    #[tokio::test]
    async fn api_requires_a_valid_token() {
        let app = app(Scope::Read).await;
        assert_eq!(status(&app, "GET", "/api/v1/profile", None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(&app, "GET", "/api/v1/profile", Some("Bearer junk".into())).await, StatusCode::UNAUTHORIZED);
        let token = mint_token(PRIVATE, "k", 60).unwrap();
        assert_eq!(status(&app, "GET", "/api/v1/profile", Some(format!("Bearer {token}"))).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn read_only_key_gets_403_on_writes() {
        let app = app(Scope::Read).await;
        let token = mint_token(PRIVATE, "k", 60).unwrap();
        assert_eq!(
            status(&app, "DELETE", "/api/v1/planned/1", Some(format!("Bearer {token}"))).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn api_router_does_not_serve_the_ui() {
        let app = app(Scope::Read).await;
        let token = mint_token(PRIVATE, "k", 60).unwrap();
        assert_eq!(status(&app, "GET", "/settings", None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(&app, "GET", "/settings", Some(format!("Bearer {token}"))).await, StatusCode::NOT_FOUND);
    }
}
