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
use zremote_protocol::status::SessionStatus;

use crate::local::state::LocalAppState;
use crate::pty::shell_integration::ShellIntegrationConfig;
use crate::shell::resolve_shell;

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

    // Apply overrides from settings. resolve_shell validates the path and
    // falls back to the login shell if the requested one doesn't exist,
    // so per-project settings remain portable across hosts (e.g. NixOS
    // vs. FHS distros, where `/bin/zsh` is not guaranteed to exist).
    let effective_shell_owned = resolve_shell(
        settings
            .as_ref()
            .and_then(|s| s.shell.as_deref())
            .or(body.shell.as_deref()),
    );
    let effective_shell: &str = &effective_shell_owned;

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

    // Spawn PTY/daemon session directly
    let manual_config = ShellIntegrationConfig::for_manual_session();
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(
            session_id,
            effective_shell,
            cols,
            rows,
            effective_working_dir,
            env_vars,
            Some(&manual_config),
        )
        .await
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
            session_state.status = zremote_protocol::status::SessionStatus::Active;
        }
    }

    // Broadcast SessionCreated event
    let _ = state.events.send(ServerEvent::SessionCreated {
        session: SessionInfo {
            id: session_id_str.clone(),
            host_id: host_id.clone(),
            shell: Some(effective_shell.to_string()),
            status: SessionStatus::Active,
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

    let found = q::find_session_for_close(&state.db, &session_id).await?;
    if found.is_none() {
        return Err(AppError::NotFound(format!(
            "session {session_id} not found or already closed"
        )));
    }

    close_session_internal(&state, &session_id, parsed_session_id).await?;

    Ok(StatusCode::ACCEPTED)
}

/// Gracefully close a single session: send SIGTERM to the PTY process, update
/// the DB row, notify browser clients, and broadcast a `SessionClosed` event.
///
/// This is the shared path used by the HTTP handler and by bulk operations
/// (e.g. tearing down all sessions bound to a worktree before removing it).
/// Safe to call on an already-closed session: the UPDATE is guarded with
/// `status != 'closed'` so a race with a concurrent `DELETE /api/sessions/:id`
/// won't emit a second `SessionClosed` event or rewrite `exit_code`.
pub(crate) async fn close_session_internal(
    state: &Arc<LocalAppState>,
    session_id: &str,
    parsed_session_id: Uuid,
) -> Result<(), AppError> {
    // Close session in session manager
    let exit_code = {
        let mut mgr = state.session_manager.lock().await;
        mgr.close(&parsed_session_id)
    };

    // Guard the UPDATE with `status != 'closed'` so a racing close (e.g. the
    // user clicking X on a terminal while a worktree deletion is in flight)
    // doesn't produce a second row update + duplicate event. The affected-row
    // count tells us whether *we* were the one that actually transitioned it.
    let result = sqlx::query(
        "UPDATE sessions SET status = 'closed', exit_code = ?, closed_at = datetime('now') \
         WHERE id = ? AND status != 'closed'",
    )
    .bind(exit_code)
    .bind(session_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    if result.rows_affected() == 0 {
        tracing::debug!(
            session_id = %session_id,
            "session already closed; skipping browser notify + event"
        );
        return Ok(());
    }

    tracing::info!(
        session_id = %session_id,
        exit_code = ?exit_code,
        "local session closed"
    );

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
        session_id: session_id.to_string(),
        exit_code,
    });

    Ok(())
}

/// Close every active session attached to `project_id` or living inside
/// `path_scope` (when provided).
///
/// Used before destroying a project/worktree so the PTY children release the
/// working directory and the GUI observes a clean `SessionClosed` event
/// instead of a terminal that silently stops responding. The `path_scope`
/// fallback catches legacy rows that were tagged with the parent project's
/// id (possible when a worktree lives inside its parent repo) so the real
/// sessions-in-a-worktree still get torn down.
pub(crate) async fn close_sessions_for_project(
    state: &Arc<LocalAppState>,
    host_id: &str,
    project_id: &str,
    path_scope: Option<&str>,
) -> Result<usize, AppError> {
    // Collect session ids from both lookups, dedup, and drop already-closed
    // rows. Order doesn't matter — each close is independent.
    //
    // `seen` tracks only sessions we intend to close. Closed rows are skipped
    // before insertion so they never participate in deduplication (they would
    // never reappear via `list_active_sessions_under_path` anyway, but being
    // explicit here avoids relying on that coincidence).
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut targets: Vec<(String, Uuid)> = Vec::new();

    let by_project = q::list_sessions_by_project(&state.db, project_id).await?;
    for row in by_project {
        if row.status == "closed" {
            continue;
        }
        if !seen.insert(row.id.clone()) {
            continue;
        }
        let Ok(parsed) = row.id.parse::<Uuid>() else {
            tracing::warn!(session_id = %row.id, "skipping session with unparseable id");
            continue;
        };
        targets.push((row.id, parsed));
    }

    if let Some(path) = path_scope {
        let by_path = q::list_active_sessions_under_path(&state.db, host_id, path).await?;
        for row in by_path {
            if !seen.insert(row.id.clone()) {
                continue;
            }
            let Ok(parsed) = row.id.parse::<Uuid>() else {
                tracing::warn!(session_id = %row.id, "skipping session with unparseable id");
                continue;
            };
            targets.push((row.id, parsed));
        }
    }

    let mut closed = 0usize;
    for (id_str, parsed) in targets {
        // Per-session errors must not abort the batch: if session 2 of 3 fails
        // to close, we still want session 3 shut down so `git worktree remove`
        // isn't blocked by the one remaining PTY. Log and continue.
        if let Err(e) = close_session_internal(state, &id_str, parsed).await {
            tracing::warn!(
                session_id = %id_str,
                error = %e,
                "failed to close session during bulk close; continuing"
            );
            continue;
        }
        closed += 1;
    }
    Ok(closed)
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

/// Request body for pushing context to a session.
#[derive(Debug, Deserialize)]
pub struct ContextPushRequest {
    #[serde(default)]
    pub memories: Vec<String>,
    #[serde(default)]
    pub conventions: Vec<String>,
}

/// `POST /api/sessions/:session_id/context/push` - push context to a running session.
pub async fn push_context(
    State(state): State<Arc<LocalAppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<ContextPushRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    // Verify session exists and is active
    let session_status = q::get_session_status(&state.db, &session_id).await?;
    match session_status {
        None => {
            return Err(AppError::NotFound(format!(
                "session {session_id} not found"
            )));
        }
        Some(ref s) if s != "active" => {
            return Err(AppError::Conflict(format!(
                "session {session_id} is not active (status: {s}), cannot push context"
            )));
        }
        _ => {}
    }

    // Build context from the push request
    let memory_inputs: Vec<crate::knowledge::context_delivery::ContextMemoryInput> = body
        .memories
        .iter()
        .map(|m| crate::knowledge::context_delivery::ContextMemoryInput {
            key: "manual".to_string(),
            content: m.clone(),
            category: zremote_protocol::knowledge::MemoryCategory::Convention,
            confidence: 1.0,
        })
        .collect();

    let context = crate::knowledge::context_delivery::ContextAssembler::assemble(
        "manual-push",
        "",
        "unknown",
        None,
        &[],
        &memory_inputs,
        &body.conventions,
        crate::knowledge::context_delivery::ContextTrigger::ManualPush,
    );

    // Store via DeliveryCoordinator (write to session via session_manager)
    let content = context.render();
    if !content.is_empty() {
        let content_bytes = content.into_bytes();
        let mut mgr = state.session_manager.lock().await;
        if let Err(e) = mgr.write_to(&parsed_session_id, &content_bytes) {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "failed to write context push to PTY"
            );
            return Err(AppError::Internal(format!(
                "failed to deliver context: {e}"
            )));
        }
    }

    Ok(StatusCode::ACCEPTED)
}

/// Query parameters for listing execution nodes.
#[derive(Debug, Deserialize)]
pub struct ListExecutionNodesQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    pub loop_id: Option<String>,
}

fn default_limit() -> i64 {
    50
}

/// `GET /api/sessions/:session_id/execution-nodes` - list execution nodes for a session.
pub async fn list_execution_nodes(
    State(state): State<Arc<LocalAppState>>,
    Path(session_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<ListExecutionNodesQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = query.limit.clamp(1, 200);
    let offset = query.offset.max(0);

    let nodes = if let Some(ref loop_id) = query.loop_id {
        zremote_core::queries::execution_nodes::list_execution_nodes_by_loop(
            &state.db, loop_id, limit, offset,
        )
        .await?
    } else {
        zremote_core::queries::execution_nodes::list_execution_nodes(
            &state.db,
            &session_id,
            limit,
            offset,
        )
        .await?
    };

    Ok(Json(nodes))
}

/// Query parameters for cleanup endpoint.
#[derive(Debug, Deserialize)]
pub struct CleanupQuery {
    #[serde(default = "default_max_age_days")]
    pub max_age_days: i64,
}

fn default_max_age_days() -> i64 {
    30
}

/// `DELETE /api/execution-nodes/cleanup` - delete old execution nodes.
pub async fn cleanup_execution_nodes(
    State(state): State<Arc<LocalAppState>>,
    axum::extract::Query(query): axum::extract::Query<CleanupQuery>,
) -> Result<impl IntoResponse, AppError> {
    let max_age_days = query.max_age_days.max(1);
    let deleted =
        zremote_core::queries::execution_nodes::delete_old_execution_nodes(&state.db, max_age_days)
            .await?;

    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

/// `GET /api/sessions/previews` - batch fetch screen snapshots for all active sessions.
pub async fn get_session_previews(
    State(state): State<Arc<LocalAppState>>,
) -> Result<impl IntoResponse, AppError> {
    let sessions = state.sessions.read().await;
    let mut previews = serde_json::Map::new();
    for (id, session_state) in &*sessions {
        let snapshot = session_state.screen_snapshot();
        let value = serde_json::to_value(&snapshot)
            .map_err(|e| AppError::Internal(format!("snapshot serialization: {e}")))?;
        previews.insert(id.to_string(), value);
    }
    Ok(Json(serde_json::json!({ "previews": previews })))
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
        LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        )
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
            .route(
                "/api/sessions/{session_id}/context/push",
                post(push_context),
            )
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

    #[tokio::test]
    async fn push_context_invalid_session_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sessions/not-a-uuid/context/push")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"memories": [], "conventions": []}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn push_context_session_not_found() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session_id}/context/push"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"memories": ["test"], "conventions": []}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn push_context_closed_session_returns_conflict() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4().to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'closed')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session_id}/context/push"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"memories": ["test"], "conventions": []}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // Execution node route tests
    // -----------------------------------------------------------------------

    fn build_execution_node_router(state: Arc<LocalAppState>) -> Router {
        Router::new()
            .route(
                "/api/sessions/{session_id}/execution-nodes",
                get(list_execution_nodes),
            )
            .route(
                "/api/execution-nodes/cleanup",
                delete(cleanup_execution_nodes),
            )
            .with_state(state)
    }

    /// Helper to insert a session row directly for execution node tests.
    async fn insert_test_session(state: &LocalAppState, session_id: &str) {
        let host_id = state.host_id.to_string();
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn api_list_execution_nodes_empty() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id).await;

        let app = build_execution_node_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{session_id}/execution-nodes"))
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
    async fn api_list_execution_nodes_with_data() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id).await;

        for i in 0..3 {
            zremote_core::queries::execution_nodes::insert_execution_node(
                &state.db,
                &session_id,
                None,
                1000 + i,
                "tool_call",
                Some(&format!("Read file{i}.rs")),
                Some("output"),
                None,
                "/home",
                50,
            )
            .await
            .unwrap();
        }

        let app = build_execution_node_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{session_id}/execution-nodes"))
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
        assert_eq!(json[0]["kind"], "tool_call");
    }

    #[tokio::test]
    async fn api_list_execution_nodes_by_loop() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id).await;

        zremote_core::queries::execution_nodes::insert_execution_node(
            &state.db,
            &session_id,
            Some("loop-a"),
            1000,
            "tool_call",
            None,
            None,
            None,
            "/home",
            50,
        )
        .await
        .unwrap();
        zremote_core::queries::execution_nodes::insert_execution_node(
            &state.db,
            &session_id,
            Some("loop-b"),
            1001,
            "tool_call",
            None,
            None,
            None,
            "/home",
            50,
        )
        .await
        .unwrap();
        zremote_core::queries::execution_nodes::insert_execution_node(
            &state.db,
            &session_id,
            Some("loop-a"),
            1002,
            "tool_call",
            None,
            None,
            None,
            "/home",
            50,
        )
        .await
        .unwrap();

        let app = build_execution_node_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/sessions/{session_id}/execution-nodes?loop_id=loop-a"
                    ))
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
        assert!(json.iter().all(|n| n["loop_id"].as_str() == Some("loop-a")));
    }

    #[tokio::test]
    async fn api_cleanup_execution_nodes() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id).await;

        let now_ms = chrono::Utc::now().timestamp_millis();
        let old_ms = now_ms - 31 * 24 * 60 * 60 * 1000; // 31 days ago

        zremote_core::queries::execution_nodes::insert_execution_node(
            &state.db,
            &session_id,
            None,
            old_ms,
            "tool_call",
            None,
            None,
            None,
            "/home",
            50,
        )
        .await
        .unwrap();
        zremote_core::queries::execution_nodes::insert_execution_node(
            &state.db,
            &session_id,
            None,
            now_ms,
            "tool_call",
            None,
            None,
            None,
            "/home",
            50,
        )
        .await
        .unwrap();

        let app = build_execution_node_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/execution-nodes/cleanup?max_age_days=30")
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
        assert_eq!(json["deleted"], 1);

        // Verify only the recent node remains
        let remaining = zremote_core::queries::execution_nodes::list_execution_nodes(
            &state.db,
            &session_id,
            10,
            0,
        )
        .await
        .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].timestamp, now_ms);
    }

    // -----------------------------------------------------------------------
    // Lifecycle integration test
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execution_node_full_lifecycle() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id).await;

        // Insert nodes
        for i in 0..5 {
            zremote_core::queries::execution_nodes::insert_execution_node(
                &state.db,
                &session_id,
                Some("loop-1"),
                1000 + i,
                "tool_call",
                Some(&format!("Read file{i}.rs")),
                Some("output"),
                None,
                "/home",
                100,
            )
            .await
            .unwrap();
        }

        // List and verify count
        let nodes = zremote_core::queries::execution_nodes::list_execution_nodes(
            &state.db,
            &session_id,
            100,
            0,
        )
        .await
        .unwrap();
        assert_eq!(nodes.len(), 5);

        let count =
            zremote_core::queries::execution_nodes::count_execution_nodes(&state.db, &session_id)
                .await
                .unwrap();
        assert_eq!(count, 5);

        // List by loop
        let by_loop = zremote_core::queries::execution_nodes::list_execution_nodes_by_loop(
            &state.db, "loop-1", 100, 0,
        )
        .await
        .unwrap();
        assert_eq!(by_loop.len(), 5);

        // Enforce cap of 3 -- should remove 2 oldest
        let deleted = zremote_core::queries::execution_nodes::enforce_session_node_cap(
            &state.db,
            &session_id,
            3,
        )
        .await
        .unwrap();
        assert_eq!(deleted, 2);

        let remaining = zremote_core::queries::execution_nodes::list_execution_nodes(
            &state.db,
            &session_id,
            100,
            0,
        )
        .await
        .unwrap();
        assert_eq!(remaining.len(), 3);
        assert_eq!(remaining[0].timestamp, 1002);
    }

    #[tokio::test]
    async fn api_list_execution_nodes_returns_all_without_filter() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id).await;

        for i in 0..10 {
            zremote_core::queries::execution_nodes::insert_execution_node(
                &state.db,
                &session_id,
                None,
                1000 + i,
                "tool_call",
                None,
                None,
                None,
                "/home",
                50,
            )
            .await
            .unwrap();
        }

        let app = build_execution_node_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/sessions/{session_id}/execution-nodes?limit=3&offset=2"
                    ))
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
        assert_eq!(json[0]["timestamp"], 1002);
    }

    #[tokio::test]
    async fn concurrent_node_insertion() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id).await;

        let mut handles = Vec::new();
        for i in 0..20 {
            let pool = state.db.clone();
            let sid = session_id.clone();
            handles.push(tokio::spawn(async move {
                zremote_core::queries::execution_nodes::insert_execution_node(
                    &pool,
                    &sid,
                    None,
                    1000 + i64::from(i),
                    "tool_call",
                    Some(&format!("Task {i}")),
                    None,
                    None,
                    "/home",
                    10,
                )
                .await
                .unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let count =
            zremote_core::queries::execution_nodes::count_execution_nodes(&state.db, &session_id)
                .await
                .unwrap();
        assert_eq!(count, 20);
    }

    #[tokio::test]
    async fn node_cap_enforcement_under_load() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        insert_test_session(&state, &session_id).await;

        for i in 0..50 {
            zremote_core::queries::execution_nodes::insert_execution_node(
                &state.db,
                &session_id,
                None,
                1000 + i,
                "tool_call",
                None,
                None,
                None,
                "/home",
                10,
            )
            .await
            .unwrap();
        }

        let deleted = zremote_core::queries::execution_nodes::enforce_session_node_cap(
            &state.db,
            &session_id,
            10,
        )
        .await
        .unwrap();
        assert_eq!(deleted, 40);

        let remaining = zremote_core::queries::execution_nodes::list_execution_nodes(
            &state.db,
            &session_id,
            100,
            0,
        )
        .await
        .unwrap();
        assert_eq!(remaining.len(), 10);
        assert_eq!(remaining[0].timestamp, 1040);
    }

    #[test]
    fn rapid_phase_transitions() {
        use crate::agentic::analyzer::{AnalyzerPhase, NodeBuilder};

        let mut nb = NodeBuilder::new("/tmp".to_string());

        for i in 0..100 {
            nb.on_tool_call("Read", &format!("file{i}.rs"), "/home");
            nb.on_output_line(&format!("content of file{i}"));
            nb.on_phase_changed(AnalyzerPhase::Busy, "/home");
            nb.on_phase_changed(AnalyzerPhase::Idle, "/home");
            nb.on_phase_changed(AnalyzerPhase::NeedsInput, "/home");
            nb.on_phase_changed(AnalyzerPhase::Busy, "/home");
        }

        nb.on_phase_changed(AnalyzerPhase::Idle, "/home");

        let nodes = nb.drain_completed();
        assert!(
            !nodes.is_empty(),
            "rapid phase transitions should produce completed nodes without panics"
        );
        for node in &nodes {
            assert!(!node.kind.is_empty());
            assert!(!node.working_dir.is_empty());
            assert!(node.duration_ms >= 0);
        }
    }
}
