use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use chrono::Utc;
use myremote_core::queries::config as q;
use myremote_core::queries::sessions as sq;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppJson};
use crate::state::AppState;

/// Config value response.
#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

/// Request body for setting a config value.
#[derive(Debug, Deserialize)]
pub struct SetConfigRequest {
    pub value: String,
}

/// `GET /api/config/:key` - get global config value.
pub async fn get_global_config(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<ConfigResponse>, AppError> {
    let (key, value, updated_at) = q::get_global_config(&state.db, &key)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("config key '{key}' not found")))?;

    Ok(Json(ConfigResponse {
        key,
        value,
        updated_at,
    }))
}

/// `PUT /api/config/:key` - set global config value.
pub async fn set_global_config(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    AppJson(body): AppJson<SetConfigRequest>,
) -> Result<Json<ConfigResponse>, AppError> {
    let now = Utc::now().to_rfc3339();
    q::set_global_config(&state.db, &key, &body.value, &now).await?;

    Ok(Json(ConfigResponse {
        key,
        value: body.value,
        updated_at: now,
    }))
}

/// `GET /api/hosts/:host_id/config/:key` - get host config value.
pub async fn get_host_config(
    State(state): State<Arc<AppState>>,
    Path((host_id, key)): Path<(String, String)>,
) -> Result<Json<ConfigResponse>, AppError> {
    let _parsed: uuid::Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    let (key, value, updated_at) = q::get_host_config(&state.db, &host_id, &key)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("config key '{key}' not found for host")))?;

    Ok(Json(ConfigResponse {
        key,
        value,
        updated_at,
    }))
}

/// `PUT /api/hosts/:host_id/config/:key` - set host config value.
pub async fn set_host_config(
    State(state): State<Arc<AppState>>,
    Path((host_id, key)): Path<(String, String)>,
    AppJson(body): AppJson<SetConfigRequest>,
) -> Result<Json<ConfigResponse>, AppError> {
    let _parsed: uuid::Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    if !sq::host_exists(&state.db, &host_id).await? {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    let now = Utc::now().to_rfc3339();
    q::set_host_config(&state.db, &host_id, &key, &body.value, &now).await?;

    Ok(Json(ConfigResponse {
        key,
        value: body.value,
        updated_at: now,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
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
                "/api/config/{key}",
                get(get_global_config).put(set_global_config),
            )
            .route(
                "/api/hosts/{host_id}/config/{key}",
                get(get_host_config).put(set_host_config),
            )
            .with_state(state)
    }

    async fn insert_host(state: &AppState, host_id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES (?, 'test', 'test-host', 'hash', 'online')",
        )
        .bind(host_id)
        .execute(&state.db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn get_global_config_not_found() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/config/missing-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_and_get_global_config() {
        let state = test_state().await;
        let body = serde_json::json!({ "value": "dark" });
        let app = build_router(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/config/theme")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(json["key"], "theme");
        assert_eq!(json["value"], "dark");
        assert!(json["updated_at"].as_str().is_some());

        // Now GET it
        let app2 = build_router(Arc::clone(&state));
        let resp2 = app2
            .oneshot(
                Request::get("/api/config/theme")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
        let body2 = resp2.into_body().collect().await.unwrap().to_bytes();
        let json2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        assert_eq!(json2["key"], "theme");
        assert_eq!(json2["value"], "dark");
    }

    #[tokio::test]
    async fn set_global_config_overwrites() {
        let state = test_state().await;
        let body1 = serde_json::json!({ "value": "v1" });
        let app = build_router(Arc::clone(&state));
        app.oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/config/key1")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body1).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

        let body2 = serde_json::json!({ "value": "v2" });
        let app2 = build_router(Arc::clone(&state));
        let resp = app2
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/config/key1")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body2).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let app3 = build_router(Arc::clone(&state));
        let resp3 = app3
            .oneshot(
                Request::get("/api/config/key1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body3 = resp3.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body3).unwrap();
        assert_eq!(json["value"], "v2");
    }

    #[tokio::test]
    async fn get_host_config_not_found() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}/config/missing"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_host_config_invalid_host_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/hosts/not-a-uuid/config/key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_and_get_host_config() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let body = serde_json::json!({ "value": "100" });
        let app = build_router(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/hosts/{host_id}/config/max_sessions"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(json["key"], "max_sessions");
        assert_eq!(json["value"], "100");

        // GET it back
        let app2 = build_router(Arc::clone(&state));
        let resp2 = app2
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}/config/max_sessions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
        let body2 = resp2.into_body().collect().await.unwrap().to_bytes();
        let json2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        assert_eq!(json2["value"], "100");
    }

    #[tokio::test]
    async fn set_host_config_nonexistent_host() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        // Do NOT insert host
        let body = serde_json::json!({ "value": "val" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/hosts/{host_id}/config/key"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_host_config_invalid_host_id() {
        let state = test_state().await;
        let body = serde_json::json!({ "value": "val" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/hosts/bad-uuid/config/key")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
