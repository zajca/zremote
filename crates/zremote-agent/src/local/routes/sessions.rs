use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_core::queries::sessions as q;
use zremote_core::state::{ServerEvent, SessionInfo, SessionState};

use crate::local::state::LocalAppState;

/// Resolve the default shell from the passwd database, falling back to $SHELL
/// and then `/bin/sh`.
pub(crate) fn default_shell() -> &'static str {
    static SHELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SHELL.get_or_init(|| {
        login_shell_from_passwd()
            .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()))
    })
}

/// Read the current user's login shell from the passwd database.
fn login_shell_from_passwd() -> Option<String> {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    let output = std::process::Command::new("getent")
        .args(["passwd", uid.trim()])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    let shell = output.trim().rsplit(':').next()?;
    if shell.is_empty() {
        return None;
    }
    Some(shell.to_string())
}

/// Request body for creating a new session.
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub shell: Option<String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub working_dir: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub initial_command: Option<String>,
}

/// `POST /api/hosts/:host_id/sessions` - create a new terminal session.
pub async fn create_session(
    State(state): State<Arc<LocalAppState>>,
    Path(host_id): Path<String>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_host_id: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    if parsed_host_id != state.host_id {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();

    // Resolve project_id from working_dir
    let project_id: Option<String> = if let Some(ref wd) = body.working_dir {
        q::resolve_project_id(&state.db, &host_id, wd).await?
    } else {
        None
    };

    // Insert session row into DB
    q::insert_session(
        &state.db,
        &session_id_str,
        &host_id,
        body.name.as_deref(),
        body.working_dir.as_deref(),
        project_id.as_deref(),
    )
    .await?;

    // Read project settings if working_dir is provided
    let (settings, settings_warning) = if let Some(ref wd) = body.working_dir {
        match crate::project::settings::read_settings(std::path::Path::new(wd)) {
            Ok(s) => (s, None),
            Err(e) => {
                tracing::warn!(working_dir = %wd, error = %e, "failed to read project settings");
                (None, Some(e))
            }
        }
    } else {
        (None, None)
    };

    // Apply overrides from settings
    let effective_shell = settings
        .as_ref()
        .and_then(|s| s.shell.as_deref())
        .or(body.shell.as_deref())
        .unwrap_or(default_shell());

    let effective_working_dir = settings
        .as_ref()
        .and_then(|s| s.working_dir.as_deref())
        .or(body.working_dir.as_deref());

    let env_vars = settings.as_ref().map(|s| &s.env).filter(|e| !e.is_empty());

    let cols = body.cols.unwrap_or(80);
    let rows = body.rows.unwrap_or(24);

    // Create in-memory session state
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, parsed_host_id));
    }

    // Spawn PTY/tmux session directly
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(
            session_id,
            effective_shell,
            cols,
            rows,
            effective_working_dir,
            env_vars,
        )
        .map_err(|e| AppError::Internal(format!("failed to spawn PTY: {e}")))?
    };

    tracing::info!(
        session_id = %session_id,
        pid = pid,
        shell = effective_shell,
        env_count = env_vars.map(|e| e.len()).unwrap_or(0),
        "local session created"
    );

    // Update DB: status -> active, shell, pid
    sqlx::query("UPDATE sessions SET status = 'active', shell = ?, pid = ? WHERE id = ?")
        .bind(effective_shell)
        .bind(i64::from(pid))
        .bind(&session_id_str)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    // Update in-memory status
    {
        let mut sessions = state.sessions.write().await;
        if let Some(session_state) = sessions.get_mut(&session_id) {
            session_state.status = "active".to_string();
        }
    }

    // Broadcast SessionCreated event
    let _ = state.events.send(ServerEvent::SessionCreated {
        session: SessionInfo {
            id: session_id_str.clone(),
            host_id: host_id.clone(),
            shell: Some(effective_shell.to_string()),
            status: "active".to_string(),
        },
    });

    // Write initial command to PTY after a short delay for shell init
    if let Some(ref cmd) = body.initial_command {
        let cmd_with_newline = format!("{cmd}\n");
        let state_clone = state.clone();
        let sid = session_id;
        let cmd_bytes = cmd_with_newline.into_bytes();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let mut mgr = state_clone.session_manager.lock().await;
            if let Err(e) = mgr.write_to(&sid, &cmd_bytes) {
                tracing::warn!(session_id = %sid, error = %e, "failed to write initial_command to PTY");
            }
        });
    }

    let response = serde_json::json!({
        "id": session_id_str,
        "status": "active",
        "shell": effective_shell,
        "pid": pid,
        "applied_settings": {
            "shell": effective_shell,
            "env_count": env_vars.map(|e| e.len()).unwrap_or(0),
            "working_dir": effective_working_dir,
        },
        "settings_warning": settings_warning,
    });

    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/hosts/:host_id/sessions` - list sessions for a host.
pub async fn list_sessions(
    State(state): State<Arc<LocalAppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<Vec<q::SessionRow>>, AppError> {
    let _parsed: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    let sessions = q::list_sessions(&state.db, &host_id).await?;
    Ok(Json(sessions))
}

/// `GET /api/sessions/:session_id` - get session detail.
pub async fn get_session(
    State(state): State<Arc<LocalAppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<q::SessionRow>, AppError> {
    let _parsed: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let session = q::get_session(&state.db, &session_id).await?;
    Ok(Json(session))
}

/// Request body for updating a session.
#[derive(Debug, Deserialize)]
pub struct UpdateSessionRequest {
    pub name: Option<String>,
}

/// `PATCH /api/sessions/:session_id` - update session metadata.
pub async fn update_session(
    State(state): State<Arc<LocalAppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<UpdateSessionRequest>,
) -> Result<Json<q::SessionRow>, AppError> {
    let _parsed: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    q::update_session_name(&state.db, &session_id, body.name.as_deref()).await?;
    let session = q::get_session(&state.db, &session_id).await?;
    Ok(Json(session))
}

/// `DELETE /api/sessions/:session_id` - close a session.
pub async fn close_session(
    State(state): State<Arc<LocalAppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let (_id, _host_id_str) = q::find_session_for_close(&state.db, &session_id)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!("session {session_id} not found or already closed"))
        })?;

    // Close session in session manager
    let exit_code = {
        let mut mgr = state.session_manager.lock().await;
        mgr.close(&parsed_session_id)
    };

    tracing::info!(
        session_id = %session_id,
        exit_code = ?exit_code,
        "local session closed"
    );

    // Update DB status
    sqlx::query(
        "UPDATE sessions SET status = 'closed', exit_code = ?, closed_at = datetime('now') WHERE id = ?",
    )
    .bind(exit_code)
    .bind(&session_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    // Notify browser clients and remove from store
    {
        let mut sessions = state.sessions.write().await;
        if let Some(session_state) = sessions.get_mut(&parsed_session_id) {
            let msg = zremote_core::state::BrowserMessage::SessionClosed { exit_code };
            session_state
                .browser_senders
                .retain(|tx| match tx.try_send(msg.clone()) {
                    Ok(()) => true,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                });
        }
        sessions.remove(&parsed_session_id);
    }

    // Broadcast event
    let _ = state.events.send(ServerEvent::SessionClosed {
        session_id: session_id.clone(),
        exit_code,
    });

    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /api/sessions/:session_id/purge` - permanently delete a closed session.
pub async fn purge_session(
    State(state): State<Arc<LocalAppState>>,
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
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{delete, get, post};
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
        LocalAppState::new(pool, "test-host".to_string(), host_id, shutdown, false)
    }

    fn build_test_router(state: Arc<LocalAppState>) -> Router {
        Router::new()
            .route(
                "/api/hosts/{host_id}/sessions",
                post(create_session).get(list_sessions),
            )
            .route(
                "/api/sessions/{session_id}",
                get(get_session).patch(update_session).delete(close_session),
            )
            .route("/api/sessions/{session_id}/purge", delete(purge_session))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_sessions_empty() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn list_sessions_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/hosts/not-a-uuid/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_session_wrong_host_returns_404() {
        let state = test_state().await;
        let wrong_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{wrong_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "cols": 80,
                            "rows": 24,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_session_nonexistent_returns_404() {
        let state = test_state().await;
        let session_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_session_invalid_uuid_returns_400() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn close_nonexistent_session_returns_404() {
        let state = test_state().await;
        let session_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn purge_nonexistent_session_returns_404() {
        let state = test_state().await;
        let session_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}/purge"))
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
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4();
        let session_id_str = session_id.to_string();

        // Insert a session with active status directly
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id_str)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}/purge"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn purge_closed_session_succeeds() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4();
        let session_id_str = session_id.to_string();

        // Insert a closed session directly
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'closed')")
            .bind(&session_id_str)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}/purge"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn update_session_name() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4();
        let session_id_str = session_id.to_string();

        // Insert a session directly
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id_str)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/sessions/{session_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "name": "my session"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "my session");
    }

    #[tokio::test]
    async fn create_session_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

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
    async fn close_session_invalid_uuid() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/sessions/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_session_invalid_uuid() {
        let state = test_state().await;
        let app = build_test_router(state);

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
    async fn purge_invalid_uuid() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/sessions/not-a-uuid/purge")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_sessions_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Insert sessions directly
        for i in 0..3 {
            let session_id = Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO sessions (id, host_id, status, name) VALUES (?, ?, 'active', ?)",
            )
            .bind(&session_id)
            .bind(&host_id)
            .bind(format!("session-{i}"))
            .execute(&state.db)
            .await
            .unwrap();
        }

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 3);
    }

    #[tokio::test]
    async fn get_session_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4();
        let session_id_str = session_id.to_string();

        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, name) VALUES (?, ?, 'active', 'my-session')",
        )
        .bind(&session_id_str)
        .bind(&host_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{session_id}"))
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
        assert_eq!(json["id"], session_id_str);
        assert_eq!(json["name"], "my-session");
        assert_eq!(json["status"], "active");
    }

    #[tokio::test]
    async fn update_session_clear_name() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4();
        let session_id_str = session_id.to_string();

        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, name) VALUES (?, ?, 'active', 'old-name')",
        )
        .bind(&session_id_str)
        .bind(&host_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        // Set name to null
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/sessions/{session_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["name"].is_null());
    }

    #[tokio::test]
    async fn close_already_closed_session_returns_404() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4();
        let session_id_str = session_id.to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'closed')")
            .bind(&session_id_str)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // find_session_for_close excludes status='closed'
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn purge_creating_session_returns_conflict() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4();
        let session_id_str = session_id.to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'creating')")
            .bind(&session_id_str)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}/purge"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_session_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "cols": 80,
                            "rows": 24,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "active");
        assert!(!json["id"].as_str().unwrap().is_empty());
        assert!(json["pid"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn create_session_with_custom_shell() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "shell": "/bin/sh",
                            "cols": 120,
                            "rows": 40,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["shell"], "/bin/sh");
    }

    #[tokio::test]
    async fn create_session_with_working_dir() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let dir = tempfile::tempdir().unwrap();
        let wd = dir.path().to_str().unwrap().to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "cols": 80,
                            "rows": 24,
                            "working_dir": wd,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_session_with_name() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "cols": 80,
                            "rows": 24,
                            "name": "my-dev-session",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_session_default_cols_rows() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        // Omit cols and rows - should use defaults (80x24)
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_and_close_session() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        // Create session
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "cols": 80,
                            "rows": 24,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = json["id"].as_str().unwrap().to_string();

        // Close session
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);

        // Verify session is closed
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{session_id}"))
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
        assert_eq!(json["status"], "closed");
    }

    #[tokio::test]
    async fn create_and_list_sessions() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        // Create two sessions
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/hosts/{host_id}/sessions"))
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
        }

        // List sessions
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 2);
    }

    #[tokio::test]
    async fn create_close_and_purge_session() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        // Create session
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = json["id"].as_str().unwrap().to_string();

        // Close session
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        // Purge session
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}/purge"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify session is gone
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_session_and_get_detail() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "cols": 80,
                            "rows": 24,
                            "name": "test-session",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = json["id"].as_str().unwrap().to_string();

        // Get session detail
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{session_id}"))
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
        assert_eq!(json["id"], session_id);
        assert_eq!(json["status"], "active");
        assert_eq!(json["name"], "test-session");
        assert_eq!(json["host_id"], host_id);
    }

    #[tokio::test]
    async fn update_session_nonexistent() {
        let state = test_state().await;
        let session_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/sessions/{session_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn purge_suspended_session_returns_conflict() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4();
        let session_id_str = session_id.to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'suspended')")
            .bind(&session_id_str)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{session_id}/purge"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}
