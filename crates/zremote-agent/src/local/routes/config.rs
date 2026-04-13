use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use zremote_core::error::{AppError, AppJson};
use zremote_core::queries::config as q;
use zremote_core::queries::sessions as sq;

use crate::local::state::LocalAppState;

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
    State(state): State<Arc<LocalAppState>>,
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
    State(state): State<Arc<LocalAppState>>,
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
    State(state): State<Arc<LocalAppState>>,
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
    State(state): State<Arc<LocalAppState>>,
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
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;

    use crate::local::upsert_local_host;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        )
    }

    fn build_test_router(state: Arc<LocalAppState>) -> Router {
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

    #[tokio::test]
    async fn get_global_config_not_found() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/config/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_and_get_global_config() {
        let state = test_state().await;
        let app = build_test_router(state);

        // Set
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/config/theme")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"value": "dark"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["key"], "theme");
        assert_eq!(json["value"], "dark");

        // Get
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/config/theme")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["key"], "theme");
        assert_eq!(json["value"], "dark");
    }

    #[tokio::test]
    async fn get_host_config_not_found() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/config/nonexistent"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_and_get_host_config() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        // Set
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/hosts/{host_id}/config/scan_interval"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"value": "300"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Get
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/config/scan_interval"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["key"], "scan_interval");
        assert_eq!(json["value"], "300");
    }

    #[tokio::test]
    async fn set_host_config_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/hosts/not-a-uuid/config/key")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"value": "val"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn set_host_config_host_not_found() {
        let state = test_state().await;
        let fake_host = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/hosts/{fake_host}/config/key"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"value": "val"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
