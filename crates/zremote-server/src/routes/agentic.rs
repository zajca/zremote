use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use zremote_core::queries::loops as q;
use zremote_core::state::LoopInfo;

use crate::error::AppError;
use crate::state::AppState;

/// Query parameters for listing agentic loops.
#[derive(Debug, Deserialize)]
pub struct ListLoopsQuery {
    pub status: Option<String>,
    pub host_id: Option<String>,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
}

fn parse_loop_id(loop_id: &str) -> Result<uuid::Uuid, AppError> {
    loop_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid loop ID: {loop_id}")))
}

/// `GET /api/loops` - list agentic loops with optional filters.
pub async fn list_loops(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ListLoopsQuery>,
) -> Result<Json<Vec<LoopInfo>>, AppError> {
    let filter = q::ListLoopsFilter {
        status: query.status,
        host_id: query.host_id,
        session_id: query.session_id,
        project_id: query.project_id,
    };
    let rows = q::list_loops(&state.db, &filter).await?;
    let loops = rows.into_iter().map(q::enrich_loop).collect();
    Ok(Json(loops))
}

/// `GET /api/loops/:id` - get loop detail.
pub async fn get_loop(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<LoopInfo>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let row = q::get_loop(&state.db, &loop_id).await?;
    Ok(Json(q::enrich_loop(row)))
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
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
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
            directory_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            settings_get_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            settings_save_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            action_inputs_requests: std::sync::Arc::new(dashmap::DashMap::new()),
            ticket_store: crate::auth::TicketStore::new(),
            oidc_flows: crate::auth::oidc::OidcFlowStore::new(),
        })
    }

    fn build_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/loops", get(list_loops))
            .route("/api/loops/{loop_id}", get(get_loop))
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

    async fn insert_session(state: &AppState, session_id: &str, host_id: &str) {
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(session_id)
            .bind(host_id)
            .execute(&state.db)
            .await
            .unwrap();
    }

    async fn insert_loop(state: &AppState, loop_id: &str, session_id: &str) {
        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, status, started_at) VALUES (?, ?, 'claude-code', 'working', '2026-01-01T00:00:00Z')",
        )
        .bind(loop_id)
        .bind(session_id)
        .execute(&state.db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn list_loops_empty() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(Request::get("/api/loops").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn list_loops_with_data() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let loop_id = uuid::Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_session(&state, &session_id, &host_id).await;
        insert_loop(&state, &loop_id, &session_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(Request::get("/api/loops").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["id"], loop_id);
    }

    #[tokio::test]
    async fn list_loops_with_status_filter() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let loop_id = uuid::Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_session(&state, &session_id, &host_id).await;
        insert_loop(&state, &loop_id, &session_id).await;

        let app = build_router(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::get("/api/loops?status=completed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty()); // loop is "working", not "completed"
    }

    #[tokio::test]
    async fn get_loop_found() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let loop_id = uuid::Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_session(&state, &session_id, &host_id).await;
        insert_loop(&state, &loop_id, &session_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/loops/{loop_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], loop_id);
        assert_eq!(json["status"], "working");
    }

    #[tokio::test]
    async fn get_loop_not_found() {
        let state = test_state().await;
        let loop_id = uuid::Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/loops/{loop_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_loop_invalid_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/loops/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
