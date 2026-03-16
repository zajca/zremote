use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;

/// Agentic loop response for API.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct LoopRow {
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
pub struct ToolCallRow {
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
pub struct TranscriptEntryRow {
    pub id: i64,
    pub loop_id: String,
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub timestamp: String,
}

/// Query parameters for listing agentic loops.
pub struct ListLoopsFilter {
    pub status: Option<String>,
    pub host_id: Option<String>,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
}

pub async fn list_loops(
    pool: &SqlitePool,
    filter: &ListLoopsFilter,
) -> Result<Vec<LoopRow>, AppError> {
    let mut sql = String::from(
        "SELECT id, session_id, project_path, tool_name, model, status, started_at, \
         ended_at, total_tokens_in, total_tokens_out, estimated_cost_usd, end_reason, summary \
         FROM agentic_loops WHERE 1=1",
    );
    let mut binds: Vec<String> = Vec::new();

    if let Some(ref status) = filter.status {
        sql.push_str(" AND status = ?");
        binds.push(status.clone());
    }
    if let Some(ref session_id) = filter.session_id {
        sql.push_str(" AND session_id = ?");
        binds.push(session_id.clone());
    }
    if let Some(ref host_id) = filter.host_id {
        sql.push_str(
            " AND session_id IN (SELECT id FROM sessions WHERE host_id = ?)",
        );
        binds.push(host_id.clone());
    }
    if let Some(ref project_id) = filter.project_id {
        sql.push_str(
            " AND session_id IN (SELECT id FROM sessions WHERE project_id = ?)",
        );
        binds.push(project_id.clone());
    }

    sql.push_str(" ORDER BY started_at DESC");

    let mut q = sqlx::query_as::<_, LoopRow>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }

    let loops = q.fetch_all(pool).await?;
    Ok(loops)
}

pub async fn get_loop(pool: &SqlitePool, loop_id: &str) -> Result<LoopRow, AppError> {
    let row: LoopRow = sqlx::query_as(
        "SELECT id, session_id, project_path, tool_name, model, status, started_at, \
         ended_at, total_tokens_in, total_tokens_out, estimated_cost_usd, end_reason, summary \
         FROM agentic_loops WHERE id = ?",
    )
    .bind(loop_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("loop {loop_id} not found")))?;
    Ok(row)
}

pub async fn get_loop_tools(
    pool: &SqlitePool,
    loop_id: &str,
) -> Result<Vec<ToolCallRow>, AppError> {
    let rows: Vec<ToolCallRow> = sqlx::query_as(
        "SELECT id, loop_id, tool_name, arguments_json, status, result_preview, \
         duration_ms, created_at, resolved_at \
         FROM tool_calls WHERE loop_id = ? ORDER BY created_at ASC",
    )
    .bind(loop_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_loop_transcript(
    pool: &SqlitePool,
    loop_id: &str,
) -> Result<Vec<TranscriptEntryRow>, AppError> {
    let rows: Vec<TranscriptEntryRow> = sqlx::query_as(
        "SELECT id, loop_id, role, content, tool_call_id, timestamp \
         FROM transcript_entries WHERE loop_id = ? ORDER BY id ASC",
    )
    .bind(loop_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_loop_session_id(
    pool: &SqlitePool,
    loop_id: &str,
) -> Result<Option<String>, AppError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT session_id FROM agentic_loops WHERE id = ?",
    )
    .bind(loop_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(s,)| s))
}

pub async fn get_session_host_id(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<String>, AppError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT host_id FROM sessions WHERE id = ?",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(s,)| s))
}
