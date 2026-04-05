use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::queries::permission_policy as policy_q;
use zremote_core::queries::sessions as session_q;
use zremote_protocol::ServerMessage;
use zremote_protocol::channel::{ChannelMessage, ChannelServerAction};

use crate::error::AppError;
use crate::state::AppState;

/// `GET /api/projects/{id}/permission-policy`
pub async fn get_policy(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<policy_q::PermissionPolicy>, AppError> {
    let policy = policy_q::get_policy(&state.db, &project_id)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "permission policy for project {project_id} not found"
            ))
        })?;
    Ok(Json(policy))
}

/// `PUT /api/projects/{id}/permission-policy`
pub async fn upsert_policy(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    Json(mut body): Json<policy_q::PermissionPolicy>,
) -> Result<impl IntoResponse, AppError> {
    // Ensure the path parameter takes precedence
    body.project_id = project_id;
    policy_q::upsert_policy(&state.db, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/projects/{id}/permission-policy`
pub async fn delete_policy(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let deleted = policy_q::delete_policy(&state.db, &project_id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound(format!(
            "permission policy for project {project_id} not found"
        )))
    }
}

/// `POST /api/sessions/{id}/channel/send`
pub async fn channel_send(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(message): Json<ChannelMessage>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let (_id, host_id_str) = session_q::find_session_for_close(&state.db, &session_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("session {session_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| {
            AppError::Conflict("host is offline, cannot send channel message".to_string())
        })?;

    let msg = ServerMessage::ChannelAction(ChannelServerAction::ChannelSend {
        session_id: parsed_session_id,
        message,
    });

    if sender.send(msg).await.is_err() {
        return Err(AppError::Conflict(
            "host went offline, cannot send channel message".to_string(),
        ));
    }

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
    State(state): State<Arc<AppState>>,
    Path((session_id, request_id)): Path<(String, String)>,
    Json(body): Json<PermissionResponseBody>,
) -> Result<impl IntoResponse, AppError> {
    // Validate request_id length to prevent oversized payloads
    if request_id.is_empty() || request_id.len() > 128 {
        return Err(AppError::BadRequest(
            "request_id must be 1-128 characters".to_string(),
        ));
    }

    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let (_id, host_id_str) = session_q::find_session_for_close(&state.db, &session_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("session {session_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| {
            AppError::Conflict("host is offline, cannot respond to permission request".to_string())
        })?;

    let msg = ServerMessage::ChannelAction(ChannelServerAction::PermissionResponse {
        session_id: parsed_session_id,
        request_id,
        allowed: body.allowed,
        reason: body.reason,
    });

    if sender.send(msg).await.is_err() {
        return Err(AppError::Conflict(
            "host went offline, cannot respond to permission request".to_string(),
        ));
    }

    Ok(StatusCode::ACCEPTED)
}

/// `GET /api/sessions/{id}/channel/status`
pub async fn channel_status(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _parsed: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let status = session_q::get_session_status(&state.db, &session_id).await?;
    let available = matches!(status.as_deref(), Some("active" | "creating"));

    Ok(Json(serde_json::json!({ "available": available })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, ConnectionManager};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

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
        })
    }

    fn create_router(state: Arc<AppState>) -> axum::Router {
        crate::create_router(state)
    }

    async fn insert_test_host(state: &AppState, id: &str, name: &str, hostname: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, agent_version, os, arch, \
             status, last_seen_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'testhash', '0.1.0', 'linux', 'x86_64', 'online', \
             '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(name)
        .bind(hostname)
        .execute(&state.db)
        .await
        .unwrap();
    }

    async fn insert_test_session(state: &AppState, session_id: &str, host_id: &str, status: &str) {
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, ?)")
            .bind(session_id)
            .bind(host_id)
            .bind(status)
            .execute(&state.db)
            .await
            .unwrap();
    }

    // -- Permission policy route tests --

    #[tokio::test]
    async fn get_policy_not_found() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get("/api/projects/proj-1/permission-policy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn upsert_and_get_policy() {
        let state = test_state().await;

        // PUT the policy
        let app = create_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/projects/proj-1/permission-policy")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{
                            "project_id": "ignored",
                            "auto_allow": ["Read", "Glob"],
                            "auto_deny": ["Bash*"],
                            "escalation_timeout_secs": 60,
                            "escalation_targets": ["gui"],
                            "updated_at": ""
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // GET the policy
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get("/api/projects/proj-1/permission-policy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let policy: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(policy["project_id"], "proj-1");
        assert_eq!(policy["auto_allow"], serde_json::json!(["Read", "Glob"]));
        assert_eq!(policy["auto_deny"], serde_json::json!(["Bash*"]));
    }

    #[tokio::test]
    async fn delete_policy_success() {
        let state = test_state().await;

        // Insert a policy first
        let policy = policy_q::PermissionPolicy {
            project_id: "proj-1".to_string(),
            auto_allow: vec![],
            auto_deny: vec![],
            escalation_timeout_secs: 30,
            escalation_targets: vec!["gui".to_string()],
            updated_at: String::new(),
        };
        policy_q::upsert_policy(&state.db, &policy).await.unwrap();

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete("/api/projects/proj-1/permission-policy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_policy_not_found() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete("/api/projects/nonexistent/permission-policy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // -- Channel send tests --

    #[tokio::test]
    async fn channel_send_invalid_session_id() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sessions/not-a-uuid/channel/send")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"type": "Signal", "action": "continue"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn channel_send_session_not_found() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session_id}/channel/send"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"type": "Signal", "action": "continue"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn channel_send_agent_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id, &host_id, "active").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session_id}/channel/send"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"type": "Signal", "action": "continue"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    // -- Permission respond tests --

    #[tokio::test]
    async fn permission_respond_invalid_session_id() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sessions/not-a-uuid/channel/permission/req-1")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"allowed": true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn permission_respond_session_not_found() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let app = create_router(state);
        let response = app
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

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn permission_respond_agent_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id, &host_id, "active").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/sessions/{session_id}/channel/permission/req-1"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"allowed": false, "reason": "denied by policy"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    // -- Channel status tests --

    #[tokio::test]
    async fn channel_status_invalid_session_id() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get("/api/sessions/not-a-uuid/channel/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn channel_status_active_session() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id, &host_id, "active").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}/channel/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["available"], true);
    }

    #[tokio::test]
    async fn channel_status_closed_session() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id, &host_id, "closed").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}/channel/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["available"], false);
    }

    #[tokio::test]
    async fn channel_status_nonexistent_session() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/sessions/{session_id}/channel/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["available"], false);
    }
}
