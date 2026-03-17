use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;

/// Host representation for API responses.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct HostRow {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub status: String,
    pub last_seen_at: Option<String>,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub async fn list_hosts(pool: &SqlitePool) -> Result<Vec<HostRow>, AppError> {
    let hosts: Vec<HostRow> = sqlx::query_as(
        "SELECT id, name, hostname, status, last_seen_at, agent_version, os, arch, \
         created_at, updated_at FROM hosts ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    Ok(hosts)
}

pub async fn get_host(pool: &SqlitePool, host_id: &str) -> Result<HostRow, AppError> {
    let host: HostRow = sqlx::query_as(
        "SELECT id, name, hostname, status, last_seen_at, agent_version, os, arch, \
         created_at, updated_at FROM hosts WHERE id = ?",
    )
    .bind(host_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("host {host_id} not found")))?;
    Ok(host)
}

pub async fn update_host_name(
    pool: &SqlitePool,
    host_id: &str,
    name: &str,
    updated_at: &str,
) -> Result<u64, AppError> {
    let result = sqlx::query("UPDATE hosts SET name = ?, updated_at = ? WHERE id = ?")
        .bind(name)
        .bind(updated_at)
        .bind(host_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub async fn delete_host(pool: &SqlitePool, host_id: &str) -> Result<u64, AppError> {
    let result = sqlx::query("DELETE FROM hosts WHERE id = ?")
        .bind(host_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
