use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use zremote_protocol::channel::ChannelMessage;

use super::types::{ChannelState, StdioEvent};

/// Build the Axum router for the channel HTTP server.
pub fn router(state: ChannelState) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .route("/notify", post(handle_notify))
        .route("/permission-response", post(handle_permission_response))
        .with_state(state)
}

async fn handle_health() -> StatusCode {
    StatusCode::OK
}

async fn handle_notify(
    State(state): State<ChannelState>,
    Json(message): Json<ChannelMessage>,
) -> StatusCode {
    if state
        .stdio_tx
        .send(StdioEvent::ChannelNotify(message))
        .await
        .is_err()
    {
        tracing::error!("failed to send channel notify to stdio loop");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::OK
}

#[derive(serde::Deserialize)]
struct PermissionResponsePayload {
    request_id: String,
    allowed: bool,
    #[serde(default)]
    reason: Option<String>,
}

async fn handle_permission_response(
    State(state): State<ChannelState>,
    Json(payload): Json<PermissionResponsePayload>,
) -> StatusCode {
    if state
        .stdio_tx
        .send(StdioEvent::PermissionResponse {
            request_id: payload.request_id,
            allowed: payload.allowed,
            reason: payload.reason,
        })
        .await
        .is_err()
    {
        tracing::error!("failed to send permission response to stdio loop");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    use uuid::Uuid;

    fn test_state() -> ChannelState {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        ChannelState {
            session_id: Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:9999".to_string(),
            stdio_tx: tx,
            http_client: reqwest::Client::new(),
        }
    }

    #[tokio::test]
    async fn health_returns_200() {
        let app: axum::Router<()> = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn notify_sends_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let state = ChannelState {
            session_id: Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:9999".to_string(),
            stdio_tx: tx,
            http_client: reqwest::Client::new(),
        };
        let app: axum::Router<()> = router(state);

        let body = serde_json::json!({
            "type": "Instruction",
            "from": "commander",
            "content": "Do the thing"
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/notify")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let evt = rx.try_recv().unwrap();
        assert!(matches!(evt, StdioEvent::ChannelNotify(_)));
    }

    #[tokio::test]
    async fn notify_invalid_json_returns_error() {
        let app: axum::Router<()> = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/notify")
                    .header("content-type", "application/json")
                    .body(Body::from(b"not json".to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status().is_client_error());
    }

    #[tokio::test]
    async fn permission_response_sends_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let state = ChannelState {
            session_id: Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:9999".to_string(),
            stdio_tx: tx,
            http_client: reqwest::Client::new(),
        };
        let app: axum::Router<()> = router(state);

        let body = serde_json::json!({
            "request_id": "perm-123",
            "allowed": true,
            "reason": "auto-approved"
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/permission-response")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let evt = rx.try_recv().unwrap();
        match evt {
            StdioEvent::PermissionResponse {
                request_id,
                allowed,
                reason,
            } => {
                assert_eq!(request_id, "perm-123");
                assert!(allowed);
                assert_eq!(reason.unwrap(), "auto-approved");
            }
            _ => panic!("expected PermissionResponse"),
        }
    }

    #[tokio::test]
    async fn permission_response_without_reason() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let state = ChannelState {
            session_id: Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:9999".to_string(),
            stdio_tx: tx,
            http_client: reqwest::Client::new(),
        };
        let app: axum::Router<()> = router(state);

        let body = serde_json::json!({
            "request_id": "perm-456",
            "allowed": false
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/permission-response")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let evt = rx.try_recv().unwrap();
        match evt {
            StdioEvent::PermissionResponse {
                allowed, reason, ..
            } => {
                assert!(!allowed);
                assert!(reason.is_none());
            }
            _ => panic!("expected PermissionResponse"),
        }
    }

    #[tokio::test]
    async fn notify_with_dropped_receiver_returns_500() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let state = ChannelState {
            session_id: Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:9999".to_string(),
            stdio_tx: tx,
            http_client: reqwest::Client::new(),
        };
        drop(rx);
        let app: axum::Router<()> = router(state);

        let body = serde_json::json!({
            "type": "Signal",
            "action": "continue"
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/notify")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
