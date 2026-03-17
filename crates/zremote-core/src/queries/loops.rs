use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;
use crate::state::{AgenticLoopStore, LoopInfo};

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
    pub context_used: Option<i64>,
    pub context_max: Option<i64>,
    pub task_name: Option<String>,
}

/// Enrich a `LoopRow` (DB data) with in-memory state to produce a `LoopInfo`.
pub fn enrich_loop(row: LoopRow, agentic_loops: &AgenticLoopStore) -> LoopInfo {
    let loop_uuid: Option<uuid::Uuid> = row.id.parse().ok();
    let (pending_tool_calls, context_used, context_max) =
        loop_uuid.and_then(|id| agentic_loops.get(&id)).map_or(
            (
                0,
                row.context_used.unwrap_or(0),
                row.context_max.unwrap_or(0),
            ),
            |e| {
                (
                    i64::try_from(e.pending_tool_calls.len()).unwrap_or(0),
                    i64::try_from(e.context_used).unwrap_or(0),
                    i64::try_from(e.context_max).unwrap_or(0),
                )
            },
        );

    LoopInfo {
        id: row.id,
        session_id: row.session_id,
        project_path: row.project_path,
        tool_name: row.tool_name,
        model: row.model,
        status: row.status,
        started_at: row.started_at,
        ended_at: row.ended_at,
        total_tokens_in: row.total_tokens_in.unwrap_or(0),
        total_tokens_out: row.total_tokens_out.unwrap_or(0),
        estimated_cost_usd: row.estimated_cost_usd.unwrap_or(0.0),
        end_reason: row.end_reason,
        summary: row.summary,
        context_used,
        context_max,
        pending_tool_calls,
        task_name: row.task_name,
    }
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
         ended_at, total_tokens_in, total_tokens_out, estimated_cost_usd, end_reason, summary, \
         context_used, context_max, task_name \
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
        sql.push_str(" AND session_id IN (SELECT id FROM sessions WHERE host_id = ?)");
        binds.push(host_id.clone());
    }
    if let Some(ref project_id) = filter.project_id {
        sql.push_str(" AND session_id IN (SELECT id FROM sessions WHERE project_id = ?)");
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
         ended_at, total_tokens_in, total_tokens_out, estimated_cost_usd, end_reason, summary, \
         context_used, context_max, task_name \
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
    let row: Option<(String,)> =
        sqlx::query_as("SELECT session_id FROM agentic_loops WHERE id = ?")
            .bind(loop_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(s,)| s))
}

pub async fn get_session_host_id(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<String>, AppError> {
    let row: Option<(String,)> = sqlx::query_as("SELECT host_id FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(s,)| s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn setup_db() -> SqlitePool {
        let pool = db::init_db("sqlite::memory:").await.unwrap();

        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('h1', 'test', 'test-host', 'hash', 'online')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES ('s1', 'h1', 'active')")
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, started_at) \
             VALUES ('l1', 's1', 'claude', 'opus', 'completed', '2026-03-10T10:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    #[tokio::test]
    async fn list_loops_no_filter() {
        let pool = setup_db().await;
        let filter = ListLoopsFilter {
            status: None,
            host_id: None,
            session_id: None,
            project_id: None,
        };
        let loops = list_loops(&pool, &filter).await.unwrap();
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].id, "l1");
    }

    #[tokio::test]
    async fn list_loops_with_status_filter() {
        let pool = setup_db().await;
        let filter = ListLoopsFilter {
            status: Some("completed".to_string()),
            host_id: None,
            session_id: None,
            project_id: None,
        };
        let loops = list_loops(&pool, &filter).await.unwrap();
        assert_eq!(loops.len(), 1);

        let filter_miss = ListLoopsFilter {
            status: Some("active".to_string()),
            host_id: None,
            session_id: None,
            project_id: None,
        };
        let loops_miss = list_loops(&pool, &filter_miss).await.unwrap();
        assert!(loops_miss.is_empty());
    }

    #[tokio::test]
    async fn list_loops_with_session_filter() {
        let pool = setup_db().await;
        let filter = ListLoopsFilter {
            status: None,
            host_id: None,
            session_id: Some("s1".to_string()),
            project_id: None,
        };
        let loops = list_loops(&pool, &filter).await.unwrap();
        assert_eq!(loops.len(), 1);
    }

    #[tokio::test]
    async fn list_loops_with_host_filter() {
        let pool = setup_db().await;
        let filter = ListLoopsFilter {
            status: None,
            host_id: Some("h1".to_string()),
            session_id: None,
            project_id: None,
        };
        let loops = list_loops(&pool, &filter).await.unwrap();
        assert_eq!(loops.len(), 1);
    }

    #[tokio::test]
    async fn get_loop_found() {
        let pool = setup_db().await;
        let loop_row = get_loop(&pool, "l1").await.unwrap();
        assert_eq!(loop_row.id, "l1");
        assert_eq!(loop_row.tool_name, "claude");
        assert_eq!(loop_row.model, Some("opus".to_string()));
    }

    #[tokio::test]
    async fn get_loop_not_found() {
        let pool = setup_db().await;
        let result = get_loop(&pool, "nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_loop_tools_empty() {
        let pool = setup_db().await;
        let tools = get_loop_tools(&pool, "l1").await.unwrap();
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn get_loop_tools_with_data() {
        let pool = setup_db().await;

        sqlx::query(
            "INSERT INTO tool_calls (id, loop_id, tool_name, status) VALUES ('tc1', 'l1', 'Bash', 'completed')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let tools = get_loop_tools(&pool, "l1").await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_name, "Bash");
    }

    #[tokio::test]
    async fn get_loop_transcript_empty() {
        let pool = setup_db().await;
        let transcript = get_loop_transcript(&pool, "l1").await.unwrap();
        assert!(transcript.is_empty());
    }

    #[tokio::test]
    async fn get_loop_transcript_with_data() {
        let pool = setup_db().await;

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) \
             VALUES ('l1', 'assistant', 'Hello', '2026-03-10T10:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let transcript = get_loop_transcript(&pool, "l1").await.unwrap();
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript[0].role, "assistant");
        assert_eq!(transcript[0].content, "Hello");
    }

    #[tokio::test]
    async fn get_loop_session_id_found() {
        let pool = setup_db().await;
        let session_id = get_loop_session_id(&pool, "l1").await.unwrap();
        assert_eq!(session_id, Some("s1".to_string()));
    }

    #[tokio::test]
    async fn get_loop_session_id_not_found() {
        let pool = setup_db().await;
        let session_id = get_loop_session_id(&pool, "nonexistent").await.unwrap();
        assert!(session_id.is_none());
    }

    #[tokio::test]
    async fn get_session_host_id_found() {
        let pool = setup_db().await;
        let host_id = get_session_host_id(&pool, "s1").await.unwrap();
        assert_eq!(host_id, Some("h1".to_string()));
    }

    #[tokio::test]
    async fn get_session_host_id_not_found() {
        let pool = setup_db().await;
        let host_id = get_session_host_id(&pool, "nonexistent").await.unwrap();
        assert!(host_id.is_none());
    }
}
