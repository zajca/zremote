use serde::Serialize;
use sqlx::SqlitePool;

use crate::error::AppError;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ClaudeTaskRow {
    pub id: String,
    pub session_id: String,
    pub host_id: String,
    pub project_path: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub claude_session_id: Option<String>,
    pub resume_from: Option<String>,
    pub status: String,
    pub options_json: Option<String>,
    pub loop_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub total_tokens_in: Option<i64>,
    pub total_tokens_out: Option<i64>,
    pub summary: Option<String>,
    pub created_at: String,
}

const TASK_COLUMNS: &str = "id, session_id, host_id, project_path, project_id, model, initial_prompt, \
     claude_session_id, resume_from, status, options_json, loop_id, started_at, ended_at, \
     total_cost_usd, total_tokens_in, total_tokens_out, summary, created_at";

pub struct ListClaudeTasksFilter {
    pub host_id: Option<String>,
    pub status: Option<String>,
    pub project_id: Option<String>,
}

pub async fn list_claude_tasks(
    pool: &SqlitePool,
    filter: &ListClaudeTasksFilter,
) -> Result<Vec<ClaudeTaskRow>, AppError> {
    let mut sql = format!(
        "SELECT {TASK_COLUMNS} FROM claude_sessions WHERE 1=1",
    );
    let mut binds: Vec<String> = Vec::new();

    if let Some(ref host_id) = filter.host_id {
        sql.push_str(" AND host_id = ?");
        binds.push(host_id.clone());
    }
    if let Some(ref status) = filter.status {
        sql.push_str(" AND status = ?");
        binds.push(status.clone());
    }
    if let Some(ref project_id) = filter.project_id {
        sql.push_str(" AND project_id = ?");
        binds.push(project_id.clone());
    }

    sql.push_str(" ORDER BY created_at DESC");

    let mut q = sqlx::query_as::<_, ClaudeTaskRow>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }

    let tasks: Vec<ClaudeTaskRow> = q.fetch_all(pool).await?;
    Ok(tasks)
}

pub async fn get_claude_task(
    pool: &SqlitePool,
    task_id: &str,
) -> Result<ClaudeTaskRow, AppError> {
    let task: ClaudeTaskRow = sqlx::query_as(
        &format!("SELECT {TASK_COLUMNS} FROM claude_sessions WHERE id = ?"),
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("claude task {task_id} not found")))?;
    Ok(task)
}

pub async fn resolve_project_id_by_path(
    pool: &SqlitePool,
    host_id: &str,
    project_path: &str,
) -> Result<Option<String>, AppError> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM projects WHERE host_id = ? AND path = ? LIMIT 1",
    )
    .bind(host_id)
    .bind(project_path)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

pub async fn insert_session_for_task(
    pool: &SqlitePool,
    session_id: &str,
    host_id: &str,
    working_dir: &str,
    project_id: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sessions (id, host_id, status, working_dir, project_id) VALUES (?, ?, 'creating', ?, ?)",
    )
    .bind(session_id)
    .bind(host_id)
    .bind(working_dir)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_claude_task(
    pool: &SqlitePool,
    id: &str,
    session_id: &str,
    host_id: &str,
    project_path: &str,
    project_id: Option<&str>,
    model: Option<&str>,
    initial_prompt: Option<&str>,
    options_json: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO claude_sessions (id, session_id, host_id, project_path, project_id, model, initial_prompt, status, options_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'starting', ?)",
    )
    .bind(id)
    .bind(session_id)
    .bind(host_id)
    .bind(project_path)
    .bind(project_id)
    .bind(model)
    .bind(initial_prompt)
    .bind(options_json)
    .execute(pool)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_resumed_claude_task(
    pool: &SqlitePool,
    id: &str,
    session_id: &str,
    host_id: &str,
    project_path: &str,
    project_id: Option<&str>,
    model: Option<&str>,
    initial_prompt: Option<&str>,
    cc_session_id: Option<&str>,
    resume_from: &str,
    options_json: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO claude_sessions (id, session_id, host_id, project_path, project_id, model, initial_prompt, \
         claude_session_id, resume_from, status, options_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'starting', ?)",
    )
    .bind(id)
    .bind(session_id)
    .bind(host_id)
    .bind(project_path)
    .bind(project_id)
    .bind(model)
    .bind(initial_prompt)
    .bind(cc_session_id)
    .bind(resume_from)
    .bind(options_json)
    .execute(pool)
    .await?;
    Ok(())
}
