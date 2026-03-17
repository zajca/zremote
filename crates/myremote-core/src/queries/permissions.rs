use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppError;

/// Permission rule response for API.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct PermissionRuleRow {
    pub id: String,
    pub scope: String,
    pub tool_pattern: String,
    pub action: String,
}

pub async fn list_permissions(pool: &SqlitePool) -> Result<Vec<PermissionRuleRow>, AppError> {
    let rules: Vec<PermissionRuleRow> = sqlx::query_as(
        "SELECT id, scope, tool_pattern, action FROM permission_rules ORDER BY scope, tool_pattern",
    )
    .fetch_all(pool)
    .await?;
    Ok(rules)
}

pub async fn upsert_permission(
    pool: &SqlitePool,
    id: &str,
    scope: &str,
    tool_pattern: &str,
    action: &str,
) -> Result<PermissionRuleRow, AppError> {
    sqlx::query(
        "INSERT INTO permission_rules (id, scope, tool_pattern, action) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET scope = excluded.scope, \
         tool_pattern = excluded.tool_pattern, action = excluded.action",
    )
    .bind(id)
    .bind(scope)
    .bind(tool_pattern)
    .bind(action)
    .execute(pool)
    .await?;

    let rule: PermissionRuleRow =
        sqlx::query_as("SELECT id, scope, tool_pattern, action FROM permission_rules WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?;
    Ok(rule)
}

pub async fn delete_permission(pool: &SqlitePool, rule_id: &str) -> Result<u64, AppError> {
    let result = sqlx::query("DELETE FROM permission_rules WHERE id = ?")
        .bind(rule_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
