use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::queries::sessions as q;
use zremote_protocol::ServerMessage;

use crate::error::AppError;
use crate::state::{AppState, SessionState};

// Re-export the core row type so other modules (projects.rs) can use it.
pub type SessionResponse = q::SessionRow;

/// Request body for creating a new session.
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub shell: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub working_dir: Option<String>,
    pub name: Option<String>,
}

/// `POST /api/hosts/:host_id/sessions` - create a new terminal session.
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_host_id: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    if !q::host_exists(&state.db, &host_id).await? {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    // Check agent is online
    let sender = state
        .connections
        .get_sender(&parsed_host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline, cannot create session".to_string()))?;

    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();

    // Resolve project_id from working_dir
    let project_id: Option<String> = if let Some(ref wd) = body.working_dir {
        q::resolve_project_id(&state.db, &host_id, wd).await?
    } else {
        None
    };

    q::insert_session(
        &state.db,
        &session_id_str,
        &host_id,
        body.name.as_deref(),
        body.working_dir.as_deref(),
        project_id.as_deref(),
    )
    .await?;

    // Create in-memory session state
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, parsed_host_id));
    }

    // Send SessionCreate to agent
    let msg = ServerMessage::SessionCreate {
        session_id,
        shell: body.shell,
        cols: body.cols,
        rows: body.rows,
        working_dir: body.working_dir,
        env: None,
    };

    if sender.send(msg).await.is_err() {
        return Err(AppError::Conflict(
            "host went offline, cannot create session".to_string(),
        ));
    }

    let response = serde_json::json!({
        "id": session_id_str,
        "status": "creating",
    });

    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/hosts/:host_id/sessions` - list sessions for a host.
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let _parsed: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    let sessions = q::list_sessions(&state.db, &host_id).await?;
    Ok(Json(sessions))
}

/// `GET /api/sessions/:session_id` - get session detail.
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionResponse>, AppError> {
    let _parsed: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let session = q::get_session(&state.db, &session_id).await?;
    Ok(Json(session))
}

#[derive(Debug, Deserialize)]
pub struct UpdateSessionRequest {
    pub name: Option<String>,
}

/// `PATCH /api/sessions/:session_id` - update session metadata.
pub async fn update_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<UpdateSessionRequest>,
) -> Result<Json<SessionResponse>, AppError> {
    let _parsed: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    q::update_session_name(&state.db, &session_id, body.name.as_deref()).await?;
    let session = q::get_session(&state.db, &session_id).await?;
    Ok(Json(session))
}

/// `DELETE /api/sessions/:session_id` - close a session.
pub async fn close_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let (_id, host_id_str) = q::find_session_for_close(&state.db, &session_id)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!("session {session_id} not found or already closed"))
        })?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    // Send SessionClose to agent if connected
    if let Some(sender) = state.connections.get_sender(&host_id).await {
        let msg = ServerMessage::SessionClose {
            session_id: parsed_session_id,
        };
        let _ = sender.send(msg).await;
    }

    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /api/sessions/:session_id/purge` - permanently delete a closed session.
pub async fn purge_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    // Only allow purging closed sessions
    match q::get_session_status(&state.db, &session_id).await? {
        None => {
            return Err(AppError::NotFound(format!(
                "session {session_id} not found"
            )));
        }
        Some(ref s) if s != "closed" => {
            return Err(AppError::Conflict(format!(
                "session {session_id} is not closed (status: {s}), cannot purge"
            )));
        }
        _ => {}
    }

    q::purge_session(&state.db, &session_id).await?;

    Ok(StatusCode::NO_CONTENT)
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

    #[tokio::test]
    async fn create_session_invalid_host_uuid_returns_bad_request() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/hosts/not-a-uuid/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cols": 80, "rows": 24}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_sessions_invalid_host_uuid_returns_bad_request() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get("/api/hosts/not-a-uuid/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_session_invalid_uuid_returns_bad_request() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get("/api/sessions/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn close_session_invalid_uuid_returns_bad_request() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete("/api/sessions/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn close_session_when_agent_offline_still_returns_202() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4();
        let host_id_str = host_id.to_string();
        insert_test_host(&state, &host_id_str, "host", "host").await;

        let session_id = uuid::Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id, &host_id_str, "active").await;

        // No connection registered -- agent is offline
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // close_session sends to agent if connected but still returns ACCEPTED
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn update_session_invalid_uuid_returns_bad_request() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/sessions/not-a-uuid")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_session_sets_name() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = uuid::Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id, &host_id, "active").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/sessions/{session_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "renamed"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let session: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(session["name"], "renamed");
    }

    #[tokio::test]
    async fn purge_session_invalid_uuid_returns_bad_request() {
        let state = test_state().await;
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete("/api/sessions/not-a-uuid/purge")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn purge_session_not_found_returns_404() {
        let state = test_state().await;
        let session_id = uuid::Uuid::new_v4().to_string();
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete(format!("/api/sessions/{session_id}/purge"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn purge_active_session_returns_conflict() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = uuid::Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id, &host_id, "active").await;

        let app = create_router(state);
        let response = app
            .oneshot(
                Request::delete(format!("/api/sessions/{session_id}/purge"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn purge_closed_session_returns_no_content() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_test_host(&state, &host_id, "host", "host").await;

        let session_id = uuid::Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id, &host_id, "closed").await;

        let app = create_router(state.clone());
        let response = app
            .oneshot(
                Request::delete(format!("/api/sessions/{session_id}/purge"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify session is actually gone
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(&session_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn list_sessions_for_nonexistent_host_returns_empty() {
        let state = test_state().await;
        // Host ID is valid UUID but not in DB -- list_sessions doesn't check host existence
        let host_id = uuid::Uuid::new_v4().to_string();
        let app = create_router(state);
        let response = app
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}/sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let sessions: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(sessions.is_empty());
    }
}
