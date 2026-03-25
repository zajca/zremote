use std::sync::Arc;

use axum::Json;
use axum::extract::State;

use super::super::state::LocalAppState;

/// Returns `{"mode": "local", "version": "..."}` so clients can detect local mode.
pub async fn api_mode() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "mode": "local",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Health check endpoint for the local server.
pub async fn health(State(state): State<Arc<LocalAppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "mode": "local",
        "hostname": state.hostname,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        LocalAppState::new(pool, "test-host".to_string(), host_id, shutdown, false)
    }

    #[tokio::test]
    async fn api_mode_returns_local() {
        let app = Router::new().route("/api/mode", get(api_mode));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/mode")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["mode"], "local");
        assert!(json["version"].is_string());
        assert!(!json["version"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn health_returns_ok_with_mode() {
        let state = test_state().await;
        let app = Router::new()
            .route("/health", get(health))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["mode"], "local");
        assert_eq!(json["hostname"], "test-host");
    }
}
