use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use myremote_core::queries::loops as q;
use myremote_protocol::agentic::{AgenticServerMessage, AgenticStatus, UserAction};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppJson};
use crate::state::AppState;

/// Query parameters for listing agentic loops.
#[derive(Debug, Deserialize)]
pub struct ListLoopsQuery {
    pub status: Option<String>,
    pub host_id: Option<String>,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
}

// Re-export core row types as response types.
pub type LoopResponse = q::LoopRow;
pub type ToolCallResponse = q::ToolCallRow;
pub type TranscriptEntryResponse = q::TranscriptEntryRow;

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
pub async fn list_loops(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListLoopsQuery>,
) -> Result<Json<Vec<LoopResponse>>, AppError> {
    let filter = q::ListLoopsFilter {
        status: query.status,
        host_id: query.host_id,
        session_id: query.session_id,
        project_id: query.project_id,
    };
    let loops = q::list_loops(&state.db, &filter).await?;
    Ok(Json(loops))
}

/// `GET /api/loops/:id` - get loop detail.
pub async fn get_loop(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<LoopResponse>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let row = q::get_loop(&state.db, &loop_id).await?;
    Ok(Json(row))
}

/// `GET /api/loops/:id/tools` - tool calls for a loop.
pub async fn get_loop_tools(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<Vec<ToolCallResponse>>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let rows = q::get_loop_tools(&state.db, &loop_id).await?;
    Ok(Json(rows))
}

/// `GET /api/loops/:id/transcript` - transcript entries for a loop.
pub async fn get_loop_transcript(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<Vec<TranscriptEntryResponse>>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let rows = q::get_loop_transcript(&state.db, &loop_id).await?;
    Ok(Json(rows))
}

/// `POST /api/loops/:id/action` - send user action to agentic loop.
pub async fn post_loop_action(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
    AppJson(body): AppJson<ActionRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let parsed_loop_id = parse_loop_id(&loop_id)?;

    let session_id_str = q::get_loop_session_id(&state.db, &loop_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("loop {loop_id} not found")))?;

    let host_id_str = q::get_session_host_id(&state.db, &session_id_str)
        .await?
        .ok_or_else(|| AppError::Internal("session has no host".to_string()))?;

    let parsed_host_id: uuid::Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    // Check loop status
    if let Some(entry) = state.agentic_loops.get(&parsed_loop_id) {
        match entry.status {
            AgenticStatus::Working | AgenticStatus::WaitingForInput | AgenticStatus::Paused => {}
            _ => {
                return Err(AppError::Conflict(
                    "Loop is not in an actionable state".to_string(),
                ));
            }
        }
    }

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
    let row = q::get_loop(&state.db, &loop_id).await?;

    Ok(Json(MetricsResponse {
        loop_id,
        status: row.status,
        total_tokens_in: row.total_tokens_in.unwrap_or(0),
        total_tokens_out: row.total_tokens_out.unwrap_or(0),
        estimated_cost_usd: row.estimated_cost_usd.unwrap_or(0.0),
        pending_tool_calls: 0,
    }))
}
