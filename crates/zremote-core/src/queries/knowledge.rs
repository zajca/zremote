use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct KnowledgeBaseRow {
    pub id: String,
    pub host_id: String,
    pub status: String,
    pub openviking_version: Option<String>,
    pub last_error: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct MemoryRow {
    pub id: String,
    pub project_id: String,
    pub loop_id: Option<String>,
    pub key: String,
    pub content: String,
    pub category: String,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
}

pub async fn get_kb_status(
    pool: &SqlitePool,
    host_id: &str,
) -> Result<Option<KnowledgeBaseRow>, AppError> {
    let kb: Option<KnowledgeBaseRow> = sqlx::query_as(
        "SELECT id, host_id, status, openviking_version, last_error, started_at, updated_at \
         FROM knowledge_bases WHERE host_id = ?",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await?;
    Ok(kb)
}

pub async fn list_memories(
    pool: &SqlitePool,
    project_id: &str,
    category: Option<&str>,
) -> Result<Vec<MemoryRow>, AppError> {
    let memories = if let Some(cat) = category {
        sqlx::query_as::<_, MemoryRow>(
            "SELECT id, project_id, loop_id, key, content, category, confidence, created_at, updated_at \
             FROM knowledge_memories WHERE project_id = ? AND category = ? ORDER BY updated_at DESC",
        )
        .bind(project_id)
        .bind(cat)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, MemoryRow>(
            "SELECT id, project_id, loop_id, key, content, category, confidence, created_at, updated_at \
             FROM knowledge_memories WHERE project_id = ? ORDER BY updated_at DESC",
        )
        .bind(project_id)
        .fetch_all(pool)
        .await?
    };
    Ok(memories)
}

pub async fn delete_memory(
    pool: &SqlitePool,
    memory_id: &str,
    project_id: &str,
) -> Result<u64, AppError> {
    let result = sqlx::query("DELETE FROM knowledge_memories WHERE id = ? AND project_id = ?")
        .bind(memory_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn update_memory_content(
    pool: &SqlitePool,
    memory_id: &str,
    project_id: &str,
    content: &str,
    now: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE knowledge_memories SET content = ?, updated_at = ? WHERE id = ? AND project_id = ?",
    )
    .bind(content)
    .bind(now)
    .bind(memory_id)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_memory_category(
    pool: &SqlitePool,
    memory_id: &str,
    project_id: &str,
    category: &str,
    now: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE knowledge_memories SET category = ?, updated_at = ? WHERE id = ? AND project_id = ?",
    )
    .bind(category)
    .bind(now)
    .bind(memory_id)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_memory(
    pool: &SqlitePool,
    memory_id: &str,
    project_id: &str,
) -> Result<MemoryRow, AppError> {
    let memory: MemoryRow = sqlx::query_as(
        "SELECT id, project_id, loop_id, key, content, category, confidence, created_at, updated_at \
         FROM knowledge_memories WHERE id = ? AND project_id = ?",
    )
    .bind(memory_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("memory {memory_id} not found")))?;
    Ok(memory)
}

pub async fn get_transcript_for_loop(
    pool: &SqlitePool,
    loop_id: &str,
) -> Result<Vec<(String, String, String)>, AppError> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT role, content, timestamp FROM transcript_entries WHERE loop_id = ? ORDER BY id",
    )
    .bind(loop_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
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

    async fn insert_session(pool: &SqlitePool, session_id: &str, host_id: &str) {
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(session_id)
            .bind(host_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn insert_loop(pool: &SqlitePool, loop_id: &str, session_id: &str) {
        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name) VALUES (?, ?, 'claude-code')",
        )
        .bind(loop_id)
        .bind(session_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_memory(
        pool: &SqlitePool,
        id: &str,
        project_id: &str,
        key: &str,
        content: &str,
        category: &str,
    ) {
        sqlx::query(
            "INSERT INTO knowledge_memories (id, project_id, key, content, category, confidence) VALUES (?, ?, ?, ?, ?, 0.9)",
        )
        .bind(id)
        .bind(project_id)
        .bind(key)
        .bind(content)
        .bind(category)
        .execute(pool)
        .await
        .unwrap();
    }

    // --- get_kb_status tests ---

    #[tokio::test]
    async fn get_kb_status_no_entry() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;

        let result = get_kb_status(&pool, host_id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_kb_status_with_entry() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;

        sqlx::query(
            "INSERT INTO knowledge_bases (id, host_id, status, openviking_version) VALUES ('kb-1', ?, 'ready', '0.2.0')",
        )
        .bind(host_id)
        .execute(&pool)
        .await
        .unwrap();

        let kb = get_kb_status(&pool, host_id).await.unwrap().unwrap();
        assert_eq!(kb.id, "kb-1");
        assert_eq!(kb.host_id, host_id);
        assert_eq!(kb.status, "ready");
        assert_eq!(kb.openviking_version, Some("0.2.0".to_string()));
        assert!(kb.last_error.is_none());
    }

    // --- list_memories tests ---

    #[tokio::test]
    async fn list_memories_empty() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;

        let memories = list_memories(&pool, "proj-1", None).await.unwrap();
        assert!(memories.is_empty());
    }

    #[tokio::test]
    async fn list_memories_returns_all_for_project() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;

        insert_memory(&pool, "m1", "proj-1", "key1", "content1", "pattern").await;
        insert_memory(&pool, "m2", "proj-1", "key2", "content2", "decision").await;

        let memories = list_memories(&pool, "proj-1", None).await.unwrap();
        assert_eq!(memories.len(), 2);
    }

    #[tokio::test]
    async fn list_memories_filters_by_category() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;

        insert_memory(&pool, "m1", "proj-1", "key1", "content1", "pattern").await;
        insert_memory(&pool, "m2", "proj-1", "key2", "content2", "decision").await;
        insert_memory(&pool, "m3", "proj-1", "key3", "content3", "pattern").await;

        let memories = list_memories(&pool, "proj-1", Some("pattern"))
            .await
            .unwrap();
        assert_eq!(memories.len(), 2);
        for m in &memories {
            assert_eq!(m.category, "pattern");
        }
    }

    #[tokio::test]
    async fn list_memories_does_not_return_other_projects() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj1").await;
        insert_project(&pool, "proj-2", host_id, "/proj2").await;

        insert_memory(&pool, "m1", "proj-1", "key1", "content1", "pattern").await;
        insert_memory(&pool, "m2", "proj-2", "key2", "content2", "pattern").await;

        let memories = list_memories(&pool, "proj-1", None).await.unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].project_id, "proj-1");
    }

    // --- delete_memory tests ---

    #[tokio::test]
    async fn delete_memory_existing() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;
        insert_memory(&pool, "m1", "proj-1", "key1", "content1", "pattern").await;

        let affected = delete_memory(&pool, "m1", "proj-1").await.unwrap();
        assert_eq!(affected, 1);

        // Verify deleted
        let memories = list_memories(&pool, "proj-1", None).await.unwrap();
        assert!(memories.is_empty());
    }

    #[tokio::test]
    async fn delete_memory_nonexistent() {
        let pool = test_db().await;
        let affected = delete_memory(&pool, "nonexistent", "proj-1").await.unwrap();
        assert_eq!(affected, 0);
    }

    #[tokio::test]
    async fn delete_memory_wrong_project() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj1").await;
        insert_project(&pool, "proj-2", host_id, "/proj2").await;
        insert_memory(&pool, "m1", "proj-1", "key1", "content1", "pattern").await;

        // Try to delete with wrong project_id
        let affected = delete_memory(&pool, "m1", "proj-2").await.unwrap();
        assert_eq!(affected, 0);

        // Still exists
        let memories = list_memories(&pool, "proj-1", None).await.unwrap();
        assert_eq!(memories.len(), 1);
    }

    // --- update_memory_content tests ---

    #[tokio::test]
    async fn update_memory_content_succeeds() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;
        insert_memory(&pool, "m1", "proj-1", "key1", "old content", "pattern").await;

        let now = chrono::Utc::now().to_rfc3339();
        update_memory_content(&pool, "m1", "proj-1", "new content", &now)
            .await
            .unwrap();

        let mem = get_memory(&pool, "m1", "proj-1").await.unwrap();
        assert_eq!(mem.content, "new content");
    }

    // --- update_memory_category tests ---

    #[tokio::test]
    async fn update_memory_category_succeeds() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;
        insert_memory(&pool, "m1", "proj-1", "key1", "content1", "pattern").await;

        let now = chrono::Utc::now().to_rfc3339();
        update_memory_category(&pool, "m1", "proj-1", "decision", &now)
            .await
            .unwrap();

        let mem = get_memory(&pool, "m1", "proj-1").await.unwrap();
        assert_eq!(mem.category, "decision");
    }

    // --- get_memory tests ---

    #[tokio::test]
    async fn get_memory_found() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;
        insert_memory(
            &pool,
            "m1",
            "proj-1",
            "error-handling",
            "Always use Result",
            "pattern",
        )
        .await;

        let mem = get_memory(&pool, "m1", "proj-1").await.unwrap();
        assert_eq!(mem.id, "m1");
        assert_eq!(mem.project_id, "proj-1");
        assert_eq!(mem.key, "error-handling");
        assert_eq!(mem.content, "Always use Result");
        assert_eq!(mem.category, "pattern");
        assert!((mem.confidence - 0.9).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn get_memory_not_found() {
        let pool = test_db().await;
        let result = get_memory(&pool, "nonexistent", "proj-1").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn get_memory_wrong_project() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj1").await;
        insert_project(&pool, "proj-2", host_id, "/proj2").await;
        insert_memory(&pool, "m1", "proj-1", "key1", "content1", "pattern").await;

        let result = get_memory(&pool, "m1", "proj-2").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::NotFound(_)));
    }

    // --- get_transcript_for_loop tests ---

    #[tokio::test]
    async fn get_transcript_for_loop_empty() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_session(&pool, "sess-1", host_id).await;
        insert_loop(&pool, "loop-1", "sess-1").await;

        let rows = get_transcript_for_loop(&pool, "loop-1").await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn get_transcript_for_loop_returns_ordered() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_session(&pool, "sess-1", host_id).await;
        insert_loop(&pool, "loop-1", "sess-1").await;

        // Insert transcript entries
        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) VALUES ('loop-1', 'user', 'Fix the bug', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) VALUES ('loop-1', 'assistant', 'I will fix it', '2026-01-01T00:00:01Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows = get_transcript_for_loop(&pool, "loop-1").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "user");
        assert_eq!(rows[0].1, "Fix the bug");
        assert_eq!(rows[1].0, "assistant");
        assert_eq!(rows[1].1, "I will fix it");
    }

    #[tokio::test]
    async fn get_transcript_for_loop_does_not_return_other_loops() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_session(&pool, "sess-1", host_id).await;
        insert_loop(&pool, "loop-1", "sess-1").await;
        insert_loop(&pool, "loop-2", "sess-1").await;

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) VALUES ('loop-1', 'user', 'msg1', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) VALUES ('loop-2', 'user', 'msg2', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows = get_transcript_for_loop(&pool, "loop-1").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "msg1");
    }

    // --- list_memories ordering ---

    #[tokio::test]
    async fn list_memories_ordered_by_updated_at_desc() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;
        insert_project(&pool, "proj-1", host_id, "/proj").await;

        insert_memory(&pool, "m1", "proj-1", "key1", "old", "pattern").await;
        insert_memory(&pool, "m2", "proj-1", "key2", "new", "pattern").await;

        // Force different updated_at
        sqlx::query(
            "UPDATE knowledge_memories SET updated_at = '2026-01-01T00:00:00Z' WHERE id = 'm1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE knowledge_memories SET updated_at = '2026-01-02T00:00:00Z' WHERE id = 'm2'",
        )
        .execute(&pool)
        .await
        .unwrap();

        let memories = list_memories(&pool, "proj-1", None).await.unwrap();
        assert_eq!(memories.len(), 2);
        // m2 is newer, should come first
        assert_eq!(memories[0].id, "m2");
        assert_eq!(memories[1].id, "m1");
    }

    // --- get_kb_status with error ---

    #[tokio::test]
    async fn get_kb_status_with_error() {
        let pool = test_db().await;
        let host_id = "host-1";
        insert_host(&pool, host_id).await;

        sqlx::query(
            "INSERT INTO knowledge_bases (id, host_id, status, last_error) VALUES ('kb-1', ?, 'error', 'failed to start OV')",
        )
        .bind(host_id)
        .execute(&pool)
        .await
        .unwrap();

        let kb = get_kb_status(&pool, host_id).await.unwrap().unwrap();
        assert_eq!(kb.status, "error");
        assert_eq!(kb.last_error, Some("failed to start OV".to_string()));
    }
}
