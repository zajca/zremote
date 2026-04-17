//! Periodic git refresh loop.
//!
//! Runs `GitInspector::inspect_fast` every [`REFRESH_INTERVAL`] on **all
//! registered top-level projects and linked worktrees for the local host**,
//! updates the git columns in SQLite when they changed, and broadcasts
//! [`ServerEvent::ProjectsUpdated`] when any row actually moved. This keeps
//! the sidebar's dirty / ahead / behind / branch badges fresh between full
//! filesystem scans.
//!
//! An earlier RFC draft mentioned refreshing only "visible/expanded"
//! projects but that required signalling the GUI's expanded set back to
//! the agent. The cost of refreshing everything on a 30 s cadence,
//! bounded by [`REFRESH_ROW_LIMIT`], is low enough in practice that the
//! extra plumbing was deferred — see RFC-007 Phase 1 item 5.

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use zremote_core::state::ServerEvent;

use super::git::GitInspector;

/// How often to refresh git metadata for visible projects.
pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Spawn the periodic git refresh task and return its `JoinHandle`.
///
/// The caller is expected to keep the handle alive for the lifetime of the
/// agent (store it on `LocalAppState`) and to trigger `shutdown.cancel()`
/// when it wants the task to exit. The task will exit within one poll cycle
/// after cancellation.
pub fn spawn_git_refresh_loop(
    db: SqlitePool,
    host_id: String,
    events: broadcast::Sender<ServerEvent>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);
        // If a cycle runs long (slow disk, many repos), don't fire catch-up
        // ticks back-to-back — wait a full interval after the slow cycle.
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // Skip the immediate tick; the first real refresh happens after
        // REFRESH_INTERVAL so startup isn't hammered.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match run_refresh_cycle(&db, &host_id).await {
                        Ok(changed) => {
                            if changed && events
                                .send(ServerEvent::ProjectsUpdated { host_id: host_id.clone() })
                                .is_err()
                            {
                                // No subscribers; fine in local mode before GUI attaches.
                                tracing::trace!("git refresh: no event subscribers");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "git refresh cycle failed");
                        }
                    }
                }
                () = shutdown.cancelled() => {
                    tracing::debug!("git refresh loop shutting down");
                    break;
                }
            }
        }
    })
}

/// Run a single refresh pass over every project for `host_id`.
///
/// Returns `true` when at least one row changed and the caller should emit
/// `ProjectsUpdated`. Errors returned from this function are database-level;
/// per-project git failures are logged and skipped so one broken repo does
/// not stop the whole cycle.
/// Hard cap on the number of projects inspected per refresh cycle. Keeps
/// the `SELECT` result and per-row spawn_blocking traffic bounded on hosts
/// that have registered an unreasonable number of repos. Rows beyond the
/// cap are simply skipped for this cycle; the next cycle resolves in the
/// same order (by path) so the same prefix is refreshed — acceptable for
/// the 30 s cadence and fails safe (no crash, no OOM).
const REFRESH_ROW_LIMIT: i64 = 1000;

async fn run_refresh_cycle(db: &SqlitePool, host_id: &str) -> Result<bool, sqlx::Error> {
    // Columns we care about: id + path + current git fields. We only touch
    // top-level projects (no parent) and linked worktrees (project_type
    // 'worktree'); archived / non-git rows are filtered below via path
    // existence and the `inspect_fast` None path. The LIMIT caps peak
    // memory use and per-cycle wall time.
    let rows: Vec<ProjectRefreshRow> = sqlx::query_as(
        "SELECT id, path, git_branch, git_is_dirty, git_ahead, git_behind \
         FROM projects \
         WHERE host_id = ? \
           AND (parent_project_id IS NULL OR project_type = 'worktree') \
         ORDER BY path \
         LIMIT ?",
    )
    .bind(host_id)
    .bind(REFRESH_ROW_LIMIT)
    .fetch_all(db)
    .await?;

    if i64::try_from(rows.len()).unwrap_or(i64::MAX) >= REFRESH_ROW_LIMIT {
        tracing::warn!(
            limit = REFRESH_ROW_LIMIT,
            host_id = %host_id,
            "git refresh hit row limit; excess projects skipped this cycle",
        );
    }

    let mut any_changed = false;

    for row in rows {
        // Canonicalize before we touch the filesystem or hand the path to
        // git. A stale row that points at `~/old/../old` or a symlink that
        // has moved needs the resolved path; a path that no longer exists
        // returns `Err` here and is skipped, matching the intent of the
        // is_dir guard below.
        let Ok(path_buf) = std::fs::canonicalize(&row.path) else {
            tracing::trace!(
                project_id = %row.id,
                path = %row.path,
                "git refresh: canonicalize failed, skipping",
            );
            continue;
        };
        if !path_buf.is_dir() {
            tracing::trace!(project_id = %row.id, path = %row.path, "git refresh: path missing, skipping");
            continue;
        }

        // Blocking git subprocess — keep off the async runtime.
        let path_for_task = path_buf.clone();
        let inspect = match tokio::task::spawn_blocking(move || {
            GitInspector::inspect_fast(&path_for_task)
        })
        .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::warn!(project_id = %row.id, error = %e, "git refresh: blocking task join failed");
                continue;
            }
        };

        let Some(info) = inspect else {
            // Not a git repo (or git unavailable) — leave the row untouched.
            continue;
        };

        let new_branch = info.branch.as_deref();
        let new_dirty = info.is_dirty;
        let new_ahead = i32::try_from(info.ahead).unwrap_or(i32::MAX);
        let new_behind = i32::try_from(info.behind).unwrap_or(i32::MAX);

        let unchanged = row.git_branch.as_deref() == new_branch
            && row.git_is_dirty == new_dirty
            && row.git_ahead == new_ahead
            && row.git_behind == new_behind;

        if unchanged {
            continue;
        }

        let now = chrono::Utc::now().to_rfc3339();
        match sqlx::query(
            "UPDATE projects SET \
             git_branch = ?, git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_updated_at = ? \
             WHERE id = ?",
        )
        .bind(new_branch)
        .bind(new_dirty)
        .bind(new_ahead)
        .bind(new_behind)
        .bind(&now)
        .bind(&row.id)
        .execute(db)
        .await
        {
            Ok(result) if result.rows_affected() > 0 => {
                any_changed = true;
            }
            Ok(_) => {
                // Row vanished between SELECT and UPDATE; ignore.
            }
            Err(e) => {
                tracing::warn!(project_id = %row.id, error = %e, "git refresh UPDATE failed");
            }
        }
    }

    Ok(any_changed)
}

#[derive(sqlx::FromRow)]
struct ProjectRefreshRow {
    id: String,
    path: String,
    git_branch: Option<String>,
    git_is_dirty: bool,
    git_ahead: i32,
    git_behind: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;
    use tokio::time::{Duration, timeout};

    fn run_git(path: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(path)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CEILING_DIRECTORIES", path)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(path: &Path) {
        run_git(path, &["init"]);
        run_git(path, &["config", "user.email", "t@t.com"]);
        run_git(path, &["config", "user.name", "t"]);
        run_git(path, &["config", "commit.gpgsign", "false"]);
        std::fs::write(path.join("README.md"), "x").unwrap();
        run_git(path, &["add", "."]);
        run_git(path, &["commit", "--no-verify", "-m", "init"]);
    }

    async fn setup_db() -> SqlitePool {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('h1', 'test', 'test-host', 'hash', 'online')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn insert_project(pool: &SqlitePool, id: &str, path: &str) {
        sqlx::query("INSERT INTO projects (id, host_id, path, name) VALUES (?, 'h1', ?, ?)")
            .bind(id)
            .bind(path)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn refresh_cycle_skips_rows_without_git() {
        let pool = setup_db().await;
        let tmp = TempDir::new().unwrap();
        insert_project(&pool, "p1", tmp.path().to_str().unwrap()).await;

        let changed = run_refresh_cycle(&pool, "h1").await.unwrap();
        assert!(!changed, "non-git dir must not trigger an UPDATE");
    }

    #[tokio::test]
    async fn refresh_cycle_updates_dirty_flag_then_dedups() {
        let pool = setup_db().await;
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        insert_project(&pool, "p1", tmp.path().to_str().unwrap()).await;

        // First cycle must report a change (row was unset / default).
        let changed = run_refresh_cycle(&pool, "h1").await.unwrap();
        assert!(changed, "first refresh must write initial git fields");

        // Second cycle must be a no-op since nothing changed on disk.
        let changed_again = run_refresh_cycle(&pool, "h1").await.unwrap();
        assert!(
            !changed_again,
            "second refresh with no disk change must not UPDATE"
        );

        // Make the tree dirty; next cycle must report a change.
        std::fs::write(tmp.path().join("dirty.txt"), "x").unwrap();
        let changed_dirty = run_refresh_cycle(&pool, "h1").await.unwrap();
        assert!(changed_dirty, "dirty tree must trigger an UPDATE");

        let (dirty,): (bool,) = sqlx::query_as("SELECT git_is_dirty FROM projects WHERE id = 'p1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(dirty);
    }

    #[tokio::test]
    async fn refresh_cycle_skips_missing_paths() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "/nonexistent/path/zremote-refresh-test").await;

        let changed = run_refresh_cycle(&pool, "h1").await.unwrap();
        assert!(!changed);
    }

    #[tokio::test]
    async fn refresh_cycle_skips_archived_children() {
        let pool = setup_db().await;
        let tmp = TempDir::new().unwrap();
        let parent_dir = tmp.path().join("parent");
        std::fs::create_dir_all(&parent_dir).unwrap();
        let child_dir = tmp.path().join("child");
        std::fs::create_dir_all(&child_dir).unwrap();
        init_repo(&child_dir);

        // Parent row (no git, no change expected).
        insert_project(&pool, "p1", parent_dir.to_str().unwrap()).await;

        // Child row with a non-worktree project_type must be skipped by the
        // SQL filter even if it points at a real repo.
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, parent_project_id, project_type) \
             VALUES ('child', 'h1', ?, 'child', 'p1', 'archived')",
        )
        .bind(child_dir.to_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();

        let changed = run_refresh_cycle(&pool, "h1").await.unwrap();
        assert!(!changed, "archived child must not be inspected");

        // Verify the archived child row's git fields stayed at defaults.
        let (branch, dirty): (Option<String>, bool) =
            sqlx::query_as("SELECT git_branch, git_is_dirty FROM projects WHERE id = 'child'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(branch.is_none());
        assert!(!dirty);
    }

    #[tokio::test]
    async fn refresh_cycle_includes_linked_worktrees() {
        let pool = setup_db().await;
        let tmp = TempDir::new().unwrap();
        let parent_dir = tmp.path().join("parent");
        std::fs::create_dir_all(&parent_dir).unwrap();
        let wt_dir = tmp.path().join("wt");
        std::fs::create_dir_all(&wt_dir).unwrap();
        init_repo(&wt_dir);

        insert_project(&pool, "p1", parent_dir.to_str().unwrap()).await;
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, parent_project_id, project_type) \
             VALUES ('wt', 'h1', ?, 'wt', 'p1', 'worktree')",
        )
        .bind(wt_dir.to_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();

        let changed = run_refresh_cycle(&pool, "h1").await.unwrap();
        assert!(changed, "linked worktree git fields must be refreshed");

        let branch: Option<String> =
            sqlx::query_scalar("SELECT git_branch FROM projects WHERE id = 'wt'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(branch.is_some(), "worktree branch must be written");
    }

    #[tokio::test]
    async fn spawn_refresh_loop_exits_on_cancellation() {
        let pool = setup_db().await;
        let (events, _rx) = broadcast::channel(8);
        let token = CancellationToken::new();
        let handle = spawn_git_refresh_loop(pool, "h1".to_string(), events, token.clone());

        // Give the loop a moment to install its tick machinery, then cancel.
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();

        let result = timeout(Duration::from_secs(1), handle).await;
        assert!(
            result.is_ok(),
            "refresh loop did not exit within 1s of cancellation"
        );
        result.unwrap().expect("join error");
    }
}
