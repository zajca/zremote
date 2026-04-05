use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_protocol::channel::ChannelMessage;

use crate::local::state::LocalAppState;

/// Maximum allowed content length for channel messages (64 KB).
const MAX_CHANNEL_CONTENT_LEN: usize = 65_536;

/// Validate channel message content length.
fn validate_channel_message(message: &ChannelMessage) -> Result<(), AppError> {
    let content_len = match message {
        ChannelMessage::Instruction { content, .. } => content.len(),
        ChannelMessage::ContextUpdate { content, .. } => content.len(),
        ChannelMessage::Signal { .. } => 0,
    };
    if content_len > MAX_CHANNEL_CONTENT_LEN {
        return Err(AppError::BadRequest(format!(
            "message content too large: {content_len} bytes (max {MAX_CHANNEL_CONTENT_LEN})"
        )));
    }
    Ok(())
}

/// `POST /api/sessions/{id}/channel/send`
pub async fn channel_send(
    State(state): State<Arc<LocalAppState>>,
    Path(session_id): Path<String>,
    Json(message): Json<ChannelMessage>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    validate_channel_message(&message)?;

    let bridge = state.channel_bridge.lock().await;
    if !bridge.is_available(&parsed_session_id) {
        return Err(AppError::NotFound(format!(
            "no channel connection for session {session_id}"
        )));
    }

    bridge
        .send(&parsed_session_id, &message)
        .await
        .map_err(|e| AppError::Internal(format!("channel send failed: {e}")))?;

    Ok(StatusCode::ACCEPTED)
}

/// Request body for permission response.
#[derive(Debug, Deserialize)]
pub struct PermissionResponseBody {
    pub allowed: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /api/sessions/{id}/channel/permission/{request_id}`
pub async fn permission_respond(
    State(state): State<Arc<LocalAppState>>,
    Path((session_id, request_id)): Path<(String, String)>,
    Json(body): Json<PermissionResponseBody>,
) -> Result<impl IntoResponse, AppError> {
    if request_id.is_empty() || request_id.len() > 128 {
        return Err(AppError::BadRequest(
            "request_id must be 1-128 characters".to_string(),
        ));
    }

    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let bridge = state.channel_bridge.lock().await;
    if !bridge.is_available(&parsed_session_id) {
        return Err(AppError::NotFound(format!(
            "no channel connection for session {session_id}"
        )));
    }

    bridge
        .respond_permission(
            &parsed_session_id,
            &request_id,
            body.allowed,
            body.reason.as_deref(),
        )
        .await
        .map_err(|e| AppError::Internal(format!("permission response failed: {e}")))?;

    Ok(StatusCode::ACCEPTED)
}

/// `GET /api/sessions/{id}/channel/status`
pub async fn channel_status(
    State(state): State<Arc<LocalAppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let bridge = state.channel_bridge.lock().await;
    let available = bridge.is_available(&parsed_session_id);

    Ok(Json(serde_json::json!({ "available": available })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use crate::local::upsert_local_host;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
        )
    }

    fn build_test_router(state: Arc<LocalAppState>) -> Router {
        Router::new()
            .route(
                "/api/sessions/{session_id}/channel/send",
                post(channel_send),
            )
            .route(
                "/api/sessions/{session_id}/channel/permission/{request_id}",
                post(permission_respond),
            )
            .route(
                "/api/sessions/{session_id}/channel/status",
                get(channel_status),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn channel_status_no_bridge() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let app = build_test_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}/channel/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["available"], false);
    }

    #[tokio::test]
    async fn channel_status_invalid_session_id() {
        let state = test_state().await;
        let app = build_test_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/sessions/not-a-uuid/channel/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn channel_send_not_connected() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let app = build_test_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session_id}/channel/send"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"type":"Signal","action":"continue"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn permission_respond_not_connected() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let app = build_test_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/sessions/{session_id}/channel/permission/req-1"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"allowed": true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn permission_respond_invalid_request_id() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let long_id = "a".repeat(200);
        let app = build_test_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/sessions/{session_id}/channel/permission/{long_id}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"allowed": true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
