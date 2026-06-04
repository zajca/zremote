use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;

/// Session representation for API responses.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct SessionRow {
    pub id: String,
    pub host_id: String,
    pub name: Option<String>,
    pub shell: Option<String>,
    pub status: String,
    pub working_dir: Option<String>,
    pub project_id: Option<String>,
    pub pid: Option<i64>,
    pub exit_code: Option<i32>,
    pub created_at: String,
    pub closed_at: Option<String>,
}

pub async fn host_exists(pool: &SqlitePool, host_id: &str) -> Result<bool, AppError> {
    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM hosts WHERE id = ?")
        .bind(host_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

pub async fn resolve_project_id(
    pool: &SqlitePool,
    host_id: &str,
    working_dir: &str,
) -> Result<Option<String>, AppError> {
    // Prefer the most specific (longest path) match. A git worktree placed
    // inside its parent repo (e.g. `/repo/.worktrees/feat`) also matches the
    // parent's path prefix, so a naked LIMIT 1 could bind the session to the
    // parent project — which then breaks worktree deletion (we can't find
    // the worktree's own sessions) and muddles project-scoped queries.
    //
    // Use SUBSTR-based prefix matching rather than LIKE: a project path stored
    // with a literal `%` or `_` (SQLite LIKE wildcards) would otherwise match
    // unrelated working directories.
    let project_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM projects WHERE host_id = ? \
           AND (? = path OR SUBSTR(?, 1, LENGTH(path) + 1) = path || '/') \
         ORDER BY LENGTH(path) DESC LIMIT 1",
    )
    .bind(host_id)
    .bind(working_dir)
    .bind(working_dir)
    .fetch_optional(pool)
    .await?;
    Ok(project_id)
}

pub async fn insert_session(
    pool: &SqlitePool,
    session_id: &str,
    host_id: &str,
    name: Option<&str>,
    working_dir: Option<&str>,
    project_id: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sessions (id, host_id, name, status, working_dir, project_id) VALUES (?, ?, ?, 'creating', ?, ?)",
    )
    .bind(session_id)
    .bind(host_id)
    .bind(name)
    .bind(working_dir)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_sessions(pool: &SqlitePool, host_id: &str) -> Result<Vec<SessionRow>, AppError> {
    let sessions: Vec<SessionRow> = sqlx::query_as(
        "SELECT id, host_id, name, shell, status, working_dir, project_id, pid, exit_code, created_at, closed_at \
         FROM sessions WHERE host_id = ? AND status != 'closed' ORDER BY created_at DESC",
    )
    .bind(host_id)
    .fetch_all(pool)
    .await?;
    Ok(sessions)
}

pub async fn get_session(pool: &SqlitePool, session_id: &str) -> Result<SessionRow, AppError> {
    let session: SessionRow = sqlx::query_as(
        "SELECT id, host_id, name, shell, status, working_dir, project_id, pid, exit_code, created_at, closed_at \
         FROM sessions WHERE id = ?",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("session {session_id} not found")))?;
    Ok(session)
}

pub async fn update_session_name(
    pool: &SqlitePool,
    session_id: &str,
    name: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query("UPDATE sessions SET name = ? WHERE id = ?")
        .bind(name)
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn find_session_for_close(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<(String, String)>, AppError> {
    let session: Option<(String, String)> =
        sqlx::query_as("SELECT id, host_id FROM sessions WHERE id = ? AND status != 'closed'")
            .bind(session_id)
            .fetch_optional(pool)
            .await?;
    Ok(session)
}

pub async fn get_session_status(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<String>, AppError> {
    let status: Option<(String,)> = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
    Ok(status.map(|(s,)| s))
}

pub async fn purge_session(pool: &SqlitePool, session_id: &str) -> Result<(), AppError> {
    // Nullify session_id on agentic_loops (preserve loop data)
    sqlx::query("UPDATE agentic_loops SET session_id = NULL WHERE session_id = ?")
        .bind(session_id)
        .execute(pool)
        .await?;

    // Delete the session row
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(session_id)
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn list_suspended_session_ids(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Vec<String>, AppError> {
    let ids: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM sessions WHERE host_id = ? AND status = 'suspended'")
            .bind(host_id)
            .fetch_all(pool)
            .await?;
    Ok(ids.into_iter().map(|(id,)| id).collect())
}

pub async fn force_close_session(pool: &SqlitePool, session_id: &str) -> Result<(), AppError> {
    let now = chrono::Utc::now().to_rfc3339();
    force_close_session_at(pool, session_id, &now).await
}

/// Like [`force_close_session`] but with a caller-supplied `closed_at`
/// timestamp, so a batch (e.g. startup recovery) can stamp every closed row with
/// the same instant.
pub async fn force_close_session_at(
    pool: &SqlitePool,
    session_id: &str,
    closed_at: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE sessions SET status = 'closed', closed_at = ? WHERE id = ?")
        .bind(closed_at)
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// List ALL suspended sessions for a host, each paired with its (possibly null)
/// `agent_session_ref` (RFC-012). The query does NOT filter by a non-null ref —
/// the caller inspects the `Option` to decide. Named for the column it returns,
/// not a filter, to avoid implying only agent sessions come back.
///
/// Used by startup recovery to classify a session whose daemon did not survive
/// the restart: `Some(ref)` means the row backs an agent conversation we can
/// re-open (-> `resumable`); `None` is a plain session (-> `closed`).
pub async fn list_suspended_sessions_with_optional_agent_ref(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Vec<(String, Option<String>)>, AppError> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT id, agent_session_ref FROM sessions \
         WHERE host_id = ? AND status = 'suspended'",
    )
    .bind(host_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Mark a session as `resumable` (RFC-013): its backend did not survive an
/// agent restart, but it can be re-opened. The row stays listed
/// (`list_sessions` includes everything `!= 'closed'`) and attach drives the
/// resume engine. `suspended_at` is preserved as the time the backend went away.
pub async fn mark_session_resumable(pool: &SqlitePool, session_id: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE sessions SET status = 'resumable' WHERE id = ?")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Reconcile a Claude task whose backing terminal session just became
/// `resumable` on startup (RFC-013 "Claude-task reconciliation").
///
/// Without this, the `claude_sessions` row stays `active`/`starting` and the
/// sidebar shows a task that maps to a now-dead `session_id` — the "shows but
/// cannot continue" symptom. We move it to `suspended` with a `disconnect_reason`,
/// mirroring the existing agent-disconnect reconciliation (a suspended Claude
/// task is the established "alive but not running, can be resumed" state). Only
/// rows in a live state (`starting`/`active`) are touched, so terminal tasks
/// (`completed`/`error`) are left intact. Returns the number of rows updated.
pub async fn reconcile_claude_session_resumable(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<u64, AppError> {
    let result = sqlx::query(
        "UPDATE claude_sessions \
         SET status = 'suspended', disconnect_reason = 'agent_restarted' \
         WHERE session_id = ? AND status IN ('starting', 'active')",
    )
    .bind(session_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn list_sessions_by_project(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<SessionRow>, AppError> {
    let sessions: Vec<SessionRow> = sqlx::query_as(
        "SELECT id, host_id, name, shell, status, working_dir, project_id, pid, exit_code, created_at, closed_at \
         FROM sessions WHERE project_id = ? ORDER BY created_at DESC",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(sessions)
}

/// List active sessions whose `working_dir` is inside (or equal to) `path`.
///
/// Complements `list_sessions_by_project` for the worktree-deletion path: a
/// session spawned before `resolve_project_id` was fixed may carry the parent
/// project's id but actually live inside a worktree subdirectory. Matching on
/// path catches those rows so we can still tear them down before running
/// `git worktree remove`.
pub async fn list_active_sessions_under_path(
    pool: &SqlitePool,
    host_id: &str,
    path: &str,
) -> Result<Vec<SessionRow>, AppError> {
    // SUBSTR prefix match (not LIKE) so that a path containing SQLite LIKE
    // wildcards (`%`, `_`) doesn't over-select sessions under unrelated
    // directories.
    let sessions: Vec<SessionRow> = sqlx::query_as(
        "SELECT id, host_id, name, shell, status, working_dir, project_id, pid, exit_code, created_at, closed_at \
         FROM sessions \
         WHERE host_id = ? AND status != 'closed' \
           AND working_dir IS NOT NULL \
           AND (working_dir = ? OR SUBSTR(working_dir, 1, LENGTH(?) + 1) = ? || '/') \
         ORDER BY created_at DESC",
    )
    .bind(host_id)
    .bind(path)
    .bind(path)
    .bind(path)
    .fetch_all(pool)
    .await?;
    Ok(sessions)
}

/// Persist an agent's native session id for a managed session.
///
/// Records which agent CLI (`agent_kind`, e.g. `claude` / `codex`) produced
/// `native_session_id` and when it was last observed (`now`, ISO 8601). This
/// row is the durable record RFC-013 reads to build the resume command. The
/// caller supplies `now` so the timestamp source is explicit (mirrors how
/// `mark_session_error`/`force_close_session` mint their own `rfc3339`).
///
/// The `native_session_id` is treated as opaque data — it is only ever bound
/// as a parameter, never interpolated into SQL or shell text.
pub async fn set_agent_session_ref(
    pool: &SqlitePool,
    session_id: &str,
    agent_kind: &str,
    native_session_id: &str,
    now: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sessions \
         SET agent_kind = ?, agent_session_ref = ?, agent_session_updated_at = ? \
         WHERE id = ?",
    )
    .bind(agent_kind)
    .bind(native_session_id)
    .bind(now)
    .bind(session_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Read the persisted agent identity for a session (RFC-012/013).
///
/// Returns `Some((agent_kind, agent_session_ref))` only when BOTH columns are
/// non-null — i.e. the session backs an agent conversation that can be resumed.
/// Returns `None` for a plain session or one that never captured a native id.
/// The resume engine maps `agent_kind` back to an `AgentKind` and builds the
/// resume argv from `agent_session_ref`.
pub async fn get_agent_session_ref(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<(String, String)>, AppError> {
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT agent_kind, agent_session_ref FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(kind, native)| Some((kind?, native?))))
}

/// Transition a session to `active` after a successful resume (RFC-013).
///
/// Clears `suspended_at` and `closed_at` so a previously `resumable` row looks
/// like a normal live session again. The same `sessions.id` is reused, so GUI
/// handles and any `claude_sessions` linkage stay stable.
pub async fn mark_session_active(pool: &SqlitePool, session_id: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sessions SET status = 'active', suspended_at = NULL, closed_at = NULL \
         WHERE id = ?",
    )
    .bind(session_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a session row as errored.
///
/// Used by the server-side `AgentLifecycle::StartFailed` handler to surface
/// launcher failures to the UI instead of leaving a session stuck in
/// `creating`. Sets `status = 'error'` and `closed_at = now()`. The
/// `sessions` table has no `updated_at` column (see migrations/001).
pub async fn mark_session_error(pool: &SqlitePool, session_id: &str) -> Result<(), AppError> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE sessions SET status = 'error', closed_at = ? WHERE id = ?")
        .bind(&now)
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> SqlitePool {
        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('h1', 'h1', 'h1', 'hash', 'online')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn mark_session_error_transitions_status_and_sets_closed_at() {
        let pool = setup().await;
        insert_session(&pool, "s1", "h1", None, None, None)
            .await
            .unwrap();
        mark_session_error(&pool, "s1").await.unwrap();
        let row = get_session(&pool, "s1").await.unwrap();
        assert_eq!(row.status, "error");
        assert!(row.closed_at.is_some());
    }

    #[tokio::test]
    async fn resolve_project_id_prefers_longest_matching_path() {
        // Regression guard: a worktree nested inside its parent repo
        // (e.g. `/repo/.worktrees/feat`) matches both the parent and itself
        // via `LIKE path || '/%'`. A naked `LIMIT 1` used to pick whichever
        // row the engine returned first, so sessions created inside the
        // worktree could be bound to the parent project — which then broke
        // worktree deletion. The longest-prefix rule must tie them to the
        // worktree.
        let pool = setup().await;
        let parent_id = "p-parent";
        let wt_id = "p-wt";
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, project_type) \
             VALUES (?, 'h1', '/repo', 'repo', 'git')",
        )
        .bind(parent_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, parent_project_id, project_type) \
             VALUES (?, 'h1', '/repo/.worktrees/feat', 'feat', ?, 'worktree')",
        )
        .bind(wt_id)
        .bind(parent_id)
        .execute(&pool)
        .await
        .unwrap();

        let resolved = resolve_project_id(&pool, "h1", "/repo/.worktrees/feat/src")
            .await
            .unwrap();
        assert_eq!(resolved.as_deref(), Some(wt_id));

        let parent_only = resolve_project_id(&pool, "h1", "/repo/src").await.unwrap();
        assert_eq!(parent_only.as_deref(), Some(parent_id));
    }

    #[tokio::test]
    async fn resolve_project_id_is_robust_to_like_wildcards_in_path() {
        // Regression guard: prior implementation used `LIKE path || '/%'`
        // where `%` and `_` in the working_dir or the stored path would act
        // as wildcards, potentially matching unrelated projects.
        let pool = setup().await;
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, project_type) \
             VALUES ('p1', 'h1', '/tmp/foo%bar', 'foo%bar', 'git')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, project_type) \
             VALUES ('p2', 'h1', '/tmp/foo_bar', 'foo_bar', 'git')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // `/tmp/fooXbar/inside` must NOT match `/tmp/foo%bar` (where `%`
        // is meant literally).
        let wrong = resolve_project_id(&pool, "h1", "/tmp/fooXbar/inside")
            .await
            .unwrap();
        assert_eq!(wrong, None);

        // Literal `%` in the working_dir matches the project that stores it.
        let right = resolve_project_id(&pool, "h1", "/tmp/foo%bar/inside")
            .await
            .unwrap();
        assert_eq!(right.as_deref(), Some("p1"));
    }

    #[tokio::test]
    async fn mark_session_error_is_noop_for_missing_row() {
        let pool = setup().await;
        // UPDATE affecting 0 rows must still return Ok so the
        // StartFailed handler can tolerate agent-side rejections that
        // happened before the server committed the session row.
        mark_session_error(&pool, "nonexistent").await.unwrap();
    }

    #[tokio::test]
    async fn migration_029_adds_agent_session_ref_columns() {
        // Guard that migration 029 applied and the three new columns exist on
        // `sessions`. A bare SELECT of the columns errors if any is missing.
        let pool = setup().await;
        insert_session(&pool, "s1", "h1", None, None, None)
            .await
            .unwrap();
        let row: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT agent_kind, agent_session_ref, agent_session_updated_at \
             FROM sessions WHERE id = 's1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        // Newly inserted session has no agent ref yet.
        assert_eq!(row, (None, None, None));
    }

    #[tokio::test]
    async fn set_agent_session_ref_persists_values() {
        let pool = setup().await;
        insert_session(&pool, "s1", "h1", None, None, None)
            .await
            .unwrap();

        set_agent_session_ref(
            &pool,
            "s1",
            "claude",
            "cc-native-abc",
            "2026-06-04T10:00:00Z",
        )
        .await
        .unwrap();

        let row: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT agent_kind, agent_session_ref, agent_session_updated_at \
             FROM sessions WHERE id = 's1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some("claude"));
        assert_eq!(row.1.as_deref(), Some("cc-native-abc"));
        assert_eq!(row.2.as_deref(), Some("2026-06-04T10:00:00Z"));
    }

    #[tokio::test]
    async fn set_agent_session_ref_overwrites_previous_values() {
        // A later capture (e.g. agent reconnect) must replace the stored ref.
        let pool = setup().await;
        insert_session(&pool, "s1", "h1", None, None, None)
            .await
            .unwrap();

        set_agent_session_ref(&pool, "s1", "claude", "first", "2026-06-04T10:00:00Z")
            .await
            .unwrap();
        set_agent_session_ref(&pool, "s1", "codex", "second", "2026-06-04T11:00:00Z")
            .await
            .unwrap();

        let row: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT agent_kind, agent_session_ref, agent_session_updated_at \
             FROM sessions WHERE id = 's1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some("codex"));
        assert_eq!(row.1.as_deref(), Some("second"));
        assert_eq!(row.2.as_deref(), Some("2026-06-04T11:00:00Z"));
    }

    #[tokio::test]
    async fn set_agent_session_ref_is_noop_for_missing_row() {
        // UPDATE affecting 0 rows returns Ok so the processing path can tolerate
        // a capture for a session that was purged/closed concurrently.
        let pool = setup().await;
        set_agent_session_ref(&pool, "nonexistent", "claude", "x", "2026-06-04T10:00:00Z")
            .await
            .unwrap();
    }

    async fn insert_suspended_session(
        pool: &SqlitePool,
        id: &str,
        agent_session_ref: Option<&str>,
    ) {
        insert_session(pool, id, "h1", None, None, None)
            .await
            .unwrap();
        sqlx::query("UPDATE sessions SET status = 'suspended', agent_session_ref = ? WHERE id = ?")
            .bind(agent_session_ref)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_suspended_sessions_with_optional_agent_ref_returns_only_suspended() {
        let pool = setup().await;
        insert_suspended_session(&pool, "s_agent", Some("cc-123")).await;
        insert_suspended_session(&pool, "s_plain", None).await;
        // An active session must NOT appear.
        insert_session(&pool, "s_active", "h1", None, None, None)
            .await
            .unwrap();
        sqlx::query("UPDATE sessions SET status = 'active' WHERE id = 's_active'")
            .execute(&pool)
            .await
            .unwrap();

        let mut rows = list_suspended_sessions_with_optional_agent_ref(&pool, "h1")
            .await
            .unwrap();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "s_agent");
        assert_eq!(rows[0].1.as_deref(), Some("cc-123"));
        assert_eq!(rows[1].0, "s_plain");
        assert_eq!(rows[1].1, None);
    }

    #[tokio::test]
    async fn mark_session_resumable_sets_status() {
        let pool = setup().await;
        insert_suspended_session(&pool, "s1", Some("cc-123")).await;

        mark_session_resumable(&pool, "s1").await.unwrap();

        let row = get_session(&pool, "s1").await.unwrap();
        assert_eq!(row.status, "resumable");
        // A resumable session is NOT closed, so it stays in list_sessions.
        let listed = list_sessions(&pool, "h1").await.unwrap();
        assert!(listed.iter().any(|s| s.id == "s1"));
    }

    #[tokio::test]
    async fn force_close_session_at_uses_supplied_timestamp() {
        let pool = setup().await;
        insert_suspended_session(&pool, "s1", None).await;

        force_close_session_at(&pool, "s1", "2026-06-04T09:00:00Z")
            .await
            .unwrap();

        let row = get_session(&pool, "s1").await.unwrap();
        assert_eq!(row.status, "closed");
        assert_eq!(row.closed_at.as_deref(), Some("2026-06-04T09:00:00Z"));
    }

    async fn insert_claude_task(pool: &SqlitePool, id: &str, session_id: &str, status: &str) {
        sqlx::query(
            "INSERT INTO claude_sessions (id, session_id, host_id, project_path, status) \
             VALUES (?, ?, 'h1', '/proj', ?)",
        )
        .bind(id)
        .bind(session_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn reconcile_claude_session_resumable_suspends_live_task() {
        let pool = setup().await;
        insert_suspended_session(&pool, "s1", Some("cc-123")).await;
        insert_claude_task(&pool, "t1", "s1", "active").await;

        let updated = reconcile_claude_session_resumable(&pool, "s1")
            .await
            .unwrap();
        assert_eq!(updated, 1);

        let (status, reason): (String, Option<String>) =
            sqlx::query_as("SELECT status, disconnect_reason FROM claude_sessions WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "suspended");
        assert_eq!(reason.as_deref(), Some("agent_restarted"));
    }

    #[tokio::test]
    async fn reconcile_claude_session_resumable_reconciles_starting_task() {
        let pool = setup().await;
        insert_suspended_session(&pool, "s1", Some("cc-123")).await;
        insert_claude_task(&pool, "t1", "s1", "starting").await;

        let updated = reconcile_claude_session_resumable(&pool, "s1")
            .await
            .unwrap();
        assert_eq!(updated, 1);

        let (status,): (String,) =
            sqlx::query_as("SELECT status FROM claude_sessions WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "suspended");
    }

    #[tokio::test]
    async fn reconcile_claude_session_resumable_leaves_terminal_task_untouched() {
        // A completed/error Claude task must not be revived into 'suspended'.
        let pool = setup().await;
        insert_suspended_session(&pool, "s1", Some("cc-123")).await;
        insert_claude_task(&pool, "t1", "s1", "completed").await;

        let updated = reconcile_claude_session_resumable(&pool, "s1")
            .await
            .unwrap();
        assert_eq!(updated, 0);

        let (status,): (String,) =
            sqlx::query_as("SELECT status FROM claude_sessions WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "completed");
    }

    #[tokio::test]
    async fn reconcile_claude_session_resumable_noop_without_linked_task() {
        let pool = setup().await;
        insert_suspended_session(&pool, "s1", Some("cc-123")).await;

        let updated = reconcile_claude_session_resumable(&pool, "s1")
            .await
            .unwrap();
        assert_eq!(updated, 0);
    }

    #[tokio::test]
    async fn get_agent_session_ref_returns_pair_when_both_set() {
        let pool = setup().await;
        insert_session(&pool, "s1", "h1", None, None, None)
            .await
            .unwrap();
        set_agent_session_ref(&pool, "s1", "claude", "cc-123", "2026-06-04T10:00:00Z")
            .await
            .unwrap();

        let got = get_agent_session_ref(&pool, "s1").await.unwrap();
        assert_eq!(got, Some(("claude".to_string(), "cc-123".to_string())));
    }

    #[tokio::test]
    async fn get_agent_session_ref_none_when_unset() {
        let pool = setup().await;
        insert_session(&pool, "s1", "h1", None, None, None)
            .await
            .unwrap();
        // Never captured -> both columns null -> None.
        assert_eq!(get_agent_session_ref(&pool, "s1").await.unwrap(), None);
        // Missing row -> None.
        assert_eq!(
            get_agent_session_ref(&pool, "nonexistent").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn mark_session_active_transitions_resumable_and_clears_timestamps() {
        let pool = setup().await;
        insert_session(&pool, "s1", "h1", None, None, None)
            .await
            .unwrap();
        // Put it in resumable with stale suspended/closed timestamps.
        sqlx::query(
            "UPDATE sessions SET status = 'resumable', suspended_at = '2026-06-04T08:00:00Z', \
             closed_at = '2026-06-04T08:30:00Z' WHERE id = 's1'",
        )
        .execute(&pool)
        .await
        .unwrap();

        mark_session_active(&pool, "s1").await.unwrap();

        let (status, suspended_at, closed_at): (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT status, suspended_at, closed_at FROM sessions WHERE id = 's1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "active");
        assert_eq!(suspended_at, None);
        assert_eq!(closed_at, None);
    }
}
