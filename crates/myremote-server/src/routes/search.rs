use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use myremote_core::queries::search as q;
use serde::{Deserialize, Serialize};

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
             VALUES ('h1', 'test', 'test-host', 'hash', 'online')"
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO sessions (id, host_id, status) VALUES ('s1', 'h1', 'active')"
        )
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
             VALUES ('l1', 'user', 'Can you fix the bug in the parser?', '2026-03-10T10:01:00Z')"
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    #[tokio::test]
    async fn fts_finds_matching_content() {
        let pool = setup_db().await;

        let results: Vec<(i64,)> = sqlx::query_as(
            "SELECT rowid FROM transcript_fts WHERE content MATCH 'function'"
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn fts_finds_multiple_words() {
        let pool = setup_db().await;

        let results: Vec<(i64,)> = sqlx::query_as(
            "SELECT rowid FROM transcript_fts WHERE content MATCH 'bug parser'"
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn fts_no_match_returns_empty() {
        let pool = setup_db().await;

        let results: Vec<(i64,)> = sqlx::query_as(
            "SELECT rowid FROM transcript_fts WHERE content MATCH 'nonexistent_xyz'"
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn fts_delete_sync() {
        let pool = setup_db().await;

        sqlx::query("DELETE FROM transcript_entries WHERE loop_id = 'l1' AND role = 'user'")
            .execute(&pool)
            .await
            .unwrap();

        let results: Vec<(i64,)> = sqlx::query_as(
            "SELECT rowid FROM transcript_fts WHERE content MATCH 'parser'"
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert!(results.is_empty());
    }
}
