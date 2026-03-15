use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
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

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SearchResult {
    pub transcript_id: i64,
    pub loop_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub tool_name: String,
    pub project_path: Option<String>,
    pub loop_status: String,
    pub model: Option<String>,
    pub estimated_cost_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
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
    let offset = (page - 1) * per_page;

    if query.q.as_ref().is_some_and(|q| q.trim().is_empty()) || query.q.is_none() {
        // No search query: return recent transcript entries with filters
        return search_without_fts(&state, &query, page, per_page, offset).await;
    }

    let search_term = query.q.as_deref().unwrap_or_default();

    let mut sql = String::from(
        "SELECT te.id as transcript_id, te.loop_id, te.role, te.content, te.timestamp, \
         al.tool_name, al.project_path, al.status as loop_status, al.model, al.estimated_cost_usd \
         FROM transcript_fts fts \
         JOIN transcript_entries te ON te.id = fts.rowid \
         JOIN agentic_loops al ON al.id = te.loop_id \
         WHERE transcript_fts MATCH ?"
    );
    let mut binds: Vec<String> = vec![search_term.to_string()];

    apply_filters(&mut sql, &mut binds, &query);

    // Count query
    let count_sql = format!(
        "SELECT COUNT(*) FROM transcript_fts fts \
         JOIN transcript_entries te ON te.id = fts.rowid \
         JOIN agentic_loops al ON al.id = te.loop_id \
         WHERE transcript_fts MATCH ?{}",
        &sql[sql.find(" AND ").map_or(sql.len(), |p| p)..sql.len()]
            .replace(" ORDER BY", "")
    );

    sql.push_str(" ORDER BY te.timestamp DESC LIMIT ? OFFSET ?");
    binds.push(per_page.to_string());
    binds.push(offset.to_string());

    let mut q = sqlx::query_as::<_, SearchResult>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }
    let results = q.fetch_all(&state.db).await?;

    // Get total count
    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
    // Rebind the same params minus LIMIT/OFFSET
    for bind in &binds[..binds.len() - 2] {
        count_q = count_q.bind(bind);
    }
    let total = count_q.fetch_one(&state.db).await.unwrap_or(0);

    Ok(Json(SearchResponse {
        results,
        total,
        page,
        per_page,
    }))
}

async fn search_without_fts(
    state: &AppState,
    query: &SearchQuery,
    page: u32,
    per_page: u32,
    offset: u32,
) -> Result<Json<SearchResponse>, AppError> {
    let mut sql = String::from(
        "SELECT te.id as transcript_id, te.loop_id, te.role, te.content, te.timestamp, \
         al.tool_name, al.project_path, al.status as loop_status, al.model, al.estimated_cost_usd \
         FROM transcript_entries te \
         JOIN agentic_loops al ON al.id = te.loop_id \
         WHERE 1=1"
    );
    let mut binds: Vec<String> = Vec::new();

    apply_filters(&mut sql, &mut binds, query);

    let count_sql = sql.replace(
        "SELECT te.id as transcript_id, te.loop_id, te.role, te.content, te.timestamp, \
         al.tool_name, al.project_path, al.status as loop_status, al.model, al.estimated_cost_usd",
        "SELECT COUNT(*)",
    );

    sql.push_str(" ORDER BY te.timestamp DESC LIMIT ? OFFSET ?");
    binds.push(per_page.to_string());
    binds.push(offset.to_string());

    let mut q = sqlx::query_as::<_, SearchResult>(&sql);
    for bind in &binds {
        q = q.bind(bind);
    }
    let results = q.fetch_all(&state.db).await?;

    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
    for bind in &binds[..binds.len() - 2] {
        count_q = count_q.bind(bind);
    }
    let total = count_q.fetch_one(&state.db).await.unwrap_or(0);

    Ok(Json(SearchResponse {
        results,
        total,
        page,
        per_page,
    }))
}

fn apply_filters(sql: &mut String, binds: &mut Vec<String>, query: &SearchQuery) {
    if let Some(ref host) = query.host {
        sql.push_str(
            " AND al.session_id IN (SELECT s.id FROM sessions s JOIN hosts h ON h.id = s.host_id WHERE h.hostname = ?)"
        );
        binds.push(host.clone());
    }
    if let Some(ref project) = query.project {
        sql.push_str(" AND al.project_path = ?");
        binds.push(project.clone());
    }
    if let Some(ref from) = query.from {
        sql.push_str(" AND te.timestamp >= ?");
        binds.push(from.clone());
    }
    if let Some(ref to) = query.to {
        sql.push_str(" AND te.timestamp <= ?");
        binds.push(to.clone());
    }
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

        // Delete a transcript entry
        sqlx::query("DELETE FROM transcript_entries WHERE loop_id = 'l1' AND role = 'user'")
            .execute(&pool)
            .await
            .unwrap();

        // The FTS entry should also be removed
        let results: Vec<(i64,)> = sqlx::query_as(
            "SELECT rowid FROM transcript_fts WHERE content MATCH 'parser'"
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        assert!(results.is_empty());
    }
}
