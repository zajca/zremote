use std::fmt::Write;

use serde::Serialize;
use sqlx::SqlitePool;

use crate::error::AppError;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TokenBreakdown {
    pub label: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CostPoint {
    pub period: String,
    pub cost: f64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SessionStats {
    pub total_sessions: i64,
    pub active_sessions: i64,
    pub avg_duration_seconds: Option<f64>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct LoopStats {
    pub total_loops: i64,
    pub completed: i64,
    pub errored: i64,
    pub avg_cost_usd: Option<f64>,
    pub total_cost_usd: f64,
    pub total_tokens_in: i64,
    pub total_tokens_out: i64,
}

fn date_filter(sql: &mut String, binds: &mut Vec<String>, from: Option<&String>, to: Option<&String>, date_col: &str) {
    if let Some(f) = from {
        write!(sql, " AND {date_col} >= ?").unwrap();
        binds.push(f.clone());
    }
    if let Some(t) = to {
        write!(sql, " AND {date_col} <= ?").unwrap();
        binds.push(t.clone());
    }
}

pub async fn get_tokens(
    pool: &SqlitePool,
    by: &str,
    from: Option<&String>,
    to: Option<&String>,
) -> Result<Vec<TokenBreakdown>, AppError> {
    let group_expr = match by {
        "model" => "COALESCE(model, 'unknown')".to_string(),
        "host" => "COALESCE((SELECT h.hostname FROM sessions s JOIN hosts h ON h.id = s.host_id WHERE s.id = agentic_loops.session_id), 'unknown')".to_string(),
        "project" => "COALESCE(project_path, 'unknown')".to_string(),
        // default: day
        _ => "date(started_at)".to_string(),
    };

    let mut sql = format!(
        "SELECT {group_expr} as label, \
         COALESCE(SUM(total_tokens_in), 0) as tokens_in, \
         COALESCE(SUM(total_tokens_out), 0) as tokens_out \
         FROM agentic_loops WHERE 1=1"
    );
    let mut binds: Vec<String> = Vec::new();
    date_filter(&mut sql, &mut binds, from, to, "started_at");
    write!(sql, " GROUP BY {group_expr} ORDER BY label").unwrap();

    let mut q = sqlx::query_as::<_, TokenBreakdown>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }

    let rows = q.fetch_all(pool).await?;
    Ok(rows)
}

pub async fn get_cost(
    pool: &SqlitePool,
    granularity: &str,
    from: Option<&String>,
    to: Option<&String>,
) -> Result<Vec<CostPoint>, AppError> {
    let date_expr = match granularity {
        "week" => "strftime('%Y-W%W', started_at)",
        "month" => "strftime('%Y-%m', started_at)",
        // default: day
        _ => "date(started_at)",
    };

    let mut sql = format!(
        "SELECT {date_expr} as period, \
         COALESCE(SUM(estimated_cost_usd), 0.0) as cost \
         FROM agentic_loops WHERE 1=1"
    );
    let mut binds: Vec<String> = Vec::new();
    date_filter(&mut sql, &mut binds, from, to, "started_at");
    write!(sql, " GROUP BY {date_expr} ORDER BY period").unwrap();

    let mut q = sqlx::query_as::<_, CostPoint>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }

    let rows = q.fetch_all(pool).await?;
    Ok(rows)
}

pub async fn get_session_stats(
    pool: &SqlitePool,
    from: Option<&String>,
    to: Option<&String>,
) -> Result<SessionStats, AppError> {
    let mut sql = String::from(
        "SELECT \
         COUNT(*) as total_sessions, \
         SUM(CASE WHEN status = 'active' THEN 1 ELSE 0 END) as active_sessions, \
         AVG(CASE WHEN ss.duration_seconds > 0 THEN ss.duration_seconds ELSE NULL END) as avg_duration_seconds \
         FROM sessions LEFT JOIN session_stats ss ON ss.session_id = sessions.id \
         WHERE 1=1"
    );
    let mut binds: Vec<String> = Vec::new();
    date_filter(&mut sql, &mut binds, from, to, "sessions.created_at");

    let mut q = sqlx::query_as::<_, SessionStats>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }

    let stats = q.fetch_one(pool).await?;
    Ok(stats)
}

pub async fn get_loop_stats(
    pool: &SqlitePool,
    from: Option<&String>,
    to: Option<&String>,
) -> Result<LoopStats, AppError> {
    let mut sql = String::from(
        "SELECT \
         COUNT(*) as total_loops, \
         SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as completed, \
         SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as errored, \
         AVG(estimated_cost_usd) as avg_cost_usd, \
         COALESCE(SUM(estimated_cost_usd), 0.0) as total_cost_usd, \
         COALESCE(SUM(total_tokens_in), 0) as total_tokens_in, \
         COALESCE(SUM(total_tokens_out), 0) as total_tokens_out \
         FROM agentic_loops WHERE 1=1"
    );
    let mut binds: Vec<String> = Vec::new();
    date_filter(&mut sql, &mut binds, from, to, "started_at");

    let mut q = sqlx::query_as::<_, LoopStats>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }

    let stats = q.fetch_one(pool).await?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn setup_db() -> SqlitePool {
        let pool = db::init_db("sqlite::memory:").await.unwrap();

        // Insert a host and session for FK constraints
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('h1', 'test', 'test-host', 'hash', 'online')"
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO sessions (id, host_id, status) VALUES ('s1', 'h1', 'active')"
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    #[tokio::test]
    async fn tokens_empty_returns_empty_vec() {
        let pool = setup_db().await;
        let rows = get_tokens(&pool, "day", None, None).await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn loop_stats_empty() {
        let pool = setup_db().await;
        let stats = get_loop_stats(&pool, None, None).await.unwrap();
        assert_eq!(stats.total_loops, 0);
        assert_eq!(stats.total_cost_usd, 0.0);
    }

    #[tokio::test]
    async fn loop_stats_with_data() {
        let pool = setup_db().await;

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, total_tokens_in, total_tokens_out, estimated_cost_usd, started_at) \
             VALUES ('l1', 's1', 'claude', 'opus', 'completed', 1000, 500, 0.05, '2026-03-10T10:00:00Z')"
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, total_tokens_in, total_tokens_out, estimated_cost_usd, started_at) \
             VALUES ('l2', 's1', 'claude', 'sonnet', 'error', 2000, 1000, 0.10, '2026-03-10T11:00:00Z')"
        )
        .execute(&pool)
        .await
        .unwrap();

        let stats = get_loop_stats(&pool, None, None).await.unwrap();
        assert_eq!(stats.total_loops, 2);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.errored, 1);
        assert!((stats.total_cost_usd - 0.15).abs() < 0.001);
        assert_eq!(stats.total_tokens_in, 3000);
        assert_eq!(stats.total_tokens_out, 1500);
    }

    #[tokio::test]
    async fn session_stats_empty() {
        let pool = setup_db().await;
        let stats = get_session_stats(&pool, None, None).await.unwrap();
        assert_eq!(stats.total_sessions, 1); // We inserted one session in setup
        assert_eq!(stats.active_sessions, 1);
    }

    #[tokio::test]
    async fn fts_search_returns_matching_transcript() {
        let pool = setup_db().await;

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) \
             VALUES ('l1', 's1', 'claude', 'completed')"
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) \
             VALUES ('l1', 'assistant', 'Hello world this is a test function', '2026-03-10T10:00:00Z')"
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT rowid FROM transcript_fts WHERE content MATCH 'function'"
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(rows.len(), 1);
    }
}
