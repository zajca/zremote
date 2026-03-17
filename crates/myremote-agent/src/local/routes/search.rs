use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use myremote_core::error::AppError;
use myremote_core::queries::search as q;
use serde::{Deserialize, Serialize};

use crate::local::state::LocalAppState;

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub host: Option<String>,
    pub project: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<q::SearchResult>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}

/// `GET /api/search/transcripts` - full-text search across transcripts.
pub async fn search_transcripts(
    State(state): State<Arc<LocalAppState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, AppError> {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).min(100);

    let filter = q::SearchFilter {
        q: query.q,
        host: query.host,
        project: query.project,
        from: query.from,
        to: query.to,
        page,
        per_page,
    };

    let output = q::search_transcripts(&state.db, &filter).await?;

    Ok(Json(SearchResponse {
        results: output.results,
        total: output.total,
        page,
        per_page,
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
        let pool = myremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(pool, "test-host".to_string(), host_id, shutdown, false)
    }

    fn build_test_router(state: Arc<LocalAppState>) -> Router {
        Router::new()
            .route("/api/search/transcripts", get(search_transcripts))
            .with_state(state)
    }

    #[tokio::test]
    async fn search_empty_db() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/search/transcripts")
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
        assert_eq!(json["total"], 0);
        assert_eq!(json["page"], 1);
        assert_eq!(json["per_page"], 20);
        assert!(json["results"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_with_fts() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4().to_string();
        let loop_id = Uuid::new_v4().to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, project_path, started_at) \
             VALUES (?, ?, 'claude', 'opus', 'completed', '/home/user/project', '2026-03-10T10:00:00Z')",
        )
        .bind(&loop_id)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) \
             VALUES (?, 'assistant', 'Implementing the search function', '2026-03-10T10:00:00Z')",
        )
        .bind(&loop_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/search/transcripts?q=function")
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
        assert_eq!(json["total"], 1);
        assert_eq!(json["results"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_no_match() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4().to_string();
        let loop_id = Uuid::new_v4().to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) VALUES (?, ?, 'claude', 'completed')",
        )
        .bind(&loop_id)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) \
             VALUES (?, 'assistant', 'Hello world', '2026-03-10T10:00:00Z')",
        )
        .bind(&loop_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/search/transcripts?q=nonexistent_xyz_term")
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
        assert_eq!(json["total"], 0);
    }

    #[tokio::test]
    async fn search_with_pagination() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/search/transcripts?page=2&per_page=5")
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
        assert_eq!(json["page"], 2);
        assert_eq!(json["per_page"], 5);
    }
}
