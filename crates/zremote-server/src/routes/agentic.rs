use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use zremote_core::queries::loops as q;
use zremote_core::state::LoopInfo;
use zremote_protocol::agentic::{AgenticServerMessage, AgenticStatus, UserAction};

use crate::error::{AppError, AppJson};
use crate::state::AppState;

/// Query parameters for listing agentic loops.
#[derive(Debug, Deserialize)]
pub struct ListLoopsQuery {
    pub status: Option<String>,
    pub host_id: Option<String>,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
}

pub type ToolCallResponse = q::ToolCallRow;
pub type TranscriptEntryResponse = q::TranscriptEntryRow;

/// Metrics response for API.
#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    pub loop_id: String,
    pub status: String,
    pub total_tokens_in: i64,
    pub total_tokens_out: i64,
    pub estimated_cost_usd: f64,
    pub pending_tool_calls: usize,
}

/// Request body for user action on a loop.
#[derive(Debug, Deserialize)]
pub struct ActionRequest {
    pub action: UserAction,
    pub payload: Option<String>,
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
    let loops = rows
        .into_iter()
        .map(|r| q::enrich_loop(r, &state.agentic_loops))
        .collect();
    Ok(Json(loops))
}

/// `GET /api/loops/:id` - get loop detail.
pub async fn get_loop(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<LoopInfo>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let row = q::get_loop(&state.db, &loop_id).await?;
    Ok(Json(q::enrich_loop(row, &state.agentic_loops)))
}

/// `GET /api/loops/:id/tools` - tool calls for a loop.
pub async fn get_loop_tools(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<Vec<ToolCallResponse>>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let rows = q::get_loop_tools(&state.db, &loop_id).await?;
    Ok(Json(rows))
}

/// `GET /api/loops/:id/transcript` - transcript entries for a loop.
pub async fn get_loop_transcript(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<Vec<TranscriptEntryResponse>>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let rows = q::get_loop_transcript(&state.db, &loop_id).await?;
    Ok(Json(rows))
}

/// `POST /api/loops/:id/action` - send user action to agentic loop.
pub async fn post_loop_action(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
    AppJson(body): AppJson<ActionRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let parsed_loop_id = parse_loop_id(&loop_id)?;

    let session_id_str = q::get_loop_session_id(&state.db, &loop_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("loop {loop_id} not found")))?;

    let host_id_str = q::get_session_host_id(&state.db, &session_id_str)
        .await?
        .ok_or_else(|| AppError::Internal("session has no host".to_string()))?;

    let parsed_host_id: uuid::Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    // Check loop status
    if let Some(entry) = state.agentic_loops.get(&parsed_loop_id) {
        match entry.status {
            AgenticStatus::Working | AgenticStatus::WaitingForInput | AgenticStatus::Paused => {}
            _ => {
                return Err(AppError::Conflict(
                    "Loop is not in an actionable state".to_string(),
                ));
            }
        }
    }

    let sender = state
        .connections
        .get_sender(&parsed_host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let msg = zremote_protocol::ServerMessage::AgenticAction(AgenticServerMessage::UserAction {
        loop_id: parsed_loop_id,
        action: body.action,
        payload: body.payload,
    });

    sender
        .send(msg)
        .await
        .map_err(|_| AppError::Conflict("failed to send action to agent".to_string()))?;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

/// `GET /api/loops/:id/metrics` - current metrics for a loop.
pub async fn get_loop_metrics(
    State(state): State<Arc<AppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<MetricsResponse>, AppError> {
    let parsed_loop_id = parse_loop_id(&loop_id)?;

    // Try in-memory state first for active loops
    if let Some(entry) = state.agentic_loops.get(&parsed_loop_id) {
        return Ok(Json(MetricsResponse {
            loop_id: loop_id.clone(),
            status: format!("{:?}", entry.status).to_lowercase(),
            total_tokens_in: i64::try_from(entry.tokens_in).unwrap_or(i64::MAX),
            total_tokens_out: i64::try_from(entry.tokens_out).unwrap_or(i64::MAX),
            estimated_cost_usd: entry.estimated_cost_usd,
            pending_tool_calls: entry.pending_tool_calls.len(),
        }));
    }

    // Fall back to DB for ended loops
    let row = q::get_loop(&state.db, &loop_id).await?;

    Ok(Json(MetricsResponse {
        loop_id,
        status: row.status,
        total_tokens_in: row.total_tokens_in.unwrap_or(0),
        total_tokens_out: row.total_tokens_out.unwrap_or(0),
        estimated_cost_usd: row.estimated_cost_usd.unwrap_or(0.0),
        pending_tool_calls: 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use zremote_core::state::AgenticLoopState;
    use zremote_protocol::agentic::AgenticStatus;

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
        })
    }

    fn build_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/loops", get(list_loops))
            .route("/api/loops/{loop_id}", get(get_loop))
            .route("/api/loops/{loop_id}/tools", get(get_loop_tools))
            .route("/api/loops/{loop_id}/transcript", get(get_loop_transcript))
            .route("/api/loops/{loop_id}/action", post(post_loop_action))
            .route("/api/loops/{loop_id}/metrics", get(get_loop_metrics))
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

    async fn insert_tool_call(state: &AppState, tool_id: &str, loop_id: &str) {
        sqlx::query(
            "INSERT INTO tool_calls (id, loop_id, tool_name, status, created_at) VALUES (?, ?, 'Bash', 'completed', '2026-01-01T00:00:01Z')",
        )
        .bind(tool_id)
        .bind(loop_id)
        .execute(&state.db)
        .await
        .unwrap();
    }

    async fn insert_transcript(state: &AppState, loop_id: &str) {
        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) VALUES (?, 'assistant', 'Hello world', '2026-01-01T00:00:00Z')",
        )
        .bind(loop_id)
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

    #[tokio::test]
    async fn get_loop_tools_empty() {
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
                Request::get(format!("/api/loops/{loop_id}/tools"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn get_loop_tools_with_data() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let loop_id = uuid::Uuid::new_v4().to_string();
        let tool_id = uuid::Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_session(&state, &session_id, &host_id).await;
        insert_loop(&state, &loop_id, &session_id).await;
        insert_tool_call(&state, &tool_id, &loop_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/loops/{loop_id}/tools"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["tool_name"], "Bash");
    }

    #[tokio::test]
    async fn get_loop_tools_invalid_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/loops/bad-id/tools")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_loop_transcript_empty() {
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
                Request::get(format!("/api/loops/{loop_id}/transcript"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(json.is_empty());
    }

    #[tokio::test]
    async fn get_loop_transcript_with_data() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let loop_id = uuid::Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_session(&state, &session_id, &host_id).await;
        insert_loop(&state, &loop_id, &session_id).await;
        insert_transcript(&state, &loop_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/loops/{loop_id}/transcript"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["role"], "assistant");
        assert_eq!(json[0]["content"], "Hello world");
    }

    #[tokio::test]
    async fn get_loop_transcript_invalid_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/loops/bad-id/transcript")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_loop_metrics_from_db() {
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
                Request::get(format!("/api/loops/{loop_id}/metrics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["loop_id"], loop_id);
        assert_eq!(json["status"], "working");
        assert_eq!(json["pending_tool_calls"], 0);
    }

    #[tokio::test]
    async fn get_loop_metrics_from_memory() {
        let state = test_state().await;
        let loop_id = uuid::Uuid::new_v4();
        let session_id = uuid::Uuid::new_v4();

        // Insert into in-memory store
        state.agentic_loops.insert(
            loop_id,
            AgenticLoopState {
                loop_id,
                session_id,
                status: AgenticStatus::Working,
                pending_tool_calls: std::collections::VecDeque::new(),
                tokens_in: 500,
                tokens_out: 1000,
                estimated_cost_usd: 0.05,
                context_used: 0,
                context_max: 0,
                last_updated: tokio::time::Instant::now(),
            },
        );

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/loops/{loop_id}/metrics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total_tokens_in"], 500);
        assert_eq!(json["total_tokens_out"], 1000);
    }

    #[tokio::test]
    async fn get_loop_metrics_invalid_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/loops/bad-id/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_loop_metrics_not_found() {
        let state = test_state().await;
        let loop_id = uuid::Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/loops/{loop_id}/metrics"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_loop_action_invalid_loop_id() {
        let state = test_state().await;
        let body = serde_json::json!({
            "action": "stop",
        });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/loops/bad-id/action")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_loop_action_loop_not_found() {
        let state = test_state().await;
        let loop_id = uuid::Uuid::new_v4().to_string();
        let body = serde_json::json!({
            "action": "stop",
        });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/loops/{loop_id}/action"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_loop_action_host_offline() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let loop_id = uuid::Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_session(&state, &session_id, &host_id).await;
        insert_loop(&state, &loop_id, &session_id).await;

        let body = serde_json::json!({
            "action": "stop",
        });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/loops/{loop_id}/action"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Host is not in connections manager -> conflict (offline)
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn post_loop_action_non_actionable_state() {
        let state = test_state().await;
        let host_id = uuid::Uuid::new_v4();
        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();

        insert_host(&state, &host_id.to_string()).await;
        insert_session(&state, &session_id.to_string(), &host_id.to_string()).await;
        insert_loop(&state, &loop_id.to_string(), &session_id.to_string()).await;

        // Insert loop into memory with "completed" status
        state.agentic_loops.insert(
            loop_id,
            AgenticLoopState {
                loop_id,
                session_id,
                status: AgenticStatus::Completed,
                pending_tool_calls: std::collections::VecDeque::new(),
                tokens_in: 0,
                tokens_out: 0,
                estimated_cost_usd: 0.0,
                context_used: 0,
                context_max: 0,
                last_updated: tokio::time::Instant::now(),
            },
        );

        let body = serde_json::json!({
            "action": "stop",
        });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/loops/{loop_id}/action"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }
}
