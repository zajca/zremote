use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ExecutionNodeRow {
    pub id: i64,
    pub session_id: String,
    pub loop_id: Option<String>,
    pub timestamp: i64,
    pub kind: String,
    pub input: Option<String>,
    pub output_summary: Option<String>,
    pub exit_code: Option<i32>,
    pub working_dir: String,
    pub duration_ms: i64,
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_execution_node(
    pool: &SqlitePool,
    session_id: &str,
    loop_id: Option<&str>,
    timestamp: i64,
    kind: &str,
    input: Option<&str>,
    output_summary: Option<&str>,
    exit_code: Option<i32>,
    working_dir: &str,
    duration_ms: i64,
) -> Result<i64, AppError> {
    let result = sqlx::query(
        "INSERT INTO execution_nodes \
         (session_id, loop_id, timestamp, kind, input, output_summary, exit_code, working_dir, duration_ms) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(loop_id)
    .bind(timestamp)
    .bind(kind)
    .bind(input)
    .bind(output_summary)
    .bind(exit_code)
    .bind(working_dir)
    .bind(duration_ms)
    .execute(pool)
    .await
    .map_err(AppError::Database)?;

    Ok(result.last_insert_rowid())
}

pub async fn list_execution_nodes(
    pool: &SqlitePool,
    session_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<ExecutionNodeRow>, AppError> {
    sqlx::query_as(
        "SELECT id, session_id, loop_id, timestamp, kind, input, output_summary, \
         exit_code, working_dir, duration_ms \
         FROM execution_nodes WHERE session_id = ? ORDER BY timestamp ASC LIMIT ? OFFSET ?",
    )
    .bind(session_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)
}

pub async fn list_execution_nodes_by_loop(
    pool: &SqlitePool,
    loop_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<ExecutionNodeRow>, AppError> {
    sqlx::query_as(
        "SELECT id, session_id, loop_id, timestamp, kind, input, output_summary, \
         exit_code, working_dir, duration_ms \
         FROM execution_nodes WHERE loop_id = ? ORDER BY timestamp ASC LIMIT ? OFFSET ?",
    )
    .bind(loop_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)
}

pub async fn count_execution_nodes(pool: &SqlitePool, session_id: &str) -> Result<i64, AppError> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM execution_nodes WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(pool)
            .await
            .map_err(AppError::Database)?;
    Ok(count)
}

pub async fn delete_old_execution_nodes(
    pool: &SqlitePool,
    max_age_days: i64,
) -> Result<u64, AppError> {
    let cutoff_ms = chrono::Utc::now().timestamp_millis() - max_age_days * 24 * 60 * 60 * 1000;
    let result = sqlx::query("DELETE FROM execution_nodes WHERE timestamp < ?")
        .bind(cutoff_ms)
        .execute(pool)
        .await
        .map_err(AppError::Database)?;
    Ok(result.rows_affected())
}

pub async fn enforce_session_node_cap(
    pool: &SqlitePool,
    session_id: &str,
    max_nodes: i64,
) -> Result<u64, AppError> {
    let result = sqlx::query(
        "DELETE FROM execution_nodes WHERE id IN (\
         SELECT id FROM execution_nodes WHERE session_id = ? \
         ORDER BY timestamp ASC \
         LIMIT MAX(0, (SELECT COUNT(*) FROM execution_nodes WHERE session_id = ?) - ?))",
    )
    .bind(session_id)
    .bind(session_id)
    .bind(max_nodes)
    .execute(pool)
    .await
    .map_err(AppError::Database)?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_db() -> SqlitePool {
        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
        // Create a host and session for FK constraints
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('host1', 'test', 'test', 'hash', 'online')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, host_id, status) VALUES ('sess1', 'host1', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn insert_and_list_nodes() {
        let pool = setup_db().await;
        for i in 0..3 {
            insert_execution_node(
                &pool,
                "sess1",
                None,
                1000 + i,
                "tool_call",
                Some(&format!("Read file{i}.rs")),
                Some("output"),
                None,
                "/home/user",
                100,
            )
            .await
            .unwrap();
        }

        let nodes = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].timestamp, 1000);
        assert_eq!(nodes[2].timestamp, 1002);
    }

    #[tokio::test]
    async fn list_nodes_pagination() {
        let pool = setup_db().await;
        for i in 0..5 {
            insert_execution_node(
                &pool,
                "sess1",
                None,
                1000 + i,
                "tool_call",
                None,
                None,
                None,
                "/home",
                50,
            )
            .await
            .unwrap();
        }

        let nodes = list_execution_nodes(&pool, "sess1", 2, 2).await.unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].timestamp, 1002);
        assert_eq!(nodes[1].timestamp, 1003);
    }

    #[tokio::test]
    async fn list_nodes_by_loop() {
        let pool = setup_db().await;
        insert_execution_node(
            &pool,
            "sess1",
            Some("loop-a"),
            1000,
            "tool_call",
            None,
            None,
            None,
            "/home",
            50,
        )
        .await
        .unwrap();
        insert_execution_node(
            &pool,
            "sess1",
            Some("loop-b"),
            1001,
            "tool_call",
            None,
            None,
            None,
            "/home",
            50,
        )
        .await
        .unwrap();
        insert_execution_node(
            &pool,
            "sess1",
            Some("loop-a"),
            1002,
            "tool_call",
            None,
            None,
            None,
            "/home",
            50,
        )
        .await
        .unwrap();

        let nodes = list_execution_nodes_by_loop(&pool, "loop-a", 10, 0)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|n| n.loop_id.as_deref() == Some("loop-a")));
    }

    #[tokio::test]
    async fn count_nodes() {
        let pool = setup_db().await;
        for i in 0..7 {
            insert_execution_node(
                &pool,
                "sess1",
                None,
                1000 + i,
                "shell_command",
                None,
                None,
                None,
                "/home",
                10,
            )
            .await
            .unwrap();
        }
        let count = count_execution_nodes(&pool, "sess1").await.unwrap();
        assert_eq!(count, 7);
    }

    #[tokio::test]
    async fn insert_node_without_loop_id() {
        let pool = setup_db().await;
        let id = insert_execution_node(
            &pool,
            "sess1",
            None,
            1000,
            "shell_command",
            None,
            None,
            None,
            "/home",
            10,
        )
        .await
        .unwrap();
        assert!(id > 0);

        let nodes = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].loop_id.is_none());
    }

    #[tokio::test]
    async fn fk_constraint_sessions() {
        let pool = setup_db().await;
        // Valid session_id should work
        let result = insert_execution_node(
            &pool,
            "sess1",
            None,
            1000,
            "tool_call",
            None,
            None,
            None,
            "/home",
            10,
        )
        .await;
        assert!(result.is_ok());

        // Enable FK enforcement check -- SQLite FKs may not be enforced by default in all configs.
        // We verify the constraint exists by checking the schema.
        let fk_check: Vec<(i64, i64, String, String)> = sqlx::query_as(
            "SELECT \"id\", \"seq\", \"table\", \"from\" FROM pragma_foreign_key_list('execution_nodes')",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(!fk_check.is_empty(), "FK constraint should exist");
    }

    #[tokio::test]
    async fn retention_delete_old_nodes() {
        let pool = setup_db().await;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let old_ms = now_ms - 31 * 24 * 60 * 60 * 1000; // 31 days ago

        insert_execution_node(
            &pool,
            "sess1",
            None,
            old_ms,
            "tool_call",
            None,
            None,
            None,
            "/home",
            10,
        )
        .await
        .unwrap();
        insert_execution_node(
            &pool,
            "sess1",
            None,
            now_ms,
            "tool_call",
            None,
            None,
            None,
            "/home",
            10,
        )
        .await
        .unwrap();

        let deleted = delete_old_execution_nodes(&pool, 30).await.unwrap();
        assert_eq!(deleted, 1);

        let remaining = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].timestamp, now_ms);
    }

    #[tokio::test]
    async fn retention_enforce_session_cap() {
        let pool = setup_db().await;
        for i in 0..15 {
            insert_execution_node(
                &pool,
                "sess1",
                None,
                1000 + i,
                "tool_call",
                None,
                None,
                None,
                "/home",
                10,
            )
            .await
            .unwrap();
        }

        let deleted = enforce_session_node_cap(&pool, "sess1", 10).await.unwrap();
        assert_eq!(deleted, 5);

        let remaining = list_execution_nodes(&pool, "sess1", 100, 0).await.unwrap();
        assert_eq!(remaining.len(), 10);
        // Oldest should have been removed
        assert_eq!(remaining[0].timestamp, 1005);
    }

    #[tokio::test]
    async fn empty_session_returns_empty_list() {
        let pool = setup_db().await;
        let nodes = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        assert!(nodes.is_empty());
        let count = count_execution_nodes(&pool, "sess1").await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn pagination_offset_beyond_total_returns_empty() {
        let pool = setup_db().await;
        for i in 0..3 {
            insert_execution_node(
                &pool,
                "sess1",
                None,
                1000 + i,
                "tool_call",
                None,
                None,
                None,
                "/home",
                10,
            )
            .await
            .unwrap();
        }

        let nodes = list_execution_nodes(&pool, "sess1", 10, 100).await.unwrap();
        assert!(
            nodes.is_empty(),
            "offset beyond total count should return empty list"
        );
    }

    #[tokio::test]
    async fn cleanup_zero_days_deletes_all() {
        let pool = setup_db().await;
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Insert a node with current timestamp
        insert_execution_node(
            &pool,
            "sess1",
            None,
            now_ms,
            "tool_call",
            None,
            None,
            None,
            "/home",
            10,
        )
        .await
        .unwrap();
        // Insert a node 1 second ago
        insert_execution_node(
            &pool,
            "sess1",
            None,
            now_ms - 1000,
            "tool_call",
            None,
            None,
            None,
            "/home",
            10,
        )
        .await
        .unwrap();

        // 0 days means cutoff = now, so all nodes with timestamp < now are deleted
        let deleted = delete_old_execution_nodes(&pool, 0).await.unwrap();
        assert!(deleted >= 1, "should delete nodes older than now");

        let remaining = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        // At most the node inserted at exactly `now_ms` might survive
        assert!(remaining.len() <= 1);
    }
}
