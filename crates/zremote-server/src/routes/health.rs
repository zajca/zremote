use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub connected_hosts: usize,
}

pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let connected_hosts = state.connections.connected_count().await;
    Json(HealthResponse {
        status: "ok",
        connected_hosts,
    })
}

/// Returns `{"mode": "server", "version": "..."}` so clients can detect server mode.
pub async fn api_mode() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "mode": "server",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Serves the enrollment shell script as `text/x-shellscript`.
pub async fn enroll_sh() -> axum::response::Response {
    use axum::http::header;
    use axum::response::IntoResponse;

    const SCRIPT: &str = include_str!("../../public/enroll.sh");
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        SCRIPT,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::state::{AppState, ConnectionManager};

    async fn test_state() -> Arc<AppState> {
        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
        let connections = Arc::new(ConnectionManager::new());
        let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let agentic_loops = Arc::new(dashmap::DashMap::new());
        let (events_tx, _) = tokio::sync::broadcast::channel(1024);
        Arc::new(AppState {
            db: pool,
            connections,
            sessions,
            agentic_loops,
            agent_token_hash: crate::auth::hash_token("test-token"),
            shutdown: tokio_util::sync::CancellationToken::new(),
            events: events_tx,
            knowledge_requests: Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: Arc::new(dashmap::DashMap::new()),
            directory_requests: Arc::new(dashmap::DashMap::new()),
            settings_get_requests: Arc::new(dashmap::DashMap::new()),
            settings_save_requests: Arc::new(dashmap::DashMap::new()),
            action_inputs_requests: Arc::new(dashmap::DashMap::new()),
            ticket_store: crate::auth::TicketStore::new(),
            oidc_flows: crate::auth::oidc::OidcFlowStore::new(),
        })
    }

    #[tokio::test]
    async fn api_mode_returns_server() {
        let state = test_state().await;
        let app = crate::create_router(state);
        let response = app
            .oneshot(Request::get("/api/mode").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["mode"], "server");
        assert!(json["version"].is_string());
        assert!(!json["version"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn health_status_is_ok() {
        let state = test_state().await;
        let app = crate::create_router(state);
        let response = app
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["connected_hosts"], 0);
    }
}
