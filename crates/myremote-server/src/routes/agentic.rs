use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use myremote_protocol::agentic::{AgenticServerMessage, UserAction};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppJson};
use crate::state::AppState;

/// Query parameters for listing agentic loops.
#[derive(Debug, Deserialize)]
pub struct ListLoopsQuery {
    pub status: Option<String>,
    pub host_id: Option<String>,
    pub session_id: Option<String>,
}

/// Agentic loop response for API.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct LoopResponse {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub model: Option<String>,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub total_tokens_in: Option<i64>,
    pub total_tokens_out: Option<i64>,
    pub estimated_cost_usd: Option<f64>,
    pub end_reason: Option<String>,
    pub summary: Option<String>,
}

/// Tool call response for API.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct ToolCallResponse {
    pub id: String,
    pub loop_id: String,
    pub tool_name: String,
    pub arguments_json: Option<String>,
    pub status: String,
    pub result_preview: Option<String>,
    pub duration_ms: Option<i64>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// Transcript entry response for API.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct TranscriptEntryResponse {
    pub id: i64,
    pub loop_id: String,
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub timestamp: String,
}

/// Metrics response for API.
#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    pub loop_id: String,
    pub status: String,
    pub total_tokens_in: i64,
    pub total_tokens_out: i64,
    pub estimated_cost_usd: f64,
    pub pending_tool_calls: usize,
}

/// Request body for user action on a loop.
#[derive(Debug, Deserialize)]
pub struct ActionRequest {
    pub action: UserAction,
    pub payload: Option<String>,
}

fn parse_loop_id(loop_id: &str) -> Result<uuid::Uuid, AppError> {
    loop_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid loop ID: {loop_id}")))
}

/// `GET /api/loops` - list agentic loops with optional filters.
#[allow(clippy::too_many_lines)]
pub async fn list_loops(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListLoopsQuery>,
) -> Result<Json<Vec<LoopResponse>>, AppError> {
    let mut sql = String::from(
        "SELECT id, session_id, project_path, tool_name, model, status, started_at, \
         ended_at, total_tokens_in, total_tokens_out, estimated_cost_usd, end_reason, summary \
         FROM agentic_loops WHERE 1=1",
    );
    let mut binds: Vec<String> = Vec::new();

    if let Some(ref status) = query.status {
        sql.push_str(" AND status = ?");
        binds.push(status.clone());
    }
    if let Some(ref session_id) = query.session_id {
        sql.push_str(" AND session_id = ?");
        binds.push(session_id.clone());
    }
    if let Some(ref host_id) = query.host_id {
        sql.push_str(
            " AND session_id IN (SELECT id FROM sessions WHERE host_id = ?)",
        );
        binds.push(host_id.clone());
    }

    sql.push_str(" ORDER BY started_at DESC");

    let mut q = sqlx::query_as::<_, LoopResponse>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }

    let loops = q.fetch_all(&state.db).await?;
    Ok(Json(loops))
}

/// `GET /api/loops/:id` - get loop detail.
pub async fn get_loop(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<LoopResponse>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;

    let row: LoopResponse = sqlx::query_as(
        "SELECT id, session_id, project_path, tool_name, model, status, started_at, \
         ended_at, total_tokens_in, total_tokens_out, estimated_cost_usd, end_reason, summary \
         FROM agentic_loops WHERE id = ?",
    )
    .bind(&loop_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("loop {loop_id} not found")))?;

    Ok(Json(row))
}

/// `GET /api/loops/:id/tools` - tool calls for a loop.
pub async fn get_loop_tools(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<Vec<ToolCallResponse>>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;

    let rows: Vec<ToolCallResponse> = sqlx::query_as(
        "SELECT id, loop_id, tool_name, arguments_json, status, result_preview, \
         duration_ms, created_at, resolved_at \
         FROM tool_calls WHERE loop_id = ? ORDER BY created_at ASC",
    )
    .bind(&loop_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(rows))
}

/// `GET /api/loops/:id/transcript` - transcript entries for a loop.
pub async fn get_loop_transcript(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<Vec<TranscriptEntryResponse>>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;

    let rows: Vec<TranscriptEntryResponse> = sqlx::query_as(
        "SELECT id, loop_id, role, content, tool_call_id, timestamp \
         FROM transcript_entries WHERE loop_id = ? ORDER BY id ASC",
    )
    .bind(&loop_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(rows))
}

/// `POST /api/loops/:id/action` - send user action to agentic loop.
pub async fn post_loop_action(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
    AppJson(body): AppJson<ActionRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let parsed_loop_id = parse_loop_id(&loop_id)?;

    // Find which session (and thus host) this loop belongs to
    let session_id: Option<(String,)> = sqlx::query_as(
        "SELECT session_id FROM agentic_loops WHERE id = ?",
    )
    .bind(&loop_id)
    .fetch_optional(&state.db)
    .await?;

    let (session_id_str,) = session_id
        .ok_or_else(|| AppError::NotFound(format!("loop {loop_id} not found")))?;

    // Find host_id from the session
    let host_id: Option<(String,)> = sqlx::query_as(
        "SELECT host_id FROM sessions WHERE id = ?",
    )
    .bind(&session_id_str)
    .fetch_optional(&state.db)
    .await?;

    let (host_id_str,) = host_id
        .ok_or_else(|| AppError::Internal("session has no host".to_string()))?;

    let parsed_host_id: uuid::Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    // Send AgenticServerMessage::UserAction to the agent via ConnectionManager
    let sender = state
        .connections
        .get_sender(&parsed_host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let msg = myremote_protocol::ServerMessage::AgenticAction(AgenticServerMessage::UserAction {
        loop_id: parsed_loop_id,
        action: body.action,
        payload: body.payload,
    });

    sender
        .send(msg)
        .await
        .map_err(|_| AppError::Conflict("failed to send action to agent".to_string()))?;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

/// `GET /api/loops/:id/metrics` - current metrics for a loop.
pub async fn get_loop_metrics(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<MetricsResponse>, AppError> {
    let parsed_loop_id = parse_loop_id(&loop_id)?;

    // Try in-memory state first for active loops
    if let Some(entry) = state.agentic_loops.get(&parsed_loop_id) {
        return Ok(Json(MetricsResponse {
            loop_id: loop_id.clone(),
            status: format!("{:?}", entry.status).to_lowercase(),
            total_tokens_in: i64::try_from(entry.tokens_in).unwrap_or(i64::MAX),
            total_tokens_out: i64::try_from(entry.tokens_out).unwrap_or(i64::MAX),
            estimated_cost_usd: entry.estimated_cost_usd,
            pending_tool_calls: entry.pending_tool_calls.len(),
        }));
    }

    // Fall back to DB for ended loops
    let row: LoopResponse = sqlx::query_as(
        "SELECT id, session_id, project_path, tool_name, model, status, started_at, \
         ended_at, total_tokens_in, total_tokens_out, estimated_cost_usd, end_reason, summary \
         FROM agentic_loops WHERE id = ?",
    )
    .bind(&loop_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("loop {loop_id} not found")))?;

    Ok(Json(MetricsResponse {
        loop_id,
        status: row.status,
        total_tokens_in: row.total_tokens_in.unwrap_or(0),
        total_tokens_out: row.total_tokens_out.unwrap_or(0),
        estimated_cost_usd: row.estimated_cost_usd.unwrap_or(0.0),
        pending_tool_calls: 0,
    }))
}
