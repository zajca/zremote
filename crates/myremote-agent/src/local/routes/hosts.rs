use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use myremote_core::error::AppError;
use myremote_core::queries::hosts as q;
use uuid::Uuid;

use crate::local::state::LocalAppState;

/// `GET /api/hosts` - list hosts (returns the single local host).
pub async fn list_hosts(
    State(state): State<Arc<LocalAppState>>,
) -> Result<Json<Vec<q::HostRow>>, AppError> {
    let hosts = q::list_hosts(&state.db).await?;
    Ok(Json(hosts))
}

/// `GET /api/hosts/:host_id` - get host detail (validates it matches local host).
pub async fn get_host(
    State(state): State<Arc<LocalAppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<q::HostRow>, AppError> {
    let parsed: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    if parsed != state.host_id {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    let host = q::get_host(&state.db, &host_id).await?;
    Ok(Json(host))
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

    #[tokio::test]
    async fn list_hosts_returns_local_host() {
        let state = test_state().await;
        let app = Router::new()
            .route("/api/hosts", get(list_hosts))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/hosts")
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
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["hostname"], "test-host");
        assert_eq!(json[0]["status"], "online");
    }

    #[tokio::test]
    async fn get_host_returns_local_host() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = Router::new()
            .route("/api/hosts/{host_id}", get(get_host))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}"))
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
        assert_eq!(json["hostname"], "test-host");
    }

    #[tokio::test]
    async fn get_host_wrong_id_returns_404() {
        let state = test_state().await;
        let wrong_id = Uuid::new_v4();
        let app = Router::new()
            .route("/api/hosts/{host_id}", get(get_host))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{wrong_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_host_invalid_uuid_returns_400() {
        let state = test_state().await;
        let app = Router::new()
            .route("/api/hosts/{host_id}", get(get_host))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/hosts/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
