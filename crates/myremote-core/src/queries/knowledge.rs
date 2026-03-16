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
