use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use zremote_protocol::channel::{ChannelAgentAction, ChannelResponse};
use zremote_protocol::{AgentMessage, SessionId};

use super::handler::HooksState;

/// Payload for channel callback routes. The session_id comes from the
/// channel server's environment and is included in the JSON body.
#[derive(Debug, Deserialize)]
pub(crate) struct ChannelCallbackPayload<T> {
    session_id: SessionId,
    #[serde(flatten)]
    inner: T,
}

/// POST /channel/reply — Worker sent a reply via `zremote_reply` tool.
pub async fn handle_channel_reply(
    State(state): State<HooksState>,
    Json(payload): Json<ChannelCallbackPayload<ChannelResponse>>,
) -> impl IntoResponse {
    let msg = AgentMessage::ChannelAction(ChannelAgentAction::WorkerResponse {
        session_id: payload.session_id,
        response: payload.inner,
    });
    send_outbound(&state, msg)
}

/// Payload for permission requests from CC workers.
#[derive(Debug, Deserialize)]
pub struct PermissionRequestPayload {
    pub session_id: SessionId,
    pub request_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
}

/// POST /channel/permission-request — Worker hit a permission prompt.
pub async fn handle_channel_permission_request(
    State(state): State<HooksState>,
    Json(payload): Json<PermissionRequestPayload>,
) -> impl IntoResponse {
    let msg = AgentMessage::ChannelAction(ChannelAgentAction::PermissionRequest {
        session_id: payload.session_id,
        request_id: payload.request_id,
        tool_name: payload.tool_name,
        tool_input: payload.tool_input,
    });
    send_outbound(&state, msg)
}

/// Payload for channel status updates.
#[derive(Debug, Deserialize)]
pub struct ChannelStatusPayload {
    pub session_id: SessionId,
    pub available: bool,
}

/// POST /channel/status — Channel server availability changed.
pub async fn handle_channel_status(
    State(state): State<HooksState>,
    Json(payload): Json<ChannelStatusPayload>,
) -> impl IntoResponse {
    let msg = AgentMessage::ChannelAction(ChannelAgentAction::ChannelStatus {
        session_id: payload.session_id,
        available: payload.available,
    });
    send_outbound(&state, msg)
}

/// Send an agent message via the outbound channel.
fn send_outbound(state: &HooksState, msg: AgentMessage) -> StatusCode {
    if state.outbound_tx.try_send(msg).is_err() {
        tracing::warn!("outbound channel full, channel callback message dropped");
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use tokio::sync::mpsc;
    use tower::ServiceExt;
    use uuid::Uuid;
    use zremote_protocol::AgenticAgentMessage;

    use crate::hooks::context::HookContextProvider;
    use crate::hooks::mapper::SessionMapper;
    use crate::knowledge::context_delivery::DeliveryCoordinator;

    fn test_state() -> (HooksState, mpsc::Receiver<AgentMessage>) {
        let (agentic_tx, _) = mpsc::channel::<AgenticAgentMessage>(64);
        let (outbound_tx, outbound_rx) = mpsc::channel::<AgentMessage>(64);
        let mapper = SessionMapper::new();
        let state = HooksState {
            context_provider: HookContextProvider::new(mapper.clone()),
            delivery_coordinator: Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
            agentic_tx,
            mapper,
            outbound_tx,
            sent_cc_session_ids: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
        };
        (state, outbound_rx)
    }

    fn channel_router(state: HooksState) -> Router {
        Router::new()
            .route("/channel/reply", post(handle_channel_reply))
            .route(
                "/channel/permission-request",
                post(handle_channel_permission_request),
            )
            .route("/channel/status", post(handle_channel_status))
            .with_state(state)
    }

    #[tokio::test]
    async fn reply_route_sends_worker_response() {
        let (state, mut rx) = test_state();
        let app: axum::Router<()> = channel_router(state);
        let session_id = Uuid::new_v4();

        let body = serde_json::json!({
            "session_id": session_id,
            "type": "Reply",
            "message": "Tests fixed",
            "metadata": {}
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/channel/reply")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let msg = rx.try_recv().unwrap();
        match msg {
            AgentMessage::ChannelAction(ChannelAgentAction::WorkerResponse {
                session_id: sid,
                response,
            }) => {
                assert_eq!(sid, session_id);
                assert!(matches!(response, ChannelResponse::Reply { .. }));
            }
            other => panic!("expected WorkerResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn status_report_route() {
        let (state, mut rx) = test_state();
        let app: axum::Router<()> = channel_router(state);
        let session_id = Uuid::new_v4();

        let body = serde_json::json!({
            "session_id": session_id,
            "type": "StatusReport",
            "status": "completed",
            "summary": "All done"
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/channel/reply")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let msg = rx.try_recv().unwrap();
        assert!(matches!(
            msg,
            AgentMessage::ChannelAction(ChannelAgentAction::WorkerResponse { .. })
        ));
    }

    #[tokio::test]
    async fn permission_request_route() {
        let (state, mut rx) = test_state();
        let app: axum::Router<()> = channel_router(state);
        let session_id = Uuid::new_v4();

        let body = serde_json::json!({
            "session_id": session_id,
            "request_id": "req-001",
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /"}
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/channel/permission-request")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let msg = rx.try_recv().unwrap();
        match msg {
            AgentMessage::ChannelAction(ChannelAgentAction::PermissionRequest {
                session_id: sid,
                request_id,
                tool_name,
                ..
            }) => {
                assert_eq!(sid, session_id);
                assert_eq!(request_id, "req-001");
                assert_eq!(tool_name, "Bash");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn channel_status_route() {
        let (state, mut rx) = test_state();
        let app: axum::Router<()> = channel_router(state);
        let session_id = Uuid::new_v4();

        let body = serde_json::json!({
            "session_id": session_id,
            "available": true
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/channel/status")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let msg = rx.try_recv().unwrap();
        match msg {
            AgentMessage::ChannelAction(ChannelAgentAction::ChannelStatus {
                session_id: sid,
                available,
            }) => {
                assert_eq!(sid, session_id);
                assert!(available);
            }
            other => panic!("expected ChannelStatus, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_json_returns_error() {
        let (state, _rx) = test_state();
        let app: axum::Router<()> = channel_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/channel/reply")
                    .header("content-type", "application/json")
                    .body(Body::from(b"not json".to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status().is_client_error());
    }
}
