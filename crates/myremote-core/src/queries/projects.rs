use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;

/// Project representation for API responses.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProjectRow {
    pub id: String,
    pub host_id: String,
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
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
}

const PROJECT_COLUMNS: &str = "id, host_id, path, name, has_claude_config, project_type, created_at, \
     parent_project_id, git_branch, git_commit_hash, git_commit_message, \
     git_is_dirty, git_ahead, git_behind, git_remotes, git_updated_at";

pub async fn list_projects(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Vec<ProjectRow>, AppError> {
    let projects: Vec<ProjectRow> = sqlx::query_as(
        &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE host_id = ? ORDER BY name"),
    )
    .bind(host_id)
    .fetch_all(pool)
    .await?;
    Ok(projects)
}

pub async fn get_project(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<ProjectRow, AppError> {
    let project: ProjectRow = sqlx::query_as(
        &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?"),
    )
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
    let project: ProjectRow = sqlx::query_as(
        &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE host_id = ? AND path = ?"),
    )
    .bind(host_id)
    .bind(path)
    .fetch_one(pool)
    .await?;
    Ok(project)
}

pub async fn insert_project(
    pool: &SqlitePool,
    project_id: &str,
    host_id: &str,
    path: &str,
    name: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT OR IGNORE INTO projects (id, host_id, path, name) VALUES (?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(host_id)
    .bind(path)
    .bind(name)
    .execute(pool)
    .await?;
    Ok(())
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
    let worktrees: Vec<ProjectRow> = sqlx::query_as(
        &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE parent_project_id = ? ORDER BY name"),
    )
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
