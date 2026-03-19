use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use zremote_core::queries::analytics as q;

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct DateRangeQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    pub by: Option<String>, // day | model | host | project
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CostQuery {
    pub granularity: Option<String>, // day | week | month
    pub from: Option<String>,
    pub to: Option<String>,
}

/// `GET /api/analytics/tokens` - token usage breakdown.
pub async fn get_tokens(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TokenQuery>,
) -> Result<Json<Vec<q::TokenBreakdown>>, AppError> {
    let by = query.by.as_deref().unwrap_or("day");
    let rows = q::get_tokens(&state.db, by, query.from.as_ref(), query.to.as_ref()).await?;
    Ok(Json(rows))
}

/// `GET /api/analytics/cost` - cost over time.
pub async fn get_cost(
    State(state): State<Arc<AppState>>,
    Query(query): Query<CostQuery>,
) -> Result<Json<Vec<q::CostPoint>>, AppError> {
    let granularity = query.granularity.as_deref().unwrap_or("day");
    let rows = q::get_cost(
        &state.db,
        granularity,
        query.from.as_ref(),
        query.to.as_ref(),
    )
    .await?;
    Ok(Json(rows))
}

/// `GET /api/analytics/sessions` - session statistics.
pub async fn get_sessions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DateRangeQuery>,
) -> Result<Json<q::SessionStats>, AppError> {
    let stats = q::get_session_stats(&state.db, query.from.as_ref(), query.to.as_ref()).await?;
    Ok(Json(stats))
}

/// `GET /api/analytics/loops` - loop statistics.
pub async fn get_loops(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DateRangeQuery>,
) -> Result<Json<q::LoopStats>, AppError> {
    let stats = q::get_loop_stats(&state.db, query.from.as_ref(), query.to.as_ref()).await?;
    Ok(Json(stats))
}

#[cfg(test)]
mod tests {
    use crate::db;
    use zremote_core::queries::analytics::{LoopStats, SessionStats, TokenBreakdown};

    async fn setup_db() -> sqlx::SqlitePool {
        let pool = db::init_db("sqlite::memory:").await.unwrap();

        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('h1', 'test', 'test-host', 'hash', 'online')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES ('s1', 'h1', 'active')")
            .execute(&pool)
            .await
            .unwrap();

        pool
    }

    #[tokio::test]
    async fn tokens_empty_returns_empty_vec() {
        let pool = setup_db().await;
        let rows: Vec<TokenBreakdown> = sqlx::query_as(
            "SELECT date(started_at) as label, \
             COALESCE(SUM(total_tokens_in), 0) as tokens_in, \
             COALESCE(SUM(total_tokens_out), 0) as tokens_out \
             FROM agentic_loops GROUP BY date(started_at)",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn loop_stats_empty() {
        let pool = setup_db().await;
        let stats: LoopStats = sqlx::query_as(
            "SELECT \
             COUNT(*) as total_loops, \
             SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as completed, \
             SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as errored, \
             AVG(estimated_cost_usd) as avg_cost_usd, \
             COALESCE(SUM(estimated_cost_usd), 0.0) as total_cost_usd, \
             COALESCE(SUM(total_tokens_in), 0) as total_tokens_in, \
             COALESCE(SUM(total_tokens_out), 0) as total_tokens_out \
             FROM agentic_loops",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stats.total_loops, 0);
        assert_eq!(stats.total_cost_usd, 0.0);
    }

    #[tokio::test]
    async fn loop_stats_with_data() {
        let pool = setup_db().await;

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, total_tokens_in, total_tokens_out, estimated_cost_usd, started_at) \
             VALUES ('l1', 's1', 'claude', 'opus', 'completed', 1000, 500, 0.05, '2026-03-10T10:00:00Z')"
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, total_tokens_in, total_tokens_out, estimated_cost_usd, started_at) \
             VALUES ('l2', 's1', 'claude', 'sonnet', 'error', 2000, 1000, 0.10, '2026-03-10T11:00:00Z')"
        )
        .execute(&pool)
        .await
        .unwrap();

        let stats: LoopStats = sqlx::query_as(
            "SELECT \
             COUNT(*) as total_loops, \
             SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as completed, \
             SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as errored, \
             AVG(estimated_cost_usd) as avg_cost_usd, \
             COALESCE(SUM(estimated_cost_usd), 0.0) as total_cost_usd, \
             COALESCE(SUM(total_tokens_in), 0) as total_tokens_in, \
             COALESCE(SUM(total_tokens_out), 0) as total_tokens_out \
             FROM agentic_loops",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(stats.total_loops, 2);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.errored, 1);
        assert!((stats.total_cost_usd - 0.15).abs() < 0.001);
        assert_eq!(stats.total_tokens_in, 3000);
        assert_eq!(stats.total_tokens_out, 1500);
    }

    #[tokio::test]
    async fn session_stats_empty() {
        let pool = setup_db().await;
        let stats: SessionStats = sqlx::query_as(
            "SELECT \
             COUNT(*) as total_sessions, \
             SUM(CASE WHEN status = 'active' THEN 1 ELSE 0 END) as active_sessions, \
             AVG(CASE WHEN ss.duration_seconds > 0 THEN ss.duration_seconds ELSE NULL END) as avg_duration_seconds \
             FROM sessions LEFT JOIN session_stats ss ON ss.session_id = sessions.id"
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stats.total_sessions, 1);
        assert_eq!(stats.active_sessions, 1);
    }

    use tower::ServiceExt;

    // --- Route-level integration tests ---

    async fn test_state() -> std::sync::Arc<crate::state::AppState> {
        let pool = db::init_db("sqlite::memory:").await.unwrap();
        let connections = std::sync::Arc::new(crate::state::ConnectionManager::new());
        let sessions =
            std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let agentic_loops = std::sync::Arc::new(dashmap::DashMap::new());
        let (events_tx, _) = tokio::sync::broadcast::channel(1024);
        std::sync::Arc::new(crate::state::AppState {
            db: pool,
            connections,
            sessions,
            agentic_loops,
            agent_token_hash: crate::auth::hash_token("test-token"),
            shutdown: tokio_util::sync::CancellationToken::new(),
            events: events_tx,
            knowledge_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            directory_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            settings_get_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            settings_save_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            action_inputs_requests: std::sync::Arc::new(dashmap::DashMap::new()),
        })
    }

    async fn seed_analytics_data(pool: &sqlx::SqlitePool) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('h1', 'test', 'test-host', 'hash', 'online')",
        )
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, created_at) VALUES ('s1', 'h1', 'active', '2026-03-10T09:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, total_tokens_in, total_tokens_out, estimated_cost_usd, started_at) \
             VALUES ('l1', 's1', 'claude', 'opus', 'completed', 1000, 500, 0.05, '2026-03-10T10:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, total_tokens_in, total_tokens_out, estimated_cost_usd, started_at) \
             VALUES ('l2', 's1', 'claude', 'sonnet', 'error', 2000, 1000, 0.10, '2026-03-11T11:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn get_tokens_route_returns_ok() {
        let state = test_state().await;
        seed_analytics_data(&state.db).await;

        let app = crate::create_router(state, None);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/analytics/tokens")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let tokens: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tokens.len(), 2); // two different days
    }

    #[tokio::test]
    async fn get_tokens_by_model() {
        let state = test_state().await;
        seed_analytics_data(&state.db).await;

        let app = crate::create_router(state, None);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/analytics/tokens?by=model")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let tokens: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tokens.len(), 2); // opus + sonnet
    }

    #[tokio::test]
    async fn get_tokens_with_date_filter() {
        let state = test_state().await;
        seed_analytics_data(&state.db).await;

        let app = crate::create_router(state, None);
        let response = app
            .oneshot(
                axum::http::Request::get(
                    "/api/analytics/tokens?from=2026-03-11T00:00:00Z&to=2026-03-12T00:00:00Z",
                )
                .body(axum::body::Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let tokens: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(tokens.len(), 1); // only the 2026-03-11 entry
    }

    #[tokio::test]
    async fn get_cost_route_returns_ok() {
        let state = test_state().await;
        seed_analytics_data(&state.db).await;

        let app = crate::create_router(state, None);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/analytics/cost")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let costs: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(costs.len(), 2);
    }

    #[tokio::test]
    async fn get_cost_by_month() {
        let state = test_state().await;
        seed_analytics_data(&state.db).await;

        let app = crate::create_router(state, None);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/analytics/cost?granularity=month")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let costs: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        // Both entries are in 2026-03, so grouped into one month
        assert_eq!(costs.len(), 1);
        let cost = costs[0]["cost"].as_f64().unwrap();
        assert!((cost - 0.15).abs() < 0.001);
    }

    #[tokio::test]
    async fn get_sessions_stats_route_returns_ok() {
        let app_state = test_state().await;
        seed_analytics_data(&app_state.db).await;

        let app = crate::create_router(app_state, None);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/analytics/sessions")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total_sessions"], 1);
        assert_eq!(json["active_sessions"], 1);
    }

    #[tokio::test]
    async fn get_sessions_stats_with_date_range() {
        let app_state = test_state().await;
        seed_analytics_data(&app_state.db).await;

        let app = crate::create_router(app_state, None);
        // Filter to a date range that excludes the session
        let response = app
            .oneshot(
                axum::http::Request::get("/api/analytics/sessions?from=2027-01-01T00:00:00Z")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total_sessions"], 0);
    }

    #[tokio::test]
    async fn get_loops_stats_route_returns_ok() {
        let app_state = test_state().await;
        seed_analytics_data(&app_state.db).await;

        let app = crate::create_router(app_state, None);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/analytics/loops")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total_loops"], 2);
        assert_eq!(json["completed"], 1);
        assert_eq!(json["errored"], 1);
    }

    #[tokio::test]
    async fn get_loops_stats_with_date_range() {
        let app_state = test_state().await;
        seed_analytics_data(&app_state.db).await;

        let app = crate::create_router(app_state, None);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/analytics/loops?from=2026-03-11T00:00:00Z")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total_loops"], 1);
        assert_eq!(json["errored"], 1);
    }

    #[tokio::test]
    async fn fts_search_returns_matching_transcript() {
        let pool = setup_db().await;

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) \
             VALUES ('l1', 's1', 'claude', 'completed')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) \
             VALUES ('l1', 'assistant', 'Hello world this is a test function', '2026-03-10T10:00:00Z')"
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT rowid FROM transcript_fts WHERE content MATCH 'function'")
                .fetch_all(&pool)
                .await
                .unwrap();

        assert_eq!(rows.len(), 1);
    }
}
