use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_core::queries::claude_sessions as q;
use myremote_core::queries::sessions as sq;
use myremote_protocol::ServerMessage;
use myremote_protocol::claude::ClaudeServerMessage;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::{AppError, AppJson};
use crate::state::{AppState, ServerEvent, SessionState};

pub type ClaudeTaskResponse = q::ClaudeTaskRow;

#[derive(Debug, Deserialize)]
pub struct CreateClaudeTaskRequest {
    pub host_id: String,
    pub project_path: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub skip_permissions: Option<bool>,
    pub output_format: Option<String>,
    pub custom_flags: Option<String>,
}

/// `POST /api/claude-tasks` - Create and start a Claude task.
#[allow(clippy::too_many_lines)]
pub async fn create_claude_task(
    State(state): State<Arc<AppState>>,
    AppJson(body): AppJson<CreateClaudeTaskRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_host_id: Uuid = body
        .host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {}", body.host_id)))?;

    if !sq::host_exists(&state.db, &body.host_id).await? {
        return Err(AppError::NotFound(format!(
            "host {} not found",
            body.host_id
        )));
    }

    let sender = state
        .connections
        .get_sender(&parsed_host_id)
        .await
        .ok_or_else(|| {
            AppError::Conflict("host is offline, cannot start Claude task".to_string())
        })?;

    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();
    let claude_task_id = Uuid::new_v4();
    let claude_task_id_str = claude_task_id.to_string();

    let project_id = if body.project_id.is_some() {
        body.project_id.clone()
    } else {
        q::resolve_project_id_by_path(&state.db, &body.host_id, &body.project_path).await?
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

    q::insert_session_for_task(
        &state.db,
        &session_id_str,
        &body.host_id,
        &body.project_path,
        project_id.as_deref(),
    )
    .await?;

    q::insert_claude_task(
        &state.db,
        &claude_task_id_str,
        &session_id_str,
        &body.host_id,
        &body.project_path,
        project_id.as_deref(),
        body.model.as_deref(),
        body.initial_prompt.as_deref(),
        options_json.as_deref(),
    )
    .await?;

    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, parsed_host_id));
    }

    let msg = ServerMessage::ClaudeAction(ClaudeServerMessage::StartSession {
        session_id,
        claude_task_id,
        working_dir: body.project_path.clone(),
        model: body.model.clone(),
        initial_prompt: body.initial_prompt.clone(),
        resume_cc_session_id: None,
        allowed_tools: body.allowed_tools.unwrap_or_default(),
        skip_permissions: body.skip_permissions.unwrap_or(false),
        output_format: body.output_format,
        custom_flags: body.custom_flags,
        continue_last: false,
    });

    if sender.send(msg).await.is_err() {
        return Err(AppError::Conflict(
            "host went offline, cannot start Claude task".to_string(),
        ));
    }

    let _ = state.events.send(ServerEvent::ClaudeTaskStarted {
        task_id: claude_task_id_str.clone(),
        session_id: session_id_str.clone(),
        host_id: body.host_id.clone(),
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
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListClaudeTasksQuery>,
) -> Result<impl IntoResponse, AppError> {
    let filter = q::ListClaudeTasksFilter {
        host_id: query.host_id,
        status: query.status,
        project_id: query.project_id,
    };
    let tasks = q::list_claude_tasks(&state.db, &filter).await?;
    Ok(Json(tasks))
}

/// `GET /api/claude-tasks/:task_id` - Get a single Claude task.
pub async fn get_claude_task(
    State(state): State<Arc<AppState>>,
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
    State(state): State<Arc<AppState>>,
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

    let parsed_host_id: Uuid = original
        .host_id
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in original task".to_string()))?;

    let sender = state
        .connections
        .get_sender(&parsed_host_id)
        .await
        .ok_or_else(|| {
            AppError::Conflict("host is offline, cannot resume Claude task".to_string())
        })?;

    let new_session_id = Uuid::new_v4();
    let new_session_id_str = new_session_id.to_string();
    let new_task_id = Uuid::new_v4();
    let new_task_id_str = new_task_id.to_string();

    let initial_prompt = body.and_then(|Json(b)| b.initial_prompt);

    q::insert_session_for_task(
        &state.db,
        &new_session_id_str,
        &original.host_id,
        &original.project_path,
        original.project_id.as_deref(),
    )
    .await?;

    q::insert_resumed_claude_task(
        &state.db,
        &new_task_id_str,
        &new_session_id_str,
        &original.host_id,
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
            SessionState::new(new_session_id, parsed_host_id),
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

    let msg = ServerMessage::ClaudeAction(ClaudeServerMessage::StartSession {
        session_id: new_session_id,
        claude_task_id: new_task_id,
        working_dir: original.project_path.clone(),
        model: original.model.clone(),
        initial_prompt,
        resume_cc_session_id: cc_session_id.clone(),
        allowed_tools,
        skip_permissions,
        output_format,
        custom_flags,
        continue_last,
    });

    if sender.send(msg).await.is_err() {
        return Err(AppError::Conflict(
            "host went offline, cannot resume Claude task".to_string(),
        ));
    }

    let _ = state.events.send(ServerEvent::ClaudeTaskStarted {
        task_id: new_task_id_str.clone(),
        session_id: new_session_id_str.clone(),
        host_id: original.host_id.clone(),
        project_path: original.project_path.clone(),
    });

    let task = q::get_claude_task(&state.db, &new_task_id_str).await?;
    Ok((StatusCode::CREATED, Json(task)))
}

/// Timeout for discover sessions response from agent.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(10);

/// `GET /api/hosts/:host_id/claude-tasks/discover?project_path=...` - Discover existing CC sessions.
pub async fn discover_claude_sessions(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
    Query(params): Query<DiscoverQuery>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_host_id: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    let sender = state
        .connections
        .get_sender(&parsed_host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    let request_key = format!("{host_id}:{}", params.project_path);
    state
        .claude_discover_requests
        .insert(request_key.clone(), tx);

    let msg = ServerMessage::ClaudeAction(ClaudeServerMessage::DiscoverSessions {
        project_path: params.project_path,
    });

    if sender.send(msg).await.is_err() {
        state.claude_discover_requests.remove(&request_key);
        return Err(AppError::Conflict("host went offline".to_string()));
    }

    match tokio::time::timeout(DISCOVER_TIMEOUT, rx).await {
        Ok(Ok(sessions)) => Ok(Json(sessions)),
        Ok(Err(_)) => Err(AppError::Internal("discover request cancelled".to_string())),
        Err(_) => {
            state.claude_discover_requests.remove(&request_key);
            Err(AppError::Internal("discover request timed out".to_string()))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct DiscoverQuery {
    pub project_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use http_body_util::BodyExt;
    use myremote_protocol::ServerMessage;
    use tower::ServiceExt;

    async fn test_state() -> Arc<AppState> {
        let pool = myremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let connections = Arc::new(crate::state::ConnectionManager::new());
        let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let agentic_loops = std::sync::Arc::new(dashmap::DashMap::new());
        let (events_tx, _) = tokio::sync::broadcast::channel(1024);
        Arc::new(AppState {
            db: pool,
            connections,
            sessions,
            agentic_loops,
            agent_token_hash: crate::auth::hash_token("test-token"),
            shutdown: tokio_util::sync::CancellationToken::new(),
            events: events_tx,
            knowledge_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        })
    }

    fn build_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/api/claude-tasks",
                get(list_claude_tasks).post(create_claude_task),
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

    async fn insert_host(state: &AppState, host_id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES (?, 'test', 'test-host', 'hash', 'online')",
        )
        .bind(host_id)
        .execute(&state.db)
        .await
        .unwrap();
    }

    async fn register_host_connection(
        state: &AppState,
        host_id: Uuid,
    ) -> tokio::sync::mpsc::Receiver<ServerMessage> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "test-host".to_string(), tx, false)
            .await;
        rx
    }

    // --- list_claude_tasks tests ---

    #[tokio::test]
    async fn list_claude_tasks_empty() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/claude-tasks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn list_claude_tasks_with_filters() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/claude-tasks?status=completed&host_id=abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    // --- get_claude_task tests ---

    #[tokio::test]
    async fn get_claude_task_not_found() {
        let state = test_state().await;
        let task_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/claude-tasks/{task_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- create_claude_task tests ---

    #[tokio::test]
    async fn create_claude_task_invalid_host_id() {
        let state = test_state().await;
        let body = serde_json::json!({
            "host_id": "not-a-uuid",
            "project_path": "/proj",
        });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_claude_task_host_not_found() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let body = serde_json::json!({
            "host_id": host_id,
            "project_path": "/proj",
        });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_claude_task_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let body = serde_json::json!({
            "host_id": host_id,
            "project_path": "/proj",
        });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_claude_task_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        insert_host(&state, &host_id.to_string()).await;
        let mut _rx = register_host_connection(&state, host_id).await;

        let body = serde_json::json!({
            "host_id": host_id.to_string(),
            "project_path": "/home/user/project",
            "model": "sonnet",
            "initial_prompt": "Fix the bug",
        });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(json["host_id"], host_id.to_string());
        assert_eq!(json["project_path"], "/home/user/project");
        assert_eq!(json["model"], "sonnet");
        assert_eq!(json["initial_prompt"], "Fix the bug");
        assert_eq!(json["status"], "starting");
    }

    #[tokio::test]
    async fn create_claude_task_with_options() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        insert_host(&state, &host_id.to_string()).await;
        let mut _rx = register_host_connection(&state, host_id).await;

        let body = serde_json::json!({
            "host_id": host_id.to_string(),
            "project_path": "/proj",
            "allowed_tools": ["Bash", "Read"],
            "skip_permissions": true,
            "output_format": "json",
            "custom_flags": "--verbose",
        });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert!(json["options_json"].is_string());
    }

    // --- list after create ---

    #[tokio::test]
    async fn list_claude_tasks_after_create() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        insert_host(&state, &host_id.to_string()).await;
        let mut _rx = register_host_connection(&state, host_id).await;

        let body = serde_json::json!({
            "host_id": host_id.to_string(),
            "project_path": "/proj",
        });
        let app = build_router(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let app2 = build_router(Arc::clone(&state));
        let resp2 = app2
            .oneshot(
                Request::get("/api/claude-tasks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
        let body2 = resp2.into_body().collect().await.unwrap().to_bytes();
        let tasks: Vec<serde_json::Value> = serde_json::from_slice(&body2).unwrap();
        assert_eq!(tasks.len(), 1);
    }

    // --- resume_claude_task tests ---

    #[tokio::test]
    async fn resume_claude_task_not_found() {
        let state = test_state().await;
        let task_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn resume_claude_task_still_active() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        insert_host(&state, &host_id.to_string()).await;
        let mut _rx = register_host_connection(&state, host_id).await;

        // Create a task
        let body = serde_json::json!({
            "host_id": host_id.to_string(),
            "project_path": "/proj",
        });
        let app = build_router(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let task: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        let task_id = task["id"].as_str().unwrap();

        // Try to resume while status is "starting"
        let app2 = build_router(Arc::clone(&state));
        let resp2 = app2
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn resume_claude_task_completed_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        insert_host(&state, &host_id.to_string()).await;
        let mut _rx = register_host_connection(&state, host_id).await;

        // Create a task
        let body = serde_json::json!({
            "host_id": host_id.to_string(),
            "project_path": "/proj",
            "model": "opus",
        });
        let app = build_router(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/claude-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let create_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let task: serde_json::Value = serde_json::from_slice(&create_bytes).unwrap();
        let task_id = task["id"].as_str().unwrap();

        // Mark it completed in DB
        sqlx::query("UPDATE claude_sessions SET status = 'completed' WHERE id = ?")
            .bind(task_id)
            .execute(&state.db)
            .await
            .unwrap();

        // Resume it
        let resume_body = serde_json::json!({ "initial_prompt": "Continue please" });
        let app2 = build_router(Arc::clone(&state));
        let resume_resp = app2
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/claude-tasks/{task_id}/resume"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&resume_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resume_resp.status(), StatusCode::CREATED);
        let resume_bytes = resume_resp.into_body().collect().await.unwrap().to_bytes();
        let new_task: serde_json::Value = serde_json::from_slice(&resume_bytes).unwrap();
        assert_eq!(new_task["status"], "starting");
        assert_eq!(new_task["resume_from"], task_id);
        assert_eq!(new_task["model"], "opus");
    }

    // --- discover_claude_sessions tests ---

    #[tokio::test]
    async fn discover_invalid_host_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/hosts/not-uuid/claude-tasks/discover?project_path=/proj")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn discover_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!(
                    "/api/hosts/{host_id}/claude-tasks/discover?project_path=/proj"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }
}
