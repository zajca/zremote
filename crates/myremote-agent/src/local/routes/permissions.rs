use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_core::error::{AppError, AppJson};
use myremote_core::queries::permissions as q;
use serde::Deserialize;
use uuid::Uuid;

use crate::local::state::LocalAppState;

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
    State(state): State<Arc<LocalAppState>>,
) -> Result<Json<Vec<PermissionRuleResponse>>, AppError> {
    let rules = q::list_permissions(&state.db).await?;
    Ok(Json(rules))
}

/// `PUT /api/permissions` - upsert a permission rule.
pub async fn upsert_permission(
    State(state): State<Arc<LocalAppState>>,
    AppJson(body): AppJson<UpsertPermissionRequest>,
) -> Result<Json<PermissionRuleResponse>, AppError> {
    validate_action(&body.action)?;

    if body.tool_pattern.is_empty() {
        return Err(AppError::BadRequest(
            "tool_pattern must not be empty".to_string(),
        ));
    }

    let id = body.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let rule =
        q::upsert_permission(&state.db, &id, &body.scope, &body.tool_pattern, &body.action)
            .await?;
    Ok(Json(rule))
}

/// `DELETE /api/permissions/:id` - delete a permission rule.
pub async fn delete_permission(
    State(state): State<Arc<LocalAppState>>,
    Path(rule_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let rows = q::delete_permission(&state.db, &rule_id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "permission rule {rule_id} not found"
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{delete, get, put};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use crate::local::upsert_local_host;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = myremote_core::db::init_db("sqlite::memory:")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(pool, "test-host".to_string(), host_id, shutdown, false)
    }

    fn build_test_router(state: Arc<LocalAppState>) -> Router {
        Router::new()
            .route(
                "/api/permissions",
                get(list_permissions).put(upsert_permission),
            )
            .route("/api/permissions/{id}", delete(delete_permission))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_permissions_empty() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/permissions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn upsert_and_list_permission() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "scope": "global",
                            "tool_pattern": "Bash",
                            "action": "auto_approve"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["scope"], "global");
        assert_eq!(json["tool_pattern"], "Bash");
        assert_eq!(json["action"], "auto_approve");

        // List should return the rule
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/permissions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
    }

    #[tokio::test]
    async fn upsert_invalid_action() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "scope": "global",
                            "tool_pattern": "Bash",
                            "action": "invalid_action"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn upsert_empty_tool_pattern() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "scope": "global",
                            "tool_pattern": "",
                            "action": "ask"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_permission_not_found() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/permissions/nonexistent-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn upsert_and_delete_permission() {
        let state = test_state().await;
        let app = build_test_router(state);

        // Create
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "id": "test-rule-1",
                            "scope": "global",
                            "tool_pattern": "Read",
                            "action": "deny"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Delete
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/permissions/test-rule-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify gone
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/permissions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }
}
