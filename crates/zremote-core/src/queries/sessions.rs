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
    let project_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM projects WHERE host_id = ? AND (? = path OR ? LIKE path || '/%') LIMIT 1",
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
    sqlx::query("UPDATE sessions SET status = 'closed', closed_at = ? WHERE id = ?")
        .bind(&now)
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
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
