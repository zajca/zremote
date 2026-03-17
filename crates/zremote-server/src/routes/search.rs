use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};
use zremote_core::queries::search as q;

use crate::error::AppError;
use crate::state::AppState;

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
    State(state): State<Arc<AppState>>,
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
    use crate::db;

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

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, project_path, started_at) \
             VALUES ('l1', 's1', 'claude', 'opus', 'completed', '/home/user/project', '2026-03-10T10:00:00Z')"
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) \
             VALUES ('l1', 'assistant', 'Implementing the search function for the project', '2026-03-10T10:00:00Z')"
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) \
             VALUES ('l1', 'user', 'Can you fix the bug in the parser?', '2026-03-10T10:01:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    #[tokio::test]
    async fn fts_finds_matching_content() {
        let pool = setup_db().await;

        let results: Vec<(i64,)> =
            sqlx::query_as("SELECT rowid FROM transcript_fts WHERE content MATCH 'function'")
                .fetch_all(&pool)
                .await
                .unwrap();

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn fts_finds_multiple_words() {
        let pool = setup_db().await;

        let results: Vec<(i64,)> =
            sqlx::query_as("SELECT rowid FROM transcript_fts WHERE content MATCH 'bug parser'")
                .fetch_all(&pool)
                .await
                .unwrap();

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn fts_no_match_returns_empty() {
        let pool = setup_db().await;

        let results: Vec<(i64,)> = sqlx::query_as(
            "SELECT rowid FROM transcript_fts WHERE content MATCH 'nonexistent_xyz'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert!(results.is_empty());
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
        })
    }

    async fn seed_search_data(pool: &sqlx::SqlitePool) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('h1', 'test', 'test-host', 'hash', 'online')",
        )
        .execute(pool)
        .await
        .unwrap();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES ('s1', 'h1', 'active')")
            .execute(pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, model, status, project_path, started_at) \
             VALUES ('l1', 's1', 'claude', 'opus', 'completed', '/home/user/project', '2026-03-10T10:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) \
             VALUES ('l1', 'assistant', 'Implementing the search function for the project', '2026-03-10T10:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) \
             VALUES ('l1', 'user', 'Can you fix the bug in the parser?', '2026-03-10T10:01:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn search_transcripts_route_with_query() {
        let state = test_state().await;
        seed_search_data(&state.db).await;

        let app = crate::create_router(state);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/search/transcripts?q=function")
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
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["total"], 1);
        assert_eq!(resp["results"].as_array().unwrap().len(), 1);
        assert_eq!(resp["page"], 1);
        assert_eq!(resp["per_page"], 20);
    }

    #[tokio::test]
    async fn search_transcripts_route_no_results() {
        let state = test_state().await;
        seed_search_data(&state.db).await;

        let app = crate::create_router(state);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/search/transcripts?q=nonexistent_xyz")
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
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["total"], 0);
        assert!(resp["results"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_transcripts_empty_query_returns_all() {
        let state = test_state().await;
        seed_search_data(&state.db).await;

        let app = crate::create_router(state);
        // Empty q= triggers search_without_fts which returns all transcripts
        let response = app
            .oneshot(
                axum::http::Request::get("/api/search/transcripts?q=")
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
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["total"], 2);
    }

    #[tokio::test]
    async fn search_transcripts_no_query_param_returns_all() {
        let state = test_state().await;
        seed_search_data(&state.db).await;

        let app = crate::create_router(state);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/search/transcripts")
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
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["total"], 2);
    }

    #[tokio::test]
    async fn search_transcripts_with_pagination() {
        let state = test_state().await;
        seed_search_data(&state.db).await;

        let app = crate::create_router(state);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/search/transcripts?per_page=1&page=1")
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
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["results"].as_array().unwrap().len(), 1);
        assert_eq!(resp["total"], 2);
        assert_eq!(resp["per_page"], 1);
    }

    #[tokio::test]
    async fn search_transcripts_with_host_filter() {
        let state = test_state().await;
        seed_search_data(&state.db).await;

        let app = crate::create_router(state);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/search/transcripts?host=test-host")
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
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["total"], 2);
    }

    #[tokio::test]
    async fn search_transcripts_with_nonexistent_host_filter() {
        let state = test_state().await;
        seed_search_data(&state.db).await;

        let app = crate::create_router(state);
        let response = app
            .oneshot(
                axum::http::Request::get("/api/search/transcripts?host=nonexistent")
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
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["total"], 0);
    }

    #[tokio::test]
    async fn fts_delete_sync() {
        let pool = setup_db().await;

        sqlx::query("DELETE FROM transcript_entries WHERE loop_id = 'l1' AND role = 'user'")
            .execute(&pool)
            .await
            .unwrap();

        let results: Vec<(i64,)> =
            sqlx::query_as("SELECT rowid FROM transcript_fts WHERE content MATCH 'parser'")
                .fetch_all(&pool)
                .await
                .unwrap();

        assert!(results.is_empty());
    }
}
