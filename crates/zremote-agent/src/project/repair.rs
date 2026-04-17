use std::path::Path;

use sqlx::SqlitePool;
use zremote_core::error::AppError;
use zremote_core::queries::projects as q;

use super::git::main_repo_path_for_worktree;

/// One-time idempotent repair at agent startup: find projects in the DB that
/// are actually linked git worktrees but lack `parent_project_id`, and
/// re-link them to their main repository's project row if that row exists.
///
/// Skips rows whose path does not exist on disk, is not a linked worktree, or
/// whose main repo is not yet registered in the DB. Logs a summary.
pub async fn repair_orphaned_worktrees(db: &SqlitePool) -> Result<(), AppError> {
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, host_id, path FROM projects WHERE parent_project_id IS NULL")
            .fetch_all(db)
            .await?;

    let total = rows.len();
    let mut fixed: u32 = 0;

    // Inspect all paths on a blocking thread so we don't stall the async
    // runtime with `std::fs` / `git rev-parse` for each orphan row.
    let inspected: Vec<(String, String, Option<String>)> = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|(id, host_id, path)| {
                let p = Path::new(&path);
                let main = if p.join(".git").is_file() {
                    main_repo_path_for_worktree(p).and_then(|mp| mp.to_str().map(String::from))
                } else {
                    None
                };
                (id, host_id, main)
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Internal(format!("repair inspection task failed: {e}")))?;

    for (id, host_id, main_path_opt) in inspected {
        let Some(main_path_str) = main_path_opt else {
            continue;
        };
        match q::get_project_by_host_and_path(db, &host_id, &main_path_str).await {
            Ok(parent) => {
                if parent.id == id {
                    continue;
                }
                match q::set_parent_project_id(db, &id, &parent.id, "worktree").await {
                    Ok(affected) if affected > 0 => fixed += 1,
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, project_id = %id, "failed to link worktree to parent");
                    }
                }
            }
            Err(AppError::Database(sqlx::Error::RowNotFound)) => continue,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    project_id = %id,
                    main_repo_path = %main_path_str,
                    "transient error looking up main repo during repair",
                );
            }
        }
    }

    tracing::info!(fixed, total, "repair_orphaned_worktrees: completed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempfile::TempDir;

    /// Run a git command directly (mirror of `project::git::run_git` test helper),
    /// used by the test setup to build a real main repo + linked worktree.
    fn run_git(path: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CEILING_DIRECTORIES", path)
            .output()
            .expect("git command");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_git_repo(dir: &Path) {
        run_git(dir, &["init"]);
        run_git(dir, &["config", "user.email", "test@test.com"]);
        run_git(dir, &["config", "user.name", "Test"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        fs::write(dir.join("README.md"), "# Test").expect("write README");
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "--no-verify", "-m", "initial"]);
    }

    async fn setup_pool_with_host() -> SqlitePool {
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

    /// Build a tempdir with `main/` repo and a linked worktree `wt/`.
    /// Returns (tmp guard, canonical main path, canonical worktree path).
    fn build_main_and_worktree() -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        init_git_repo(&main);

        let wt = tmp.path().join("wt");
        run_git(
            &main,
            &["worktree", "add", "-b", "feat", wt.to_str().unwrap()],
        );

        let main_canon = fs::canonicalize(&main).unwrap();
        let wt_canon = fs::canonicalize(&wt).unwrap();
        (tmp, main_canon, wt_canon)
    }

    async fn insert_row(pool: &SqlitePool, id: &str, host_id: &str, path: &str, name: &str) {
        q::insert_project(pool, id, host_id, path, name)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn repair_orphaned_worktrees_links_orphan() {
        let pool = setup_pool_with_host().await;
        let (_tmp, main_path, wt_path) = build_main_and_worktree();
        let main_s = main_path.to_string_lossy().to_string();
        let wt_s = wt_path.to_string_lossy().to_string();

        insert_row(&pool, "p_main", "h1", &main_s, "main").await;
        insert_row(&pool, "p_wt", "h1", &wt_s, "wt").await;

        repair_orphaned_worktrees(&pool).await.unwrap();

        let wt_row = q::get_project(&pool, "p_wt").await.unwrap();
        assert_eq!(wt_row.parent_project_id.as_deref(), Some("p_main"));
        assert_eq!(wt_row.project_type, "worktree");
    }

    #[tokio::test]
    async fn repair_orphaned_worktrees_is_idempotent() {
        let pool = setup_pool_with_host().await;
        let (_tmp, main_path, wt_path) = build_main_and_worktree();
        let main_s = main_path.to_string_lossy().to_string();
        let wt_s = wt_path.to_string_lossy().to_string();

        insert_row(&pool, "p_main", "h1", &main_s, "main").await;
        insert_row(&pool, "p_wt", "h1", &wt_s, "wt").await;

        repair_orphaned_worktrees(&pool).await.unwrap();
        // Second call: worktree is no longer in the orphan set (parent_project_id
        // is non-null) so it's a genuine no-op on that row.
        repair_orphaned_worktrees(&pool).await.unwrap();

        let wt_row = q::get_project(&pool, "p_wt").await.unwrap();
        assert_eq!(wt_row.parent_project_id.as_deref(), Some("p_main"));
        assert_eq!(wt_row.project_type, "worktree");
    }

    #[tokio::test]
    async fn repair_orphaned_worktrees_skips_when_main_not_registered() {
        let pool = setup_pool_with_host().await;
        let (_tmp, _main_path, wt_path) = build_main_and_worktree();
        let wt_s = wt_path.to_string_lossy().to_string();

        insert_row(&pool, "p_wt", "h1", &wt_s, "wt").await;

        repair_orphaned_worktrees(&pool).await.unwrap();

        let wt_row = q::get_project(&pool, "p_wt").await.unwrap();
        assert!(wt_row.parent_project_id.is_none());
    }

    #[tokio::test]
    async fn repair_orphaned_worktrees_skips_non_git_paths() {
        let pool = setup_pool_with_host().await;
        let tmp = TempDir::new().unwrap();
        let plain = tmp.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        let plain_s = plain.to_string_lossy().to_string();

        insert_row(&pool, "p_plain", "h1", &plain_s, "plain").await;

        repair_orphaned_worktrees(&pool).await.unwrap();

        let row = q::get_project(&pool, "p_plain").await.unwrap();
        assert!(row.parent_project_id.is_none());
    }
}
