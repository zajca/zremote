use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use zremote_protocol::ProjectInfo;

use crate::error::AppError;

/// Project representation for API responses.
#[allow(clippy::struct_excessive_bools)] // DB row maps booleans directly from SQLite columns
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProjectRow {
    pub id: String,
    pub host_id: String,
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
    #[serde(default)]
    pub has_zremote_config: bool,
    pub project_type: String,
    pub created_at: String,
    pub parent_project_id: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit_hash: Option<String>,
    pub git_commit_message: Option<String>,
    #[serde(default)]
    pub git_is_dirty: bool,
    #[serde(default)]
    pub git_ahead: i32,
    #[serde(default)]
    pub git_behind: i32,
    pub git_remotes: Option<String>,
    pub git_updated_at: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub frameworks: Option<String>,
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub conventions: Option<String>,
    #[serde(default)]
    pub package_manager: Option<String>,
}

const PROJECT_COLUMNS: &str = "id, host_id, path, name, has_claude_config, has_zremote_config, project_type, created_at, \
     parent_project_id, git_branch, git_commit_hash, git_commit_message, \
     git_is_dirty, git_ahead, git_behind, git_remotes, git_updated_at, pinned, \
     frameworks, architecture, conventions, package_manager";

pub async fn list_projects(pool: &SqlitePool, host_id: &str) -> Result<Vec<ProjectRow>, AppError> {
    let projects: Vec<ProjectRow> = sqlx::query_as(&format!(
        "SELECT {PROJECT_COLUMNS} FROM projects WHERE host_id = ? ORDER BY pinned DESC, name"
    ))
    .bind(host_id)
    .fetch_all(pool)
    .await?;
    Ok(projects)
}

pub async fn get_project(pool: &SqlitePool, project_id: &str) -> Result<ProjectRow, AppError> {
    let project: ProjectRow = sqlx::query_as(&format!(
        "SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?"
    ))
    .bind(project_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;
    Ok(project)
}

pub async fn get_project_by_host_and_path(
    pool: &SqlitePool,
    host_id: &str,
    path: &str,
) -> Result<ProjectRow, AppError> {
    let project: ProjectRow = sqlx::query_as(&format!(
        "SELECT {PROJECT_COLUMNS} FROM projects WHERE host_id = ? AND path = ?"
    ))
    .bind(host_id)
    .bind(path)
    .fetch_one(pool)
    .await?;
    Ok(project)
}

/// Insert a project. Returns `true` if the row was inserted, `false` if it was a duplicate.
pub async fn insert_project(
    pool: &SqlitePool,
    project_id: &str,
    host_id: &str,
    path: &str,
    name: &str,
) -> Result<bool, AppError> {
    let result =
        sqlx::query("INSERT OR IGNORE INTO projects (id, host_id, path, name) VALUES (?, ?, ?, ?)")
            .bind(project_id)
            .bind(host_id)
            .bind(path)
            .bind(name)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

/// Insert a project with optional `parent_project_id` and `project_type`.
/// Returns true if a new row was inserted, false on duplicate path.
pub async fn insert_project_with_parent(
    pool: &SqlitePool,
    project_id: &str,
    host_id: &str,
    path: &str,
    name: &str,
    parent_project_id: Option<&str>,
    project_type: &str,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        "INSERT OR IGNORE INTO projects (id, host_id, path, name, parent_project_id, project_type) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(host_id)
    .bind(path)
    .bind(name)
    .bind(parent_project_id)
    .bind(project_type)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Update an existing project row's `parent_project_id` and `project_type`.
/// Used by the orphaned-worktree repair step.
pub async fn set_parent_project_id(
    pool: &SqlitePool,
    project_id: &str,
    parent_project_id: &str,
    project_type: &str,
) -> Result<u64, AppError> {
    let result =
        sqlx::query("UPDATE projects SET parent_project_id = ?, project_type = ? WHERE id = ?")
            .bind(parent_project_id)
            .bind(project_type)
            .bind(project_id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

pub async fn get_project_host_and_path(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Option<(String, String)>, AppError> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT host_id, path FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

pub async fn delete_project(pool: &SqlitePool, project_id: &str) -> Result<u64, AppError> {
    let result = sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn list_worktrees(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<ProjectRow>, AppError> {
    let worktrees: Vec<ProjectRow> = sqlx::query_as(&format!(
        "SELECT {PROJECT_COLUMNS} FROM projects WHERE parent_project_id = ? ORDER BY pinned DESC, name"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(worktrees)
}

pub async fn get_worktree_path(
    pool: &SqlitePool,
    worktree_id: &str,
    parent_project_id: &str,
) -> Result<Option<String>, AppError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT path FROM projects WHERE id = ? AND parent_project_id = ?")
            .bind(worktree_id)
            .bind(parent_project_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(p,)| p))
}

pub async fn get_project_info(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<(String, String, String), AppError> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT id, host_id, path FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    row.ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))
}

pub async fn set_project_pinned(
    pool: &SqlitePool,
    project_id: &str,
    pinned: bool,
) -> Result<u64, AppError> {
    let result = sqlx::query("UPDATE projects SET pinned = ? WHERE id = ?")
        .bind(pinned)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Update a project row with detected metadata from `ProjectInfo`.
/// Shared by agent (manual registration, scan, backfill) and server
/// (agent `ProjectList` dispatch) so the metadata UPDATE SQL lives in exactly
/// one place. Preserves the "worktree" `project_type` marker set at insert time —
/// it encodes structural role, not language, and must not be clobbered by the
/// detected language type (e.g. "rust" from a Cargo.toml inside the worktree).
pub async fn update_project_metadata_from_info(
    pool: &SqlitePool,
    project_id: &str,
    info: &ProjectInfo,
) -> Result<(), AppError> {
    // Fall back to an empty JSON array rather than an empty string when
    // serialization fails — downstream readers parse these columns as JSON
    // and an empty string is not valid JSON.
    let remotes_json = info
        .git_info
        .as_ref()
        .map(|g| serde_json::to_string(&g.remotes).unwrap_or_else(|_| "[]".to_string()));
    let now = chrono::Utc::now().to_rfc3339();
    let frameworks_json =
        serde_json::to_string(&info.frameworks).unwrap_or_else(|_| "[]".to_string());
    let architecture_str = info
        .architecture
        .as_ref()
        .and_then(|a| serde_json::to_value(a).ok())
        .and_then(|v| v.as_str().map(String::from));
    let conventions_json =
        serde_json::to_string(&info.conventions).unwrap_or_else(|_| "[]".to_string());

    sqlx::query(
        "UPDATE projects SET \
         project_type = CASE WHEN project_type = 'worktree' THEN project_type ELSE ? END, \
         has_claude_config = ?, has_zremote_config = ?, \
         git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
         git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_remotes = ?, git_updated_at = ?, \
         frameworks = ?, architecture = ?, conventions = ?, package_manager = ? \
         WHERE id = ?",
    )
    .bind(&info.project_type)
    .bind(info.has_claude_config)
    .bind(info.has_zremote_config)
    .bind(info.git_info.as_ref().and_then(|g| g.branch.as_deref()))
    .bind(
        info.git_info
            .as_ref()
            .and_then(|g| g.commit_hash.as_deref()),
    )
    .bind(
        info.git_info
            .as_ref()
            .and_then(|g| g.commit_message.as_deref()),
    )
    .bind(info.git_info.as_ref().is_some_and(|g| g.is_dirty))
    .bind(info.git_info.as_ref().map_or(0, |g| g.ahead))
    .bind(info.git_info.as_ref().map_or(0, |g| g.behind))
    .bind(&remotes_json)
    .bind(&now)
    .bind(&frameworks_json)
    .bind(&architecture_str)
    .bind(&conventions_json)
    .bind(&info.package_manager)
    .bind(project_id)
    .execute(pool)
    .await?;

    Ok(())
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

        pool
    }

    async fn insert_project(pool: &SqlitePool, id: &str, host_id: &str, path: &str, name: &str) {
        super::insert_project(pool, id, host_id, path, name)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_projects_empty() {
        let pool = setup_db().await;
        let projects = list_projects(&pool, "h1").await.unwrap();
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn list_projects_returns_all_for_host() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj-a", "proj-a").await;
        insert_project(&pool, "p2", "h1", "/home/user/proj-b", "proj-b").await;

        let projects = list_projects(&pool, "h1").await.unwrap();
        assert_eq!(projects.len(), 2);
        // Ordered by name
        assert_eq!(projects[0].name, "proj-a");
        assert_eq!(projects[1].name, "proj-b");
    }

    #[tokio::test]
    async fn get_project_found() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let project = get_project(&pool, "p1").await.unwrap();
        assert_eq!(project.id, "p1");
        assert_eq!(project.path, "/home/user/proj");
    }

    #[tokio::test]
    async fn get_project_not_found() {
        let pool = setup_db().await;
        let result = get_project(&pool, "nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_project_by_host_and_path_found() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let project = get_project_by_host_and_path(&pool, "h1", "/home/user/proj")
            .await
            .unwrap();
        assert_eq!(project.id, "p1");
    }

    #[tokio::test]
    async fn get_project_by_host_and_path_not_found() {
        let pool = setup_db().await;
        let result = get_project_by_host_and_path(&pool, "h1", "/nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_project_host_and_path_found() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let result = get_project_host_and_path(&pool, "p1").await.unwrap();
        assert!(result.is_some());
        let (host_id, path) = result.unwrap();
        assert_eq!(host_id, "h1");
        assert_eq!(path, "/home/user/proj");
    }

    #[tokio::test]
    async fn get_project_host_and_path_not_found() {
        let pool = setup_db().await;
        let result = get_project_host_and_path(&pool, "nonexistent")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn delete_project_removes_row() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let affected = delete_project(&pool, "p1").await.unwrap();
        assert_eq!(affected, 1);

        let projects = list_projects(&pool, "h1").await.unwrap();
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn delete_project_nonexistent_returns_zero() {
        let pool = setup_db().await;
        let affected = delete_project(&pool, "nonexistent").await.unwrap();
        assert_eq!(affected, 0);
    }

    #[tokio::test]
    async fn list_worktrees_empty() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let worktrees = list_worktrees(&pool, "p1").await.unwrap();
        assert!(worktrees.is_empty());
    }

    #[tokio::test]
    async fn list_worktrees_returns_children() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        // Insert worktree as child project
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, parent_project_id) VALUES ('wt1', 'h1', '/home/user/proj-wt', 'proj-wt', 'p1')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let worktrees = list_worktrees(&pool, "p1").await.unwrap();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].id, "wt1");
        assert_eq!(worktrees[0].parent_project_id, Some("p1".to_string()));
    }

    #[tokio::test]
    async fn get_worktree_path_found() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, parent_project_id) VALUES ('wt1', 'h1', '/home/user/proj-wt', 'proj-wt', 'p1')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let path = get_worktree_path(&pool, "wt1", "p1").await.unwrap();
        assert_eq!(path, Some("/home/user/proj-wt".to_string()));
    }

    #[tokio::test]
    async fn get_worktree_path_not_found() {
        let pool = setup_db().await;
        let path = get_worktree_path(&pool, "nonexistent", "p1").await.unwrap();
        assert!(path.is_none());
    }

    #[tokio::test]
    async fn get_worktree_path_wrong_parent() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, parent_project_id) VALUES ('wt1', 'h1', '/home/user/proj-wt', 'proj-wt', 'p1')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Query with wrong parent ID
        let path = get_worktree_path(&pool, "wt1", "wrong-parent")
            .await
            .unwrap();
        assert!(path.is_none());
    }

    #[tokio::test]
    async fn get_project_info_found() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let (id, host_id, path) = get_project_info(&pool, "p1").await.unwrap();
        assert_eq!(id, "p1");
        assert_eq!(host_id, "h1");
        assert_eq!(path, "/home/user/proj");
    }

    #[tokio::test]
    async fn get_project_info_not_found() {
        let pool = setup_db().await;
        let result = get_project_info(&pool, "nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn insert_project_ignores_duplicate() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;
        // Inserting again with same ID should not fail (INSERT OR IGNORE)
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let projects = list_projects(&pool, "h1").await.unwrap();
        assert_eq!(projects.len(), 1);
    }

    #[tokio::test]
    async fn pinned_defaults_to_false() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;
        let project = get_project(&pool, "p1").await.unwrap();
        assert!(!project.pinned);
    }

    #[tokio::test]
    async fn set_project_pinned_updates_flag() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let affected = set_project_pinned(&pool, "p1", true).await.unwrap();
        assert_eq!(affected, 1);

        let project = get_project(&pool, "p1").await.unwrap();
        assert!(project.pinned);

        // Unpin
        let affected = set_project_pinned(&pool, "p1", false).await.unwrap();
        assert_eq!(affected, 1);
        let project = get_project(&pool, "p1").await.unwrap();
        assert!(!project.pinned);
    }

    #[tokio::test]
    async fn set_project_pinned_nonexistent_returns_zero() {
        let pool = setup_db().await;
        let affected = set_project_pinned(&pool, "nonexistent", true)
            .await
            .unwrap();
        assert_eq!(affected, 0);
    }

    #[tokio::test]
    async fn project_intelligence_columns_default() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let project = get_project(&pool, "p1").await.unwrap();
        // Default from migration: frameworks = '[]', architecture = NULL, conventions = '[]', package_manager = NULL
        assert_eq!(project.frameworks.as_deref(), Some("[]"));
        assert!(project.architecture.is_none());
        assert_eq!(project.conventions.as_deref(), Some("[]"));
        assert!(project.package_manager.is_none());
    }

    #[tokio::test]
    async fn project_intelligence_columns_persist() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        // Update intelligence columns
        sqlx::query(
            "UPDATE projects SET frameworks = ?, architecture = ?, conventions = ?, package_manager = ? WHERE id = ?",
        )
        .bind(r#"["nextjs","react"]"#)
        .bind("mvc")
        .bind(r#"[{"kind":"testing","value":"jest"}]"#)
        .bind("npm")
        .bind("p1")
        .execute(&pool)
        .await
        .unwrap();

        let project = get_project(&pool, "p1").await.unwrap();
        assert_eq!(project.frameworks.as_deref(), Some(r#"["nextjs","react"]"#));
        assert_eq!(project.architecture.as_deref(), Some("mvc"));
        assert_eq!(
            project.conventions.as_deref(),
            Some(r#"[{"kind":"testing","value":"jest"}]"#)
        );
        assert_eq!(project.package_manager.as_deref(), Some("npm"));
    }

    #[tokio::test]
    async fn insert_project_with_parent_sets_fields() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;

        let inserted = super::insert_project_with_parent(
            &pool,
            "wt1",
            "h1",
            "/home/user/proj-wt",
            "proj-wt",
            Some("p1"),
            "worktree",
        )
        .await
        .unwrap();
        assert!(inserted);

        let project = get_project(&pool, "wt1").await.unwrap();
        assert_eq!(project.parent_project_id.as_deref(), Some("p1"));
        assert_eq!(project.project_type, "worktree");
    }

    #[tokio::test]
    async fn insert_project_with_parent_none_parent_is_top_level() {
        let pool = setup_db().await;

        let inserted = super::insert_project_with_parent(
            &pool,
            "p1",
            "h1",
            "/home/user/proj",
            "proj",
            None,
            "rust",
        )
        .await
        .unwrap();
        assert!(inserted);

        let project = get_project(&pool, "p1").await.unwrap();
        assert!(project.parent_project_id.is_none());
        assert_eq!(project.project_type, "rust");
    }

    #[tokio::test]
    async fn insert_project_with_parent_ignores_duplicate_path() {
        let pool = setup_db().await;

        let first = super::insert_project_with_parent(
            &pool,
            "p1",
            "h1",
            "/home/user/proj",
            "proj",
            None,
            "rust",
        )
        .await
        .unwrap();
        assert!(first);

        let second = super::insert_project_with_parent(
            &pool,
            "p2",
            "h1",
            "/home/user/proj",
            "proj-dup",
            None,
            "rust",
        )
        .await
        .unwrap();
        assert!(!second);
    }

    #[tokio::test]
    async fn set_parent_project_id_updates_row() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/proj", "proj").await;
        insert_project(&pool, "wt1", "h1", "/home/user/proj-wt", "proj-wt").await;

        let affected = super::set_parent_project_id(&pool, "wt1", "p1", "worktree")
            .await
            .unwrap();
        assert_eq!(affected, 1);

        let project = get_project(&pool, "wt1").await.unwrap();
        assert_eq!(project.parent_project_id.as_deref(), Some("p1"));
        assert_eq!(project.project_type, "worktree");
    }

    #[tokio::test]
    async fn set_parent_project_id_nonexistent_returns_zero() {
        let pool = setup_db().await;
        let affected = super::set_parent_project_id(&pool, "nonexistent", "p1", "worktree")
            .await
            .unwrap();
        assert_eq!(affected, 0);
    }

    #[tokio::test]
    async fn list_projects_pinned_first() {
        let pool = setup_db().await;
        insert_project(&pool, "p1", "h1", "/home/user/alpha", "alpha").await;
        insert_project(&pool, "p2", "h1", "/home/user/beta", "beta").await;
        insert_project(&pool, "p3", "h1", "/home/user/gamma", "gamma").await;

        // Pin "gamma" which alphabetically comes last
        set_project_pinned(&pool, "p3", true).await.unwrap();

        let projects = list_projects(&pool, "h1").await.unwrap();
        assert_eq!(projects.len(), 3);
        assert_eq!(projects[0].name, "gamma"); // pinned, comes first
        assert!(projects[0].pinned);
        assert_eq!(projects[1].name, "alpha");
        assert!(!projects[1].pinned);
    }
}
