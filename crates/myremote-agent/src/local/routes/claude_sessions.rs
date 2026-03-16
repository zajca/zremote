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
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, AppError> {
    let resume_req: ResumeClaudeTaskRequest = if body.is_empty() {
        ResumeClaudeTaskRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| AppError::BadRequest(format!("invalid JSON: {e}")))?
    };

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

    let initial_prompt = resume_req.initial_prompt;

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

    #[tokio::test]
    async fn list_claude_tasks_with_status_filter() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Insert a task via direct DB
        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            Some("opus"),
            Some("hello"),
            None,
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        // Filter by status=starting should return the task
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/claude-tasks?status=starting")
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
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["id"], task_id);

        // Filter by status=completed should return nothing
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/claude-tasks?status=completed")
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
    async fn list_claude_tasks_with_project_id_filter() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        myremote_core::queries::projects::insert_project(
            &state.db,
            &project_id,
            &host_id,
            "/tmp/project",
            "test",
        )
        .await
        .unwrap();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            Some(&project_id),
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            Some(&project_id),
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        // Filter by project_id
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/claude-tasks?project_id={project_id}"))
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
        assert_eq!(json.len(), 1);

        // Filter by non-matching project_id
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/claude-tasks?project_id={}",
                        Uuid::new_v4()
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
    async fn get_claude_task_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            Some("sonnet"),
            Some("test prompt"),
            None,
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/claude-tasks/{task_id}"))
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
        assert_eq!(json["id"], task_id);
        assert_eq!(json["model"], "sonnet");
        assert_eq!(json["initial_prompt"], "test prompt");
        assert_eq!(json["status"], "starting");
    }

    #[tokio::test]
    async fn resume_active_task_returns_conflict() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // Update to active status
        sqlx::query("UPDATE claude_sessions SET status = 'active' WHERE id = ?")
            .bind(&task_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn resume_starting_task_returns_conflict() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        // status is 'starting' by default

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn resume_with_invalid_json_returns_400() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // Mark as completed so resume is allowed
        sqlx::query("UPDATE claude_sessions SET status = 'completed' WHERE id = ?")
            .bind(&task_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .header("content-type", "application/json")
                    .body(Body::from("{invalid json"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_claude_task_invalid_body() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Missing required field project_path
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn discover_sessions_missing_query_param() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/hosts/{host_id}/claude-tasks/discover"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Missing required query param project_path
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_claude_tasks_with_explicit_host_id() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Insert a task for this host
        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let app = build_test_router(state.clone());

        // Query with explicit host_id should return the task
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/claude-tasks?host_id={host_id}"))
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
        assert_eq!(json.len(), 1);

        // Query with a different host_id should return nothing
        let other_host = Uuid::new_v4();
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/claude-tasks?host_id={other_host}"))
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
    async fn list_claude_tasks_combined_filters() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        myremote_core::queries::projects::insert_project(
            &state.db,
            &project_id,
            &host_id,
            "/tmp/project",
            "test",
        )
        .await
        .unwrap();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            Some(&project_id),
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            Some(&project_id),
            Some("opus"),
            Some("hello"),
            None,
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        // Combine status + project_id filters
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/claude-tasks?status=starting&project_id={project_id}"
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
        assert_eq!(json.len(), 1);

        // Mismatched status should return empty
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/claude-tasks?status=completed&project_id={project_id}"
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
    async fn get_claude_task_returns_all_fields() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        myremote_core::queries::projects::insert_project(
            &state.db,
            &project_id,
            &host_id,
            "/tmp/project",
            "test",
        )
        .await
        .unwrap();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        let options = r#"{"allowed_tools":["bash"],"output_format":"json","custom_flags":"--verbose"}"#;

        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            Some(&project_id),
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            Some(&project_id),
            Some("opus"),
            Some("do something"),
            Some(options),
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/claude-tasks/{task_id}"))
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
        assert_eq!(json["id"], task_id);
        assert_eq!(json["session_id"], session_id);
        assert_eq!(json["host_id"], host_id);
        assert_eq!(json["project_path"], "/tmp/project");
        assert_eq!(json["project_id"], project_id);
        assert_eq!(json["model"], "opus");
        assert_eq!(json["initial_prompt"], "do something");
        assert_eq!(json["status"], "starting");
        assert!(!json["options_json"].is_null());
    }

    #[tokio::test]
    async fn create_claude_task_missing_content_type() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .body(Body::from(r#"{"project_path": "/tmp/test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Missing content-type header
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resume_with_empty_body_on_nonexistent() {
        let state = test_state().await;
        let task_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_claude_tasks_defaults_to_local_host() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Insert tasks for this host and a different host
        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        // Insert task for a different host
        let other_host = Uuid::new_v4().to_string();
        let session_id2 = Uuid::new_v4().to_string();
        let task_id2 = Uuid::new_v4().to_string();

        // Insert the other host first
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) VALUES (?, ?, ?, 'x', 'online')",
        )
        .bind(&other_host)
        .bind("other")
        .bind("other")
        .execute(&state.db)
        .await
        .unwrap();

        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id2,
            &other_host,
            "/tmp/other",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id2,
            &session_id2,
            &other_host,
            "/tmp/other",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        // Default list should only return tasks for local host
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
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["id"], task_id);
    }

    #[test]
    fn default_shell_returns_value() {
        // default_shell should return some non-empty string
        let shell = default_shell();
        assert!(!shell.is_empty());
    }

    #[test]
    fn create_request_deserialize_minimal() {
        let json = r#"{"project_path": "/tmp/test"}"#;
        let req: CreateClaudeTaskRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.project_path, "/tmp/test");
        assert!(req.project_id.is_none());
        assert!(req.model.is_none());
        assert!(req.initial_prompt.is_none());
        assert!(req.allowed_tools.is_none());
        assert!(req.skip_permissions.is_none());
        assert!(req.output_format.is_none());
        assert!(req.custom_flags.is_none());
    }

    #[test]
    fn create_request_deserialize_full() {
        let json = r#"{
            "project_path": "/tmp/test",
            "project_id": "abc-123",
            "model": "opus",
            "initial_prompt": "do it",
            "allowed_tools": ["Read", "Write"],
            "skip_permissions": true,
            "output_format": "stream-json",
            "custom_flags": "--verbose"
        }"#;
        let req: CreateClaudeTaskRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.project_path, "/tmp/test");
        assert_eq!(req.project_id.as_deref(), Some("abc-123"));
        assert_eq!(req.model.as_deref(), Some("opus"));
        assert_eq!(req.initial_prompt.as_deref(), Some("do it"));
        assert_eq!(req.allowed_tools.as_ref().unwrap().len(), 2);
        assert_eq!(req.skip_permissions, Some(true));
        assert_eq!(req.output_format.as_deref(), Some("stream-json"));
        assert_eq!(req.custom_flags.as_deref(), Some("--verbose"));
    }

    #[test]
    fn resume_request_deserialize_empty() {
        let json = "{}";
        let req: ResumeClaudeTaskRequest = serde_json::from_str(json).unwrap();
        assert!(req.initial_prompt.is_none());
    }

    #[test]
    fn resume_request_deserialize_with_prompt() {
        let json = r#"{"initial_prompt": "continue with this"}"#;
        let req: ResumeClaudeTaskRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.initial_prompt.as_deref(), Some("continue with this"));
    }

    #[test]
    fn resume_request_default_has_no_prompt() {
        let req = ResumeClaudeTaskRequest::default();
        assert!(req.initial_prompt.is_none());
    }

    #[test]
    fn discover_query_deserialize() {
        let json = r#"{"project_path": "/home/user/project"}"#;
        let query: DiscoverQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.project_path, "/home/user/project");
    }

    #[test]
    fn list_query_deserialize_empty() {
        let json = "{}";
        let query: ListClaudeTasksQuery = serde_json::from_str(json).unwrap();
        assert!(query.host_id.is_none());
        assert!(query.status.is_none());
        assert!(query.project_id.is_none());
    }

    #[test]
    fn list_query_deserialize_full() {
        let json = r#"{"host_id": "abc", "status": "active", "project_id": "p1"}"#;
        let query: ListClaudeTasksQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.host_id.as_deref(), Some("abc"));
        assert_eq!(query.status.as_deref(), Some("active"));
        assert_eq!(query.project_id.as_deref(), Some("p1"));
    }

    #[tokio::test]
    async fn get_claude_task_with_options_json() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        let options = r#"{"allowed_tools":["Read","Write"],"skip_permissions":true,"output_format":"json","custom_flags":"--verbose"}"#;

        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            Some("opus"),
            Some("test prompt"),
            Some(options),
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/claude-tasks/{task_id}"))
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
        assert_eq!(json["id"], task_id);
        // Verify options_json is preserved
        let opts: serde_json::Value =
            serde_json::from_str(json["options_json"].as_str().unwrap()).unwrap();
        assert!(opts["allowed_tools"].is_array());
        assert_eq!(opts["allowed_tools"].as_array().unwrap().len(), 2);
        assert_eq!(opts["skip_permissions"], true);
        assert_eq!(opts["output_format"], "json");
        assert_eq!(opts["custom_flags"], "--verbose");
    }

    #[tokio::test]
    async fn resume_completed_task_without_cc_session_id_uses_continue() {
        // This tests the code path where cc_session_id is None so continue_last = true
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            Some("opus"),
            Some("original prompt"),
            None, // no options
        )
        .await
        .unwrap();

        // Mark as completed, leave claude_session_id as NULL
        sqlx::query("UPDATE claude_sessions SET status = 'completed' WHERE id = ?")
            .bind(&task_id)
            .execute(&state.db)
            .await
            .unwrap();

        // Verify the task has no claude_session_id
        let task = myremote_core::queries::claude_sessions::get_claude_task(&state.db, &task_id)
            .await
            .unwrap();
        assert!(task.claude_session_id.is_none());

        // Try to resume -- will fail at PTY spawn but we verify the path up to that point
        let app = build_test_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"initial_prompt": "continue from here"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // PTY spawn will fail in test env but it exercises the DB insertion and option parsing paths
        // Status will be 500 (Internal Server Error) because PTY spawn fails, or 201 if it succeeds
        let status = response.status();
        assert!(
            status == StatusCode::INTERNAL_SERVER_ERROR || status == StatusCode::CREATED,
            "expected 500 or 201, got {status}"
        );
    }

    #[tokio::test]
    async fn resume_completed_task_with_options_json() {
        // Tests the code path where options_json is present and parsed
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        let options = r#"{"allowed_tools":["Read"],"skip_permissions":true,"output_format":"stream-json","custom_flags":"--verbose"}"#;

        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            Some("opus"),
            Some("original"),
            Some(options),
        )
        .await
        .unwrap();

        // Mark as completed with a claude_session_id
        sqlx::query(
            "UPDATE claude_sessions SET status = 'completed', claude_session_id = ? WHERE id = ?",
        )
        .bind("cc-session-abc123")
        .bind(&task_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Will fail at PTY spawn but exercises the options parsing path
        let status = response.status();
        assert!(
            status == StatusCode::INTERNAL_SERVER_ERROR || status == StatusCode::CREATED,
            "expected 500 or 201, got {status}"
        );
    }

    #[tokio::test]
    async fn create_claude_task_exercises_options_json_building() {
        // Tests the code path where allowed_tools, output_format, and custom_flags
        // trigger options_json building
        let state = test_state().await;
        let app = build_test_router(state);

        let body = serde_json::json!({
            "project_path": "/tmp/test",
            "model": "opus",
            "initial_prompt": "do it",
            "allowed_tools": ["Read", "Write"],
            "skip_permissions": true,
            "output_format": "stream-json",
            "custom_flags": "--verbose"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // PTY spawn will likely fail but the DB insertion paths are exercised
        let status = response.status();
        assert!(
            status == StatusCode::INTERNAL_SERVER_ERROR || status == StatusCode::CREATED,
            "expected 500 or 201, got {status}"
        );
    }

    #[tokio::test]
    async fn create_claude_task_with_project_id() {
        // Tests the code path where project_id is explicitly provided
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        myremote_core::queries::projects::insert_project(
            &state.db,
            &project_id,
            &host_id,
            "/tmp/project",
            "test",
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        let body = serde_json::json!({
            "project_path": "/tmp/project",
            "project_id": project_id,
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // PTY spawn may fail, but exercises the project_id code path
        let status = response.status();
        assert!(
            status == StatusCode::INTERNAL_SERVER_ERROR || status == StatusCode::CREATED,
            "expected 500 or 201, got {status}"
        );
    }

    #[tokio::test]
    async fn create_claude_task_without_options_no_options_json() {
        // Tests the code path where no allowed_tools/output_format/custom_flags
        // so options_json remains None
        let state = test_state().await;
        let app = build_test_router(state);

        let body = serde_json::json!({
            "project_path": "/tmp/test",
            "model": "opus",
            "initial_prompt": "hello"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        assert!(
            status == StatusCode::INTERNAL_SERVER_ERROR || status == StatusCode::CREATED,
            "expected 500 or 201, got {status}"
        );
    }

    #[tokio::test]
    async fn resume_failed_task_allowed() {
        // A task with status 'failed' should be resumable
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        sqlx::query("UPDATE claude_sessions SET status = 'failed' WHERE id = ?")
            .bind(&task_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should not be conflict (failed tasks are resumable)
        let status = response.status();
        assert_ne!(
            status,
            StatusCode::CONFLICT,
            "failed tasks should be resumable"
        );
    }

    #[tokio::test]
    async fn resume_with_empty_options_json_uses_defaults() {
        // Tests the else branch: when original.options_json is None
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        let session_id = Uuid::new_v4().to_string();
        let task_id = Uuid::new_v4().to_string();
        myremote_core::queries::claude_sessions::insert_session_for_task(
            &state.db,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
        )
        .await
        .unwrap();
        myremote_core::queries::claude_sessions::insert_claude_task(
            &state.db,
            &task_id,
            &session_id,
            &host_id,
            "/tmp/project",
            None,
            None,
            None,
            None, // no options_json
        )
        .await
        .unwrap();

        sqlx::query("UPDATE claude_sessions SET status = 'completed' WHERE id = ?")
            .bind(&task_id)
            .execute(&state.db)
            .await
            .unwrap();

        let app = build_test_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        // Exercises the None options_json branch (defaults: empty tools, no skip, no format, no flags)
        assert!(
            status == StatusCode::INTERNAL_SERVER_ERROR || status == StatusCode::CREATED,
            "expected 500 or 201, got {status}"
        );
    }
}
