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
    let mut sql = format!("SELECT {TASK_COLUMNS} FROM claude_sessions WHERE 1=1",);
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

pub async fn get_claude_task(pool: &SqlitePool, task_id: &str) -> Result<ClaudeTaskRow, AppError> {
    let task: ClaudeTaskRow = sqlx::query_as(&format!(
        "SELECT {TASK_COLUMNS} FROM claude_sessions WHERE id = ?"
    ))
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
    let id: Option<String> =
        sqlx::query_scalar("SELECT id FROM projects WHERE host_id = ? AND path = ? LIMIT 1")
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> SqlitePool {
        crate::db::init_db("sqlite::memory:").await.unwrap()
    }

    async fn insert_host(pool: &SqlitePool, host_id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) VALUES (?, 'test', 'test-host', 'hash', 'online')",
        )
        .bind(host_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_session(pool: &SqlitePool, session_id: &str, host_id: &str) {
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(session_id)
            .bind(host_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn insert_project(pool: &SqlitePool, project_id: &str, host_id: &str, path: &str) {
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, project_type) VALUES (?, ?, ?, 'test-proj', 'rust')",
        )
        .bind(project_id)
        .bind(host_id)
        .bind(path)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn list_claude_tasks_empty() {
        let pool = test_db().await;
        let filter = ListClaudeTasksFilter {
            host_id: None,
            status: None,
            project_id: None,
        };
        let tasks = list_claude_tasks(&pool, &filter).await.unwrap();
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn insert_and_get_claude_task() {
        let pool = test_db().await;
        let host_id = "host-1";
        let session_id = "sess-1";
        let task_id = "task-1";
        insert_host(&pool, host_id).await;
        insert_session(&pool, session_id, host_id).await;

        insert_claude_task(
            &pool,
            task_id,
            session_id,
            host_id,
            "/home/user/project",
            None,
            Some("sonnet"),
            Some("Fix the bug"),
            None,
        )
        .await
        .unwrap();

        let task = get_claude_task(&pool, task_id).await.unwrap();
        assert_eq!(task.id, task_id);
        assert_eq!(task.session_id, session_id);
        assert_eq!(task.host_id, host_id);
        assert_eq!(task.project_path, "/home/user/project");
        assert_eq!(task.model, Some("sonnet".to_string()));
        assert_eq!(task.initial_prompt, Some("Fix the bug".to_string()));
        assert_eq!(task.status, "starting");
        assert!(task.project_id.is_none());
        assert!(task.options_json.is_none());
    }

    #[tokio::test]
    async fn get_claude_task_not_found() {
        let pool = test_db().await;
        let result = get_claude_task(&pool, "nonexistent").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_claude_tasks_with_host_filter() {
        let pool = test_db().await;
        let host1 = "host-1";
        let host2 = "host-2";
        insert_host(&pool, host1).await;
        insert_host(&pool, host2).await;

        let s1 = "sess-1";
        let s2 = "sess-2";
        insert_session(&pool, s1, host1).await;
        insert_session(&pool, s2, host2).await;

        insert_claude_task(&pool, "t1", s1, host1, "/proj1", None, None, None, None)
            .await
            .unwrap();
        insert_claude_task(&pool, "t2", s2, host2, "/proj2", None, None, None, None)
            .await
            .unwrap();

        let filter = ListClaudeTasksFilter {
            host_id: Some(host1.to_string()),
            status: None,
            project_id: None,
        };
        let tasks = list_claude_tasks(&pool, &filter).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].host_id, host1);
    }

    #[tokio::test]
    async fn list_claude_tasks_with_status_filter() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;

        let s1 = "sess-1";
        let s2 = "sess-2";
        insert_session(&pool, s1, host_id).await;
        insert_session(&pool, s2, host_id).await;

        insert_claude_task(&pool, "t1", s1, host_id, "/proj", None, None, None, None)
            .await
            .unwrap();
        insert_claude_task(&pool, "t2", s2, host_id, "/proj", None, None, None, None)
            .await
            .unwrap();

        // Update one to 'active'
        sqlx::query("UPDATE claude_sessions SET status = 'active' WHERE id = 't1'")
            .execute(&pool)
            .await
            .unwrap();

        let filter = ListClaudeTasksFilter {
            host_id: None,
            status: Some("active".to_string()),
            project_id: None,
        };
        let tasks = list_claude_tasks(&pool, &filter).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t1");
    }

    #[tokio::test]
    async fn list_claude_tasks_with_project_filter() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj1").await;

        let s1 = "sess-1";
        let s2 = "sess-2";
        insert_session(&pool, s1, host_id).await;
        insert_session(&pool, s2, host_id).await;

        insert_claude_task(
            &pool,
            "t1",
            s1,
            host_id,
            "/proj1",
            Some("proj-1"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        insert_claude_task(&pool, "t2", s2, host_id, "/proj2", None, None, None, None)
            .await
            .unwrap();

        let filter = ListClaudeTasksFilter {
            host_id: None,
            status: None,
            project_id: Some("proj-1".to_string()),
        };
        let tasks = list_claude_tasks(&pool, &filter).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t1");
    }

    #[tokio::test]
    async fn list_claude_tasks_ordered_by_created_at_desc() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;

        let s1 = "sess-1";
        let s2 = "sess-2";
        insert_session(&pool, s1, host_id).await;
        insert_session(&pool, s2, host_id).await;

        insert_claude_task(&pool, "t1", s1, host_id, "/proj", None, None, None, None)
            .await
            .unwrap();
        // Manually set created_at to ensure ordering
        sqlx::query(
            "UPDATE claude_sessions SET created_at = '2026-01-01T00:00:00Z' WHERE id = 't1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        insert_claude_task(&pool, "t2", s2, host_id, "/proj", None, None, None, None)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE claude_sessions SET created_at = '2026-01-02T00:00:00Z' WHERE id = 't2'",
        )
        .execute(&pool)
        .await
        .unwrap();

        let filter = ListClaudeTasksFilter {
            host_id: None,
            status: None,
            project_id: None,
        };
        let tasks = list_claude_tasks(&pool, &filter).await.unwrap();
        assert_eq!(tasks.len(), 2);
        // t2 is newer, should come first
        assert_eq!(tasks[0].id, "t2");
        assert_eq!(tasks[1].id, "t1");
    }

    #[tokio::test]
    async fn resolve_project_id_by_path_found() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/home/user/project").await;

        let id = resolve_project_id_by_path(&pool, host_id, "/home/user/project")
            .await
            .unwrap();
        assert_eq!(id, Some("proj-1".to_string()));
    }

    #[tokio::test]
    async fn resolve_project_id_by_path_not_found() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;

        let id = resolve_project_id_by_path(&pool, host_id, "/nonexistent")
            .await
            .unwrap();
        assert!(id.is_none());
    }

    #[tokio::test]
    async fn insert_session_for_task_creates_session() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;

        insert_session_for_task(&pool, "sess-1", host_id, "/home/user/project", None)
            .await
            .unwrap();

        let (status, working_dir): (String, Option<String>) =
            sqlx::query_as("SELECT status, working_dir FROM sessions WHERE id = 'sess-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "creating");
        assert_eq!(working_dir.unwrap(), "/home/user/project");
    }

    #[tokio::test]
    async fn insert_session_for_task_with_project_id() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;

        insert_session_for_task(&pool, "sess-1", host_id, "/proj", Some("proj-1"))
            .await
            .unwrap();

        let (project_id,): (Option<String>,) =
            sqlx::query_as("SELECT project_id FROM sessions WHERE id = 'sess-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(project_id, Some("proj-1".to_string()));
    }

    #[tokio::test]
    async fn insert_claude_task_with_all_fields() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_session(&pool, "sess-1", host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;

        insert_claude_task(
            &pool,
            "task-1",
            "sess-1",
            host_id,
            "/proj",
            Some("proj-1"),
            Some("opus"),
            Some("Refactor the auth module"),
            Some(r#"{"allowedTools":["Read","Edit"]}"#),
        )
        .await
        .unwrap();

        let task = get_claude_task(&pool, "task-1").await.unwrap();
        assert_eq!(task.project_id, Some("proj-1".to_string()));
        assert_eq!(task.model, Some("opus".to_string()));
        assert_eq!(
            task.initial_prompt,
            Some("Refactor the auth module".to_string())
        );
        assert_eq!(
            task.options_json,
            Some(r#"{"allowedTools":["Read","Edit"]}"#.to_string())
        );
        assert_eq!(task.status, "starting");
    }

    #[tokio::test]
    async fn insert_resumed_claude_task_creates_entry() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_session(&pool, "sess-orig", host_id).await;
        insert_session(&pool, "sess-resumed", host_id).await;

        // Original task
        insert_claude_task(
            &pool,
            "task-orig",
            "sess-orig",
            host_id,
            "/proj",
            None,
            None,
            Some("original prompt"),
            None,
        )
        .await
        .unwrap();

        // Resumed task
        insert_resumed_claude_task(
            &pool,
            "task-resumed",
            "sess-resumed",
            host_id,
            "/proj",
            None,
            Some("sonnet"),
            Some("continue"),
            Some("cc-session-123"),
            "task-orig",
            None,
        )
        .await
        .unwrap();

        let task = get_claude_task(&pool, "task-resumed").await.unwrap();
        assert_eq!(task.resume_from, Some("task-orig".to_string()));
        assert_eq!(task.claude_session_id, Some("cc-session-123".to_string()));
        assert_eq!(task.status, "starting");
        assert_eq!(task.initial_prompt, Some("continue".to_string()));
    }

    #[tokio::test]
    async fn insert_claude_task_duplicate_id_fails() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_session(&pool, "sess-1", host_id).await;
        insert_session(&pool, "sess-2", host_id).await;

        insert_claude_task(
            &pool, "task-1", "sess-1", host_id, "/proj", None, None, None, None,
        )
        .await
        .unwrap();

        let result = insert_claude_task(
            &pool, "task-1", "sess-2", host_id, "/proj", None, None, None, None,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_claude_tasks_combined_filters() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj1").await;

        let s1 = "sess-1";
        let s2 = "sess-2";
        let s3 = "sess-3";
        insert_session(&pool, s1, host_id).await;
        insert_session(&pool, s2, host_id).await;
        insert_session(&pool, s3, host_id).await;

        insert_claude_task(
            &pool,
            "t1",
            s1,
            host_id,
            "/proj1",
            Some("proj-1"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        insert_claude_task(
            &pool,
            "t2",
            s2,
            host_id,
            "/proj1",
            Some("proj-1"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        insert_claude_task(&pool, "t3", s3, host_id, "/proj2", None, None, None, None)
            .await
            .unwrap();

        // Set t1 to active
        sqlx::query("UPDATE claude_sessions SET status = 'active' WHERE id = 't1'")
            .execute(&pool)
            .await
            .unwrap();

        // Filter: host + status + project
        let filter = ListClaudeTasksFilter {
            host_id: Some(host_id.to_string()),
            status: Some("active".to_string()),
            project_id: Some("proj-1".to_string()),
        };
        let tasks = list_claude_tasks(&pool, &filter).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t1");
    }
}
