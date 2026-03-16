use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_core::error::{AppError, AppJson};
use myremote_core::queries::claude_sessions as q;
use myremote_core::queries::sessions as sq;
use myremote_core::state::{ServerEvent, SessionState};
use serde::Deserialize;
use uuid::Uuid;

use crate::claude::{CommandBuilder, CommandOptions, SessionScanner};
use crate::local::state::LocalAppState;

pub type ClaudeTaskResponse = q::ClaudeTaskRow;

#[derive(Debug, Deserialize)]
pub struct CreateClaudeTaskRequest {
    pub project_path: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub skip_permissions: Option<bool>,
    pub output_format: Option<String>,
    pub custom_flags: Option<String>,
}

/// Resolve the default shell (same logic as sessions.rs).
fn default_shell() -> &'static str {
    static SHELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SHELL.get_or_init(|| {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    })
}

/// `POST /api/claude-tasks` - Create and start a Claude task.
#[allow(clippy::too_many_lines)]
pub async fn create_claude_task(
    State(state): State<Arc<LocalAppState>>,
    AppJson(body): AppJson<CreateClaudeTaskRequest>,
) -> Result<impl IntoResponse, AppError> {
    let host_id = state.host_id.to_string();

    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();
    let claude_task_id = Uuid::new_v4();
    let claude_task_id_str = claude_task_id.to_string();

    let project_id = if body.project_id.is_some() {
        body.project_id.clone()
    } else {
        q::resolve_project_id_by_path(&state.db, &host_id, &body.project_path).await?
    };

    let options_json = if body.allowed_tools.is_some()
        || body.output_format.is_some()
        || body.custom_flags.is_some()
    {
        let opts = serde_json::json!({
            "allowed_tools": body.allowed_tools,
            "output_format": body.output_format,
            "custom_flags": body.custom_flags,
        });
        Some(opts.to_string())
    } else {
        None
    };

    // Insert DB rows
    q::insert_session_for_task(
        &state.db,
        &session_id_str,
        &host_id,
        &body.project_path,
        project_id.as_deref(),
    )
    .await?;

    q::insert_claude_task(
        &state.db,
        &claude_task_id_str,
        &session_id_str,
        &host_id,
        &body.project_path,
        project_id.as_deref(),
        body.model.as_deref(),
        body.initial_prompt.as_deref(),
        options_json.as_deref(),
    )
    .await?;

    // Create in-memory session state
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, state.host_id));
    }

    // Build claude command
    let allowed_tools = body.allowed_tools.unwrap_or_default();
    let opts = CommandOptions {
        working_dir: &body.project_path,
        model: body.model.as_deref(),
        initial_prompt: body.initial_prompt.as_deref(),
        resume_cc_session_id: None,
        continue_last: false,
        allowed_tools: &allowed_tools,
        skip_permissions: body.skip_permissions.unwrap_or(false),
        output_format: body.output_format.as_deref(),
        custom_flags: body.custom_flags.as_deref(),
    };

    let cmd = CommandBuilder::build(&opts)
        .map_err(|e| AppError::BadRequest(format!("invalid command options: {e}")))?;

    // Spawn PTY session
    let shell = default_shell();
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(session_id, shell, 120, 40, Some(&body.project_path))
            .map_err(|e| AppError::Internal(format!("failed to spawn PTY: {e}")))?
    };

    // Update session status in DB
    sqlx::query("UPDATE sessions SET status = 'active', shell = ?, pid = ? WHERE id = ?")
        .bind(shell)
        .bind(i64::from(pid))
        .bind(&session_id_str)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    // Write the claude command into the PTY
    {
        let mut mgr = state.session_manager.lock().await;
        mgr.write_to(&session_id, cmd.as_bytes())
            .map_err(|e| AppError::Internal(format!("failed to write command to PTY: {e}")))?;
    }

    // Broadcast event
    let _ = state.events.send(ServerEvent::ClaudeTaskStarted {
        task_id: claude_task_id_str.clone(),
        session_id: session_id_str.clone(),
        host_id: host_id.clone(),
        project_path: body.project_path.clone(),
    });

    let task = q::get_claude_task(&state.db, &claude_task_id_str).await?;
    Ok((StatusCode::CREATED, Json(task)))
}

#[derive(Debug, Deserialize)]
pub struct ListClaudeTasksQuery {
    pub host_id: Option<String>,
    pub status: Option<String>,
    pub project_id: Option<String>,
}

/// `GET /api/claude-tasks` - List Claude tasks with optional filters.
pub async fn list_claude_tasks(
    State(state): State<Arc<LocalAppState>>,
    Query(query): Query<ListClaudeTasksQuery>,
) -> Result<impl IntoResponse, AppError> {
    let filter = q::ListClaudeTasksFilter {
        host_id: query.host_id.or_else(|| Some(state.host_id.to_string())),
        status: query.status,
        project_id: query.project_id,
    };
    let tasks = q::list_claude_tasks(&state.db, &filter).await?;
    Ok(Json(tasks))
}

/// `GET /api/claude-tasks/:task_id` - Get a single Claude task.
pub async fn get_claude_task(
    State(state): State<Arc<LocalAppState>>,
    Path(task_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let task = q::get_claude_task(&state.db, &task_id).await?;
    Ok(Json(task))
}

#[derive(Debug, Default, Deserialize)]
pub struct ResumeClaudeTaskRequest {
    pub initial_prompt: Option<String>,
}

/// `POST /api/claude-tasks/:task_id/resume` - Resume a completed Claude task.
#[allow(clippy::too_many_lines)]
pub async fn resume_claude_task(
    State(state): State<Arc<LocalAppState>>,
    Path(task_id): Path<String>,
    body: Option<Json<ResumeClaudeTaskRequest>>,
) -> Result<impl IntoResponse, AppError> {
    let original = q::get_claude_task(&state.db, &task_id).await?;

    if original.status == "starting" || original.status == "active" {
        return Err(AppError::Conflict(format!(
            "cannot resume task with status '{}', task is still running",
            original.status
        )));
    }

    let cc_session_id = original.claude_session_id;
    let continue_last = cc_session_id.is_none();

    let host_id = state.host_id.to_string();

    let new_session_id = Uuid::new_v4();
    let new_session_id_str = new_session_id.to_string();
    let new_task_id = Uuid::new_v4();
    let new_task_id_str = new_task_id.to_string();

    let initial_prompt = body.and_then(|Json(b)| b.initial_prompt);

    q::insert_session_for_task(
        &state.db,
        &new_session_id_str,
        &host_id,
        &original.project_path,
        original.project_id.as_deref(),
    )
    .await?;

    q::insert_resumed_claude_task(
        &state.db,
        &new_task_id_str,
        &new_session_id_str,
        &host_id,
        &original.project_path,
        original.project_id.as_deref(),
        original.model.as_deref(),
        initial_prompt.as_deref(),
        cc_session_id.as_deref(),
        &original.id,
        original.options_json.as_deref(),
    )
    .await?;

    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(
            new_session_id,
            SessionState::new(new_session_id, state.host_id),
        );
    }

    let (allowed_tools, skip_permissions, output_format, custom_flags) =
        if let Some(ref opts_str) = original.options_json {
            let opts: serde_json::Value = serde_json::from_str(opts_str).unwrap_or_default();
            let tools = opts["allowed_tools"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let skip = opts["skip_permissions"].as_bool().unwrap_or(false);
            let fmt = opts["output_format"].as_str().map(String::from);
            let flags = opts["custom_flags"].as_str().map(String::from);
            (tools, skip, fmt, flags)
        } else {
            (vec![], false, None, None)
        };

    let cmd_opts = CommandOptions {
        working_dir: &original.project_path,
        model: original.model.as_deref(),
        initial_prompt: initial_prompt.as_deref(),
        resume_cc_session_id: cc_session_id.as_deref(),
        continue_last,
        allowed_tools: &allowed_tools,
        skip_permissions,
        output_format: output_format.as_deref(),
        custom_flags: custom_flags.as_deref(),
    };

    let cmd = CommandBuilder::build(&cmd_opts)
        .map_err(|e| AppError::BadRequest(format!("invalid command options: {e}")))?;

    // Spawn PTY session
    let shell = default_shell();
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(
            new_session_id,
            shell,
            120,
            40,
            Some(&original.project_path),
        )
        .map_err(|e| AppError::Internal(format!("failed to spawn PTY: {e}")))?
    };

    // Update session status in DB
    sqlx::query("UPDATE sessions SET status = 'active', shell = ?, pid = ? WHERE id = ?")
        .bind(shell)
        .bind(i64::from(pid))
        .bind(&new_session_id_str)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    // Write the claude command into the PTY
    {
        let mut mgr = state.session_manager.lock().await;
        mgr.write_to(&new_session_id, cmd.as_bytes())
            .map_err(|e| AppError::Internal(format!("failed to write command to PTY: {e}")))?;
    }

    let _ = state.events.send(ServerEvent::ClaudeTaskStarted {
        task_id: new_task_id_str.clone(),
        session_id: new_session_id_str.clone(),
        host_id: host_id.clone(),
        project_path: original.project_path.clone(),
    });

    let task = q::get_claude_task(&state.db, &new_task_id_str).await?;
    Ok((StatusCode::CREATED, Json(task)))
}

#[derive(Debug, Deserialize)]
pub struct DiscoverQuery {
    pub project_path: String,
}

/// `GET /api/hosts/:host_id/claude-tasks/discover?project_path=...` - Discover CC sessions directly.
pub async fn discover_claude_sessions(
    State(_state): State<Arc<LocalAppState>>,
    Path(host_id): Path<String>,
    Query(params): Query<DiscoverQuery>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    let project_path = params.project_path.clone();
    let sessions = tokio::task::spawn_blocking(move || SessionScanner::discover(&project_path))
        .await
        .map_err(|e| AppError::Internal(format!("discover task failed: {e}")))?;

    Ok(Json(sessions))
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
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
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
                "/api/claude-tasks",
                post(create_claude_task).get(list_claude_tasks),
            )
            .route("/api/claude-tasks/{task_id}", get(get_claude_task))
            .route(
                "/api/claude-tasks/{task_id}/resume",
                post(resume_claude_task),
            )
            .route(
                "/api/hosts/{host_id}/claude-tasks/discover",
                get(discover_claude_sessions),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn list_claude_tasks_empty() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/claude-tasks")
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
    async fn get_claude_task_not_found() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/claude-tasks/nonexistent-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn discover_sessions() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/hosts/{host_id}/claude-tasks/discover?project_path=/nonexistent/path"
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
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn discover_sessions_invalid_host() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(
                        "/api/hosts/not-a-uuid/claude-tasks/discover?project_path=/tmp",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resume_nonexistent_task() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks/nonexistent-id/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
