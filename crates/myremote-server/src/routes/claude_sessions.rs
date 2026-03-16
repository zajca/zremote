use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_protocol::ServerMessage;
use myremote_protocol::claude::ClaudeServerMessage;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppJson};
use crate::state::{AppState, SessionState, ServerEvent};

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

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ClaudeTaskResponse {
    pub id: String,
    pub session_id: String,
    pub host_id: String,
    pub project_path: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub claude_session_id: Option<String>,
    pub resume_from: Option<String>,
    pub status: String,
    pub options_json: Option<String>,
    pub loop_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub total_tokens_in: Option<i64>,
    pub total_tokens_out: Option<i64>,
    pub summary: Option<String>,
    pub created_at: String,
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

    // Check host exists
    let host_exists: Option<(String,)> = sqlx::query_as("SELECT id FROM hosts WHERE id = ?")
        .bind(&body.host_id)
        .fetch_optional(&state.db)
        .await?;
    if host_exists.is_none() {
        return Err(AppError::NotFound(format!(
            "host {} not found",
            body.host_id
        )));
    }

    // Check agent is online
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

    // Resolve project_id if not provided
    let project_id = if body.project_id.is_some() {
        body.project_id.clone()
    } else {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM projects WHERE host_id = ? AND path = ? LIMIT 1",
        )
        .bind(&body.host_id)
        .bind(&body.project_path)
        .fetch_optional(&state.db)
        .await?
    };

    // Build options_json
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

    // 1. Insert terminal session (status: creating)
    sqlx::query(
        "INSERT INTO sessions (id, host_id, status, working_dir, project_id) VALUES (?, ?, 'creating', ?, ?)",
    )
    .bind(&session_id_str)
    .bind(&body.host_id)
    .bind(&body.project_path)
    .bind(&project_id)
    .execute(&state.db)
    .await?;

    // 2. Insert claude_sessions row
    sqlx::query(
        "INSERT INTO claude_sessions (id, session_id, host_id, project_path, project_id, model, initial_prompt, status, options_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'starting', ?)",
    )
    .bind(&claude_task_id_str)
    .bind(&session_id_str)
    .bind(&body.host_id)
    .bind(&body.project_path)
    .bind(&project_id)
    .bind(&body.model)
    .bind(&body.initial_prompt)
    .bind(&options_json)
    .execute(&state.db)
    .await?;

    // 3. Create in-memory session state
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, parsed_host_id));
    }

    // 4. Send ClaudeAction::StartSession to agent
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
    });

    if sender.send(msg).await.is_err() {
        return Err(AppError::Conflict(
            "host went offline, cannot start Claude task".to_string(),
        ));
    }

    // 5. Broadcast event
    let _ = state.events.send(ServerEvent::ClaudeTaskStarted {
        task_id: claude_task_id_str.clone(),
        session_id: session_id_str.clone(),
        host_id: body.host_id.clone(),
        project_path: body.project_path.clone(),
    });

    // 6. Fetch and return the created row
    let task: ClaudeTaskResponse = sqlx::query_as(
        "SELECT id, session_id, host_id, project_path, project_id, model, initial_prompt, \
         claude_session_id, resume_from, status, options_json, loop_id, started_at, ended_at, \
         total_cost_usd, total_tokens_in, total_tokens_out, summary, created_at \
         FROM claude_sessions WHERE id = ?",
    )
    .bind(&claude_task_id_str)
    .fetch_one(&state.db)
    .await?;

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
    // Build dynamic SQL query with optional filters
    let mut sql = String::from(
        "SELECT id, session_id, host_id, project_path, project_id, model, initial_prompt, \
         claude_session_id, resume_from, status, options_json, loop_id, started_at, ended_at, \
         total_cost_usd, total_tokens_in, total_tokens_out, summary, created_at \
         FROM claude_sessions WHERE 1=1",
    );
    let mut binds: Vec<String> = Vec::new();

    if let Some(ref host_id) = query.host_id {
        sql.push_str(" AND host_id = ?");
        binds.push(host_id.clone());
    }
    if let Some(ref status) = query.status {
        sql.push_str(" AND status = ?");
        binds.push(status.clone());
    }
    if let Some(ref project_id) = query.project_id {
        sql.push_str(" AND project_id = ?");
        binds.push(project_id.clone());
    }

    sql.push_str(" ORDER BY created_at DESC");

    // Use sqlx::query_as with dynamic binds
    let mut q = sqlx::query_as::<_, ClaudeTaskResponse>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }

    let tasks: Vec<ClaudeTaskResponse> = q.fetch_all(&state.db).await?;

    Ok(Json(tasks))
}

/// `GET /api/claude-tasks/:task_id` - Get a single Claude task.
pub async fn get_claude_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let task: ClaudeTaskResponse = sqlx::query_as(
        "SELECT id, session_id, host_id, project_path, project_id, model, initial_prompt, \
         claude_session_id, resume_from, status, options_json, loop_id, started_at, ended_at, \
         total_cost_usd, total_tokens_in, total_tokens_out, summary, created_at \
         FROM claude_sessions WHERE id = ?",
    )
    .bind(&task_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("claude task {task_id} not found")))?;

    Ok(Json(task))
}
