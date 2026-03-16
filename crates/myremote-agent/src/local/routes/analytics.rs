use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use myremote_core::error::AppError;
use myremote_core::queries::analytics as q;
use serde::Deserialize;

use crate::local::state::LocalAppState;

#[derive(Debug, Deserialize)]
pub struct DateRangeQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    pub by: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CostQuery {
    pub granularity: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// `GET /api/analytics/tokens` - token usage breakdown.
pub async fn get_tokens(
    State(state): State<Arc<LocalAppState>>,
    Query(query): Query<TokenQuery>,
) -> Result<Json<Vec<q::TokenBreakdown>>, AppError> {
    let by = query.by.as_deref().unwrap_or("day");
    let rows = q::get_tokens(&state.db, by, query.from.as_ref(), query.to.as_ref()).await?;
    Ok(Json(rows))
}

/// `GET /api/analytics/cost` - cost over time.
pub async fn get_cost(
    State(state): State<Arc<LocalAppState>>,
    Query(query): Query<CostQuery>,
) -> Result<Json<Vec<q::CostPoint>>, AppError> {
    let granularity = query.granularity.as_deref().unwrap_or("day");
    let rows =
        q::get_cost(&state.db, granularity, query.from.as_ref(), query.to.as_ref()).await?;
    Ok(Json(rows))
}

/// `GET /api/analytics/sessions` - session statistics.
pub async fn get_sessions(
    State(state): State<Arc<LocalAppState>>,
    Query(query): Query<DateRangeQuery>,
) -> Result<Json<q::SessionStats>, AppError> {
    let stats = q::get_session_stats(&state.db, query.from.as_ref(), query.to.as_ref()).await?;
    Ok(Json(stats))
}

/// `GET /api/analytics/loops` - loop statistics.
pub async fn get_loops(
    State(state): State<Arc<LocalAppState>>,
    Query(query): Query<DateRangeQuery>,
) -> Result<Json<q::LoopStats>, AppError> {
    let stats = q::get_loop_stats(&state.db, query.from.as_ref(), query.to.as_ref()).await?;
    Ok(Json(stats))
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
            .route("/api/analytics/tokens", get(get_tokens))
            .route("/api/analytics/cost", get(get_cost))
            .route("/api/analytics/sessions", get(get_sessions))
            .route("/api/analytics/loops", get(get_loops))
            .with_state(state)
    }

    #[tokio::test]
    async fn tokens_empty() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/analytics/tokens")
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
    async fn cost_empty() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/analytics/cost")
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
    async fn sessions_stats() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/analytics/sessions")
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
        // Should have the stats fields
        assert!(json.get("total_sessions").is_some());
        assert!(json.get("active_sessions").is_some());
    }

    #[tokio::test]
    async fn loops_stats_empty() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/analytics/loops")
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
        assert_eq!(json["total_loops"], 0);
        assert_eq!(json["total_cost_usd"], 0.0);
    }

    #[tokio::test]
    async fn tokens_with_by_param() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/analytics/tokens?by=model")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn cost_with_granularity() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/analytics/cost?granularity=week")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn loops_stats_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4().to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, total_tokens_in, total_tokens_out, estimated_cost_usd, started_at) \
             VALUES (?, ?, 'claude', 'opus', 'completed', 1000, 500, 0.05, '2026-03-10T10:00:00Z')",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/analytics/loops")
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
        assert_eq!(json["total_loops"], 1);
        assert_eq!(json["completed"], 1);
    }
}
