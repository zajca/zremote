use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppJson};
use crate::state::AppState;

/// Permission rule response for API.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct PermissionRuleResponse {
    pub id: String,
    pub scope: String,
    pub tool_pattern: String,
    pub action: String,
}

/// Request body for upserting a permission rule.
#[derive(Debug, Deserialize)]
pub struct UpsertPermissionRequest {
    pub id: Option<String>,
    pub scope: String,
    pub tool_pattern: String,
    pub action: String,
}

fn validate_action(action: &str) -> Result<(), AppError> {
    match action {
        "auto_approve" | "ask" | "deny" => Ok(()),
        _ => Err(AppError::BadRequest(format!(
            "invalid action: {action}, must be one of: auto_approve, ask, deny"
        ))),
    }
}

/// `GET /api/permissions` - list all permission rules.
pub async fn list_permissions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<PermissionRuleResponse>>, AppError> {
    let rules: Vec<PermissionRuleResponse> = sqlx::query_as(
        "SELECT id, scope, tool_pattern, action FROM permission_rules ORDER BY scope, tool_pattern",
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(rules))
}

/// `PUT /api/permissions` - upsert a permission rule.
pub async fn upsert_permission(
    State(state): State<Arc<AppState>>,
    AppJson(body): AppJson<UpsertPermissionRequest>,
) -> Result<Json<PermissionRuleResponse>, AppError> {
    validate_action(&body.action)?;

    if body.tool_pattern.is_empty() {
        return Err(AppError::BadRequest("tool_pattern must not be empty".to_string()));
    }

    let id = body.id.unwrap_or_else(|| Uuid::new_v4().to_string());

    sqlx::query(
        "INSERT INTO permission_rules (id, scope, tool_pattern, action) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET scope = excluded.scope, \
         tool_pattern = excluded.tool_pattern, action = excluded.action",
    )
    .bind(&id)
    .bind(&body.scope)
    .bind(&body.tool_pattern)
    .bind(&body.action)
    .execute(&state.db)
    .await?;

    let rule: PermissionRuleResponse = sqlx::query_as(
        "SELECT id, scope, tool_pattern, action FROM permission_rules WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(rule))
}

/// `DELETE /api/permissions/:id` - delete a permission rule.
pub async fn delete_permission(
    State(state): State<Arc<AppState>>,
    Path(rule_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let result = sqlx::query("DELETE FROM permission_rules WHERE id = ?")
        .bind(&rule_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("permission rule {rule_id} not found")));
    }

    Ok(StatusCode::NO_CONTENT)
}
