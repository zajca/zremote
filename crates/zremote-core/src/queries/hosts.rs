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

/// Find a host by hostname or name. Used by the admin CLI's `revoke-host`
/// where the operator may pass either the UUID, hostname, or configured
/// friendly name. Returns the first matching row (hostname is unique within
/// the bootstrap server; if multiple share a `name`, this returns the
/// newest by `created_at`).
pub async fn find_by_hostname_or_name(
    pool: &SqlitePool,
    needle: &str,
) -> Result<Option<HostRow>, AppError> {
    let row: Option<HostRow> = sqlx::query_as(
        "SELECT id, name, hostname, status, last_seen_at, agent_version, os, arch, \
         created_at, updated_at FROM hosts \
         WHERE hostname = ? OR name = ? \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(needle)
    .bind(needle)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// List every host with the current count of **non-revoked** agents.
/// Used by `zremote admin list-hosts`.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct HostListEntry {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub status: String,
    pub created_at: String,
    pub agents: i64,
}

pub async fn list_hosts_with_agent_count(
    pool: &SqlitePool,
) -> Result<Vec<HostListEntry>, AppError> {
    let rows: Vec<HostListEntry> = sqlx::query_as(
        "SELECT h.id, h.name, h.hostname, h.status, h.created_at, \
         (SELECT COUNT(*) FROM agents a WHERE a.host_id = h.id AND a.revoked_at IS NULL) AS agents \
         FROM hosts h ORDER BY h.created_at DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
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
