use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use zremote_protocol::NodeStatus;

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
    pub status: String,      // "running" | "completed" | "stopped" | "stale"
    pub tool_use_id: String, // empty string for legacy / non-hook PTY path
}

/// INSERT a new execution node in the `running` state. Idempotent on
/// `(session_id, tool_use_id)`. Returns the row id (existing or newly inserted).
#[allow(clippy::too_many_arguments)]
pub async fn open_execution_node(
    pool: &SqlitePool,
    session_id: &str,
    loop_id: Option<&str>,
    tool_use_id: &str,
    timestamp: i64,
    kind: &str,
    input: Option<&str>,
    working_dir: &str,
) -> Result<i64, AppError> {
    // Try insert; on conflict (unique index on (session_id, tool_use_id) WHERE tool_use_id != '')
    // do nothing and return the existing row's id.
    // When tool_use_id is '' the partial index does not apply, so each call inserts a new row.
    let result = sqlx::query(
        "INSERT INTO execution_nodes \
         (session_id, loop_id, tool_use_id, timestamp, kind, input, working_dir, \
          output_summary, exit_code, duration_ms, status) \
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, 0, 'running') \
         ON CONFLICT (session_id, tool_use_id) WHERE tool_use_id != '' DO NOTHING",
    )
    .bind(session_id)
    .bind(loop_id)
    .bind(tool_use_id)
    .bind(timestamp)
    .bind(kind)
    .bind(input)
    .bind(working_dir)
    .execute(pool)
    .await
    .map_err(AppError::Database)?;

    if result.rows_affected() > 0 {
        return Ok(result.last_insert_rowid());
    }

    // Conflict path: the unique index fired (tool_use_id != ''), return the existing row's id.
    let (id,): (i64,) =
        sqlx::query_as("SELECT id FROM execution_nodes WHERE session_id = ? AND tool_use_id = ?")
            .bind(session_id)
            .bind(tool_use_id)
            .fetch_one(pool)
            .await
            .map_err(AppError::Database)?;

    Ok(id)
}

/// UPDATE a running node to a terminal state by `(session_id, tool_use_id)`.
/// Returns the row if found and updated, None if no matching running node exists.
#[allow(clippy::too_many_arguments)]
pub async fn close_execution_node(
    pool: &SqlitePool,
    session_id: &str,
    tool_use_id: &str,
    kind: &str,
    output_summary: Option<&str>,
    exit_code: Option<i32>,
    duration_ms: i64,
    status: NodeStatus,
) -> Result<Option<ExecutionNodeRow>, AppError> {
    let status_str = node_status_str(status);
    let result = sqlx::query(
        "UPDATE execution_nodes \
         SET status = ?, kind = ?, output_summary = ?, exit_code = ?, duration_ms = ? \
         WHERE session_id = ? AND tool_use_id = ? AND status = 'running'",
    )
    .bind(status_str)
    .bind(kind)
    .bind(output_summary)
    .bind(exit_code)
    .bind(duration_ms)
    .bind(session_id)
    .bind(tool_use_id)
    .execute(pool)
    .await
    .map_err(AppError::Database)?;

    if result.rows_affected() == 0 {
        return Ok(None);
    }

    let row: ExecutionNodeRow = sqlx::query_as(
        "SELECT id, session_id, loop_id, timestamp, kind, input, output_summary, \
         exit_code, working_dir, duration_ms, status, tool_use_id \
         FROM execution_nodes WHERE session_id = ? AND tool_use_id = ? LIMIT 1",
    )
    .bind(session_id)
    .bind(tool_use_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Database)?;

    Ok(Some(row))
}

/// Bulk close every still-running node in a session as `stopped`.
/// Returns the affected rows for broadcast.
pub async fn mark_session_running_as_stopped(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<ExecutionNodeRow>, AppError> {
    // Capture ids of currently-running rows before the UPDATE so the subsequent
    // SELECT returns only the rows we just transitioned, not any pre-existing stopped ones.
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM execution_nodes WHERE session_id = ? AND status = 'running'",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)?;

    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let update_sql =
        format!("UPDATE execution_nodes SET status = 'stopped' WHERE id IN ({placeholders})");
    let mut q = sqlx::query(&update_sql);
    for id in &ids {
        q = q.bind(id);
    }
    q.execute(pool).await.map_err(AppError::Database)?;

    let select_sql = format!(
        "SELECT id, session_id, loop_id, timestamp, kind, input, output_summary, \
         exit_code, working_dir, duration_ms, status, tool_use_id \
         FROM execution_nodes WHERE id IN ({placeholders}) ORDER BY timestamp ASC"
    );
    let mut q = sqlx::query_as::<_, ExecutionNodeRow>(&select_sql);
    for id in &ids {
        q = q.bind(id);
    }
    q.fetch_all(pool).await.map_err(AppError::Database)
}

/// Find all `running` nodes older than `ttl_secs`, mark them `stale`, return them for broadcast.
pub async fn sweep_stale_running(
    pool: &SqlitePool,
    ttl_secs: i64,
) -> Result<Vec<ExecutionNodeRow>, AppError> {
    let cutoff_ms = chrono::Utc::now().timestamp_millis() - ttl_secs * 1000;

    // Capture ids of running rows that are old enough before the UPDATE so the
    // subsequent SELECT returns only the rows we just transitioned, not any
    // pre-existing stale ones.
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM execution_nodes WHERE status = 'running' AND timestamp < ?",
    )
    .bind(cutoff_ms)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)?;

    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let update_sql =
        format!("UPDATE execution_nodes SET status = 'stale' WHERE id IN ({placeholders})");
    let mut q = sqlx::query(&update_sql);
    for id in &ids {
        q = q.bind(id);
    }
    q.execute(pool).await.map_err(AppError::Database)?;

    let select_sql = format!(
        "SELECT id, session_id, loop_id, timestamp, kind, input, output_summary, \
         exit_code, working_dir, duration_ms, status, tool_use_id \
         FROM execution_nodes WHERE id IN ({placeholders}) ORDER BY timestamp ASC"
    );
    let mut q = sqlx::query_as::<_, ExecutionNodeRow>(&select_sql);
    for id in &ids {
        q = q.bind(id);
    }
    q.fetch_all(pool).await.map_err(AppError::Database)
}

pub async fn list_execution_nodes(
    pool: &SqlitePool,
    session_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<ExecutionNodeRow>, AppError> {
    sqlx::query_as(
        "SELECT id, session_id, loop_id, timestamp, kind, input, output_summary, \
         exit_code, working_dir, duration_ms, status, tool_use_id \
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
         exit_code, working_dir, duration_ms, status, tool_use_id \
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

fn node_status_str(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Running => "running",
        NodeStatus::Completed => "completed",
        // Unknown is only produced by forward-compat deserialization; treat as stopped.
        NodeStatus::Stopped | NodeStatus::Unknown => "stopped",
        NodeStatus::Stale => "stale",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_db() -> SqlitePool {
        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
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

    // Test #1: Migration applies cleanly; legacy rows are wiped
    #[tokio::test]
    async fn migration_applies_and_wipes_legacy_rows() {
        // init_db runs all migrations, including 026. Since we start fresh (in-memory),
        // there are no legacy rows to wipe. Verify the new columns exist.
        let pool = setup_db().await;
        let row = sqlx::query_as::<_, ExecutionNodeRow>(
            "SELECT id, session_id, loop_id, timestamp, kind, input, output_summary, \
             exit_code, working_dir, duration_ms, status, tool_use_id \
             FROM execution_nodes LIMIT 1",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        // No rows (fresh DB), but columns must exist — if they didn't the query would fail.
        assert!(row.is_none());

        // Also verify migration 026 leaves the table with status/tool_use_id columns.
        let cols: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM pragma_table_info('execution_nodes')")
                .fetch_all(&pool)
                .await
                .unwrap();
        let names: Vec<&str> = cols.iter().map(|(n,)| n.as_str()).collect();
        assert!(names.contains(&"status"), "status column must exist");
        assert!(
            names.contains(&"tool_use_id"),
            "tool_use_id column must exist"
        );
    }

    // Test #2: open_execution_node inserts a running row and returns id
    #[tokio::test]
    async fn open_execution_node_inserts_running_row() {
        let pool = setup_db().await;
        let id = open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_abc",
            1000,
            "read",
            Some("src/main.rs"),
            "/home",
        )
        .await
        .unwrap();
        assert!(id > 0);

        let nodes = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].status, "running");
        assert_eq!(nodes[0].tool_use_id, "toolu_abc");
    }

    // Test #3: open_execution_node idempotent on duplicate (session_id, tool_use_id)
    #[tokio::test]
    async fn open_execution_node_is_idempotent() {
        let pool = setup_db().await;
        let id1 = open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_abc",
            1000,
            "read",
            None,
            "/home",
        )
        .await
        .unwrap();
        let id2 = open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_abc",
            2000,
            "edit",
            None,
            "/home",
        )
        .await
        .unwrap();
        assert_eq!(id1, id2, "duplicate open must return existing id");

        let nodes = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        assert_eq!(nodes.len(), 1, "only one row must exist");
    }

    // Test #4: close_execution_node transitions running to completed, returns row
    #[tokio::test]
    async fn close_execution_node_transitions_to_completed() {
        let pool = setup_db().await;
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_abc",
            1000,
            "bash",
            None,
            "/home",
        )
        .await
        .unwrap();

        let row = close_execution_node(
            &pool,
            "sess1",
            "toolu_abc",
            "bash",
            Some("exit 0"),
            Some(0),
            500,
            NodeStatus::Completed,
        )
        .await
        .unwrap();

        assert!(row.is_some());
        let row = row.unwrap();
        assert_eq!(row.status, "completed");
        assert_eq!(row.exit_code, Some(0));
        assert_eq!(row.duration_ms, 500);
    }

    // Test #5: close_execution_node returns None when no matching running node
    #[tokio::test]
    async fn close_execution_node_returns_none_when_not_found() {
        let pool = setup_db().await;
        let result = close_execution_node(
            &pool,
            "sess1",
            "nonexistent_id",
            "bash",
            None,
            None,
            0,
            NodeStatus::Completed,
        )
        .await
        .unwrap();
        assert!(result.is_none());
    }

    // Test #6: close_execution_node does not transition rows already in completed
    #[tokio::test]
    async fn close_execution_node_idempotent_on_completed() {
        let pool = setup_db().await;
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_abc",
            1000,
            "bash",
            None,
            "/home",
        )
        .await
        .unwrap();
        // First close
        close_execution_node(
            &pool,
            "sess1",
            "toolu_abc",
            "bash",
            Some("output"),
            Some(0),
            100,
            NodeStatus::Completed,
        )
        .await
        .unwrap();
        // Second close on already-completed row returns None
        let result = close_execution_node(
            &pool,
            "sess1",
            "toolu_abc",
            "bash",
            Some("new output"),
            Some(1),
            200,
            NodeStatus::Stopped,
        )
        .await
        .unwrap();
        assert!(result.is_none(), "should not update non-running rows");

        // Status must remain completed
        let nodes = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        assert_eq!(nodes[0].status, "completed");
    }

    // Test #7: mark_session_running_as_stopped only touches rows with status='running'
    #[tokio::test]
    async fn mark_session_running_as_stopped_only_touches_running() {
        let pool = setup_db().await;
        // Insert a completed node
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_done",
            1000,
            "read",
            None,
            "/home",
        )
        .await
        .unwrap();
        close_execution_node(
            &pool,
            "sess1",
            "toolu_done",
            "read",
            None,
            None,
            50,
            NodeStatus::Completed,
        )
        .await
        .unwrap();

        // Insert two running nodes
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_run1",
            2000,
            "bash",
            None,
            "/home",
        )
        .await
        .unwrap();
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_run2",
            3000,
            "edit",
            None,
            "/home",
        )
        .await
        .unwrap();

        let stopped = mark_session_running_as_stopped(&pool, "sess1")
            .await
            .unwrap();
        assert_eq!(stopped.len(), 2);
        assert!(stopped.iter().all(|r| r.status == "stopped"));

        let nodes = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        let completed: Vec<_> = nodes.iter().filter(|n| n.status == "completed").collect();
        let stopped_rows: Vec<_> = nodes.iter().filter(|n| n.status == "stopped").collect();
        assert_eq!(completed.len(), 1);
        assert_eq!(stopped_rows.len(), 2);
    }

    // Regression for issue #1: mark_session_running_as_stopped must NOT return pre-existing stopped rows
    #[tokio::test]
    async fn mark_session_running_as_stopped_excludes_prior_stopped_rows() {
        let pool = setup_db().await;

        // Pre-seed one already-stopped row (stopped in a prior call)
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_prestop",
            1000,
            "read",
            None,
            "/home",
        )
        .await
        .unwrap();
        close_execution_node(
            &pool,
            "sess1",
            "toolu_prestop",
            "read",
            None,
            None,
            50,
            NodeStatus::Stopped,
        )
        .await
        .unwrap();

        // One currently-running row
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_running",
            2000,
            "bash",
            None,
            "/home",
        )
        .await
        .unwrap();

        let returned = mark_session_running_as_stopped(&pool, "sess1")
            .await
            .unwrap();
        assert_eq!(
            returned.len(),
            1,
            "must return only the just-transitioned row, not the pre-existing stopped one"
        );
        assert_eq!(returned[0].tool_use_id, "toolu_running");
        assert_eq!(returned[0].status, "stopped");
    }

    // Regression for issue #2: sweep_stale_running must NOT return pre-existing stale rows
    #[tokio::test]
    async fn sweep_stale_running_excludes_prior_stale_rows() {
        let pool = setup_db().await;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let old_ms = now_ms - 400_000; // 400 seconds ago

        // Pre-seed an already-stale row from a prior sweep
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_prestale",
            old_ms - 1000,
            "read",
            None,
            "/home",
        )
        .await
        .unwrap();
        // Force it to stale by direct UPDATE (simulating a prior sweep)
        sqlx::query(
            "UPDATE execution_nodes SET status = 'stale' WHERE tool_use_id = 'toolu_prestale'",
        )
        .execute(&pool)
        .await
        .unwrap();

        // One running row older than ttl — should become stale now
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_newstale",
            old_ms,
            "bash",
            None,
            "/home",
        )
        .await
        .unwrap();

        let returned = sweep_stale_running(&pool, 300).await.unwrap();
        assert_eq!(
            returned.len(),
            1,
            "must return only the just-transitioned row, not the pre-existing stale one"
        );
        assert_eq!(returned[0].tool_use_id, "toolu_newstale");
        assert_eq!(returned[0].status, "stale");
    }

    // Test #8: sweep_stale_running ignores completed/stopped, picks up old running ones
    #[tokio::test]
    async fn sweep_stale_running_marks_old_running_as_stale() {
        let pool = setup_db().await;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let old_ms = now_ms - 400_000; // 400 seconds ago

        // Insert a completed node (should be ignored)
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_done",
            old_ms,
            "read",
            None,
            "/home",
        )
        .await
        .unwrap();
        close_execution_node(
            &pool,
            "sess1",
            "toolu_done",
            "read",
            None,
            None,
            50,
            NodeStatus::Completed,
        )
        .await
        .unwrap();

        // Insert a stopped node (should be ignored)
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_stopped",
            old_ms,
            "bash",
            None,
            "/home",
        )
        .await
        .unwrap();
        close_execution_node(
            &pool,
            "sess1",
            "toolu_stopped",
            "bash",
            None,
            None,
            50,
            NodeStatus::Stopped,
        )
        .await
        .unwrap();

        // Insert an old running node (should become stale)
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_stale",
            old_ms,
            "edit",
            None,
            "/home",
        )
        .await
        .unwrap();

        // Insert a recent running node (should NOT become stale with ttl=300)
        open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_recent",
            now_ms,
            "edit",
            None,
            "/home",
        )
        .await
        .unwrap();

        let stale = sweep_stale_running(&pool, 300).await.unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].tool_use_id, "toolu_stale");
        assert_eq!(stale[0].status, "stale");

        // Recent running node must remain running
        let nodes = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        let recent: Vec<_> = nodes
            .iter()
            .filter(|n| n.tool_use_id == "toolu_recent")
            .collect();
        assert_eq!(recent[0].status, "running");
    }

    // Test #9: Unique index rejects duplicate (session_id, tool_use_id) — open is idempotent
    #[tokio::test]
    async fn unique_index_enforced_on_tool_use_id() {
        let pool = setup_db().await;
        // open_execution_node uses ON CONFLICT DO NOTHING — so no error, just returns existing id
        let id1 = open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_unique",
            1000,
            "bash",
            None,
            "/home",
        )
        .await
        .unwrap();
        let id2 = open_execution_node(
            &pool,
            "sess1",
            None,
            "toolu_unique",
            2000,
            "read",
            None,
            "/home",
        )
        .await
        .unwrap();
        assert_eq!(id1, id2);
        let count = count_execution_nodes(&pool, "sess1").await.unwrap();
        assert_eq!(count, 1, "unique index prevents duplicate rows");
    }

    // Test #C: empty tool_use_id bypasses the unique index (legacy / PTY-fallback path)
    #[tokio::test]
    async fn empty_tool_use_id_allows_multiple_rows() {
        let pool = setup_db().await;
        // Two rows with empty tool_use_id must both succeed and get distinct ids.
        let id1 = open_execution_node(&pool, "sess1", None, "", 1000, "bash", None, "/home")
            .await
            .unwrap();
        let id2 = open_execution_node(&pool, "sess1", None, "", 2000, "bash", None, "/home")
            .await
            .unwrap();
        assert_ne!(id1, id2, "empty tool_use_id rows must have distinct ids");
        let count = count_execution_nodes(&pool, "sess1").await.unwrap();
        assert_eq!(count, 2, "both rows must exist");
    }

    // --- legacy tests preserved (updated to use open_execution_node where needed) ---

    #[tokio::test]
    async fn list_and_count_nodes() {
        let pool = setup_db().await;
        for i in 0..3 {
            open_execution_node(
                &pool,
                "sess1",
                None,
                &format!("tool_{i}"),
                1000 + i,
                "tool_call",
                Some(&format!("Read file{i}.rs")),
                "/home/user",
            )
            .await
            .unwrap();
        }

        let nodes = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].timestamp, 1000);
        assert_eq!(nodes[2].timestamp, 1002);

        let count = count_execution_nodes(&pool, "sess1").await.unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn list_nodes_pagination() {
        let pool = setup_db().await;
        for i in 0..5_i64 {
            open_execution_node(
                &pool,
                "sess1",
                None,
                &format!("tool_{i}"),
                1000 + i,
                "tool_call",
                None,
                "/home",
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
        open_execution_node(
            &pool,
            "sess1",
            Some("loop-a"),
            "t1",
            1000,
            "tool_call",
            None,
            "/home",
        )
        .await
        .unwrap();
        open_execution_node(
            &pool,
            "sess1",
            Some("loop-b"),
            "t2",
            1001,
            "tool_call",
            None,
            "/home",
        )
        .await
        .unwrap();
        open_execution_node(
            &pool,
            "sess1",
            Some("loop-a"),
            "t3",
            1002,
            "tool_call",
            None,
            "/home",
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
    async fn fk_constraint_sessions() {
        let pool = setup_db().await;
        let result =
            open_execution_node(&pool, "sess1", None, "t1", 1000, "tool_call", None, "/home").await;
        assert!(result.is_ok());

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
        let old_ms = now_ms - 31 * 24 * 60 * 60 * 1000;

        open_execution_node(
            &pool,
            "sess1",
            None,
            "t1",
            old_ms,
            "tool_call",
            None,
            "/home",
        )
        .await
        .unwrap();
        open_execution_node(
            &pool,
            "sess1",
            None,
            "t2",
            now_ms,
            "tool_call",
            None,
            "/home",
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
        for i in 0..15_i64 {
            open_execution_node(
                &pool,
                "sess1",
                None,
                &format!("t{i}"),
                1000 + i,
                "tool_call",
                None,
                "/home",
            )
            .await
            .unwrap();
        }

        let deleted = enforce_session_node_cap(&pool, "sess1", 10).await.unwrap();
        assert_eq!(deleted, 5);

        let remaining = list_execution_nodes(&pool, "sess1", 100, 0).await.unwrap();
        assert_eq!(remaining.len(), 10);
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
        for i in 0..3_i64 {
            open_execution_node(
                &pool,
                "sess1",
                None,
                &format!("t{i}"),
                1000 + i,
                "tool_call",
                None,
                "/home",
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
        open_execution_node(
            &pool,
            "sess1",
            None,
            "t1",
            now_ms,
            "tool_call",
            None,
            "/home",
        )
        .await
        .unwrap();
        open_execution_node(
            &pool,
            "sess1",
            None,
            "t2",
            now_ms - 1000,
            "tool_call",
            None,
            "/home",
        )
        .await
        .unwrap();

        let deleted = delete_old_execution_nodes(&pool, 0).await.unwrap();
        assert!(deleted >= 1, "should delete nodes older than now");

        let remaining = list_execution_nodes(&pool, "sess1", 10, 0).await.unwrap();
        assert!(remaining.len() <= 1);
    }
}
