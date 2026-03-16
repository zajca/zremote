use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_core::queries::permissions as q;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::{AppError, AppJson};
use crate::state::AppState;

pub type PermissionRuleResponse = q::PermissionRuleRow;

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
    let rules = q::list_permissions(&state.db).await?;
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
    let rule = q::upsert_permission(&state.db, &id, &body.scope, &body.tool_pattern, &body.action).await?;
    Ok(Json(rule))
}

/// `DELETE /api/permissions/:id` - delete a permission rule.
pub async fn delete_permission(
    State(state): State<Arc<AppState>>,
    Path(rule_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let rows = q::delete_permission(&state.db, &rule_id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!("permission rule {rule_id} not found")));
    }
    Ok(StatusCode::NO_CONTENT)
}
