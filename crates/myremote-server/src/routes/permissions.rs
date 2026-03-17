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
        return Err(AppError::BadRequest(
            "tool_pattern must not be empty".to_string(),
        ));
    }

    let id = body.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let rule = q::upsert_permission(
        &state.db,
        &id,
        &body.scope,
        &body.tool_pattern,
        &body.action,
    )
    .await?;
    Ok(Json(rule))
}

/// `DELETE /api/permissions/:id` - delete a permission rule.
pub async fn delete_permission(
    State(state): State<Arc<AppState>>,
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
    use axum::routing::{delete, get};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn test_state() -> Arc<AppState> {
        let pool = myremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let connections = Arc::new(crate::state::ConnectionManager::new());
        let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let agentic_loops = std::sync::Arc::new(dashmap::DashMap::new());
        let (events_tx, _) = tokio::sync::broadcast::channel(1024);
        Arc::new(AppState {
            db: pool,
            connections,
            sessions,
            agentic_loops,
            agent_token_hash: crate::auth::hash_token("test-token"),
            shutdown: tokio_util::sync::CancellationToken::new(),
            events: events_tx,
            knowledge_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        })
    }

    fn build_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/api/permissions",
                get(list_permissions).put(upsert_permission),
            )
            .route("/api/permissions/{rule_id}", delete(delete_permission))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_permissions_empty() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/permissions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn upsert_and_list_permission() {
        let state = test_state().await;
        let app = build_router(Arc::clone(&state));
        let body = serde_json::json!({
            "scope": "global",
            "tool_pattern": "Bash*",
            "action": "auto_approve",
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let rule: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(rule["scope"], "global");
        assert_eq!(rule["tool_pattern"], "Bash*");
        assert_eq!(rule["action"], "auto_approve");
        assert!(rule["id"].as_str().is_some());

        // List should return one rule
        let app2 = build_router(Arc::clone(&state));
        let resp2 = app2
            .oneshot(
                Request::get("/api/permissions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body2 = resp2.into_body().collect().await.unwrap().to_bytes();
        let rules: Vec<serde_json::Value> = serde_json::from_slice(&body2).unwrap();
        assert_eq!(rules.len(), 1);
    }

    #[tokio::test]
    async fn upsert_permission_with_explicit_id() {
        let state = test_state().await;
        let app = build_router(state);
        let body = serde_json::json!({
            "id": "custom-id-1",
            "scope": "host:abc",
            "tool_pattern": "Read",
            "action": "ask",
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let rule: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(rule["id"], "custom-id-1");
    }

    #[tokio::test]
    async fn upsert_permission_invalid_action() {
        let state = test_state().await;
        let app = build_router(state);
        let body = serde_json::json!({
            "scope": "global",
            "tool_pattern": "Bash",
            "action": "invalid_action",
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn upsert_permission_empty_tool_pattern() {
        let state = test_state().await;
        let app = build_router(state);
        let body = serde_json::json!({
            "scope": "global",
            "tool_pattern": "",
            "action": "ask",
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_permission_success() {
        let state = test_state().await;
        // Insert a rule first
        let app = build_router(Arc::clone(&state));
        let body = serde_json::json!({
            "id": "to-delete",
            "scope": "global",
            "tool_pattern": "Bash",
            "action": "deny",
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Delete it
        let app2 = build_router(Arc::clone(&state));
        let resp2 = app2
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/permissions/to-delete")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_permission_not_found() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/permissions/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn upsert_permission_updates_existing() {
        let state = test_state().await;
        let body1 = serde_json::json!({
            "id": "update-me",
            "scope": "global",
            "tool_pattern": "Bash",
            "action": "ask",
        });
        let app = build_router(Arc::clone(&state));
        app.oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/permissions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body1).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

        // Update the same ID
        let body2 = serde_json::json!({
            "id": "update-me",
            "scope": "host:xyz",
            "tool_pattern": "Read",
            "action": "deny",
        });
        let app2 = build_router(Arc::clone(&state));
        let resp = app2
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/permissions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body2).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let rule: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(rule["id"], "update-me");
        assert_eq!(rule["scope"], "host:xyz");
        assert_eq!(rule["tool_pattern"], "Read");
        assert_eq!(rule["action"], "deny");

        // List should still have 1 rule
        let app3 = build_router(Arc::clone(&state));
        let resp3 = app3
            .oneshot(
                Request::get("/api/permissions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body3 = resp3.into_body().collect().await.unwrap().to_bytes();
        let rules: Vec<serde_json::Value> = serde_json::from_slice(&body3).unwrap();
        assert_eq!(rules.len(), 1);
    }
}
