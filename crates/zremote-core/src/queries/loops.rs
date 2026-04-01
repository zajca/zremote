use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use zremote_protocol::AgenticStatus;

use crate::error::AppError;
use crate::state::LoopInfo;

/// Agentic loop response for API.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct LoopRow {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
    pub task_name: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: Option<f64>,
}

/// Parse a status string from DB into `AgenticStatus`.
pub fn parse_status(s: &str) -> AgenticStatus {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .unwrap_or(AgenticStatus::Unknown)
}

/// Convert a `LoopRow` (DB data) to a `LoopInfo`.
pub fn enrich_loop(row: LoopRow) -> LoopInfo {
    LoopInfo {
        id: row.id,
        session_id: row.session_id,
        project_path: row.project_path,
        tool_name: row.tool_name,
        status: parse_status(&row.status),
        started_at: row.started_at,
        ended_at: row.ended_at,
        end_reason: row.end_reason,
        task_name: row.task_name,
        input_tokens: row.input_tokens.cast_unsigned(),
        output_tokens: row.output_tokens.cast_unsigned(),
        cost_usd: row.cost_usd,
    }
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
        "SELECT id, session_id, project_path, tool_name, status, started_at, \
         ended_at, end_reason, task_name, input_tokens, output_tokens, cost_usd \
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
        "SELECT id, session_id, project_path, tool_name, status, started_at, \
         ended_at, end_reason, task_name, input_tokens, output_tokens, cost_usd \
         FROM agentic_loops WHERE id = ?",
    )
    .bind(loop_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("loop {loop_id} not found")))?;
    Ok(row)
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
    }

    #[tokio::test]
    async fn get_loop_not_found() {
        let pool = setup_db().await;
        let result = get_loop(&pool, "nonexistent").await;
        assert!(result.is_err());
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

    #[test]
    fn parse_status_known_variants() {
        assert_eq!(super::parse_status("working"), AgenticStatus::Working);
        assert_eq!(
            super::parse_status("waiting_for_input"),
            AgenticStatus::WaitingForInput
        );
        assert_eq!(super::parse_status("error"), AgenticStatus::Error);
        assert_eq!(super::parse_status("completed"), AgenticStatus::Completed);
    }

    #[test]
    fn parse_status_unknown_falls_back() {
        assert_eq!(super::parse_status("active"), AgenticStatus::Unknown);
        assert_eq!(super::parse_status("paused"), AgenticStatus::Unknown);
        assert_eq!(super::parse_status(""), AgenticStatus::Unknown);
        assert_eq!(
            super::parse_status("some_future_status"),
            AgenticStatus::Unknown
        );
    }
}
