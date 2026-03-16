use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use myremote_core::error::AppError;
use myremote_core::queries::loops as q;
use myremote_protocol::agentic::{AgenticStatus, UserAction};
use serde::{Deserialize, Serialize};

use crate::local::state::LocalAppState;

/// Query parameters for listing agentic loops.
#[derive(Debug, Deserialize)]
pub struct ListLoopsQuery {
    pub status: Option<String>,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
}

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
    State(state): State<Arc<LocalAppState>>,
    Query(query): Query<ListLoopsQuery>,
) -> Result<Json<Vec<q::LoopRow>>, AppError> {
    let filter = q::ListLoopsFilter {
        status: query.status,
        host_id: Some(state.host_id.to_string()),
        session_id: query.session_id,
        project_id: query.project_id,
    };
    let loops = q::list_loops(&state.db, &filter).await?;
    Ok(Json(loops))
}

/// `GET /api/loops/:id` - get loop detail.
pub async fn get_loop(
    State(state): State<Arc<LocalAppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<q::LoopRow>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let row = q::get_loop(&state.db, &loop_id).await?;
    Ok(Json(row))
}

/// `GET /api/loops/:id/tools` - tool calls for a loop.
pub async fn get_loop_tools(
    State(state): State<Arc<LocalAppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<Vec<q::ToolCallRow>>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let rows = q::get_loop_tools(&state.db, &loop_id).await?;
    Ok(Json(rows))
}

/// `GET /api/loops/:id/transcript` - transcript entries for a loop.
pub async fn get_loop_transcript(
    State(state): State<Arc<LocalAppState>>,
    Path(loop_id): Path<String>,
) -> Result<Json<Vec<q::TranscriptEntryRow>>, AppError> {
    let _parsed = parse_loop_id(&loop_id)?;
    let rows = q::get_loop_transcript(&state.db, &loop_id).await?;
    Ok(Json(rows))
}

/// `GET /api/loops/:id/metrics` - current metrics for a loop.
pub async fn get_loop_metrics(
    State(state): State<Arc<LocalAppState>>,
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

/// `POST /api/loops/:id/action` - send user action to agentic loop.
pub async fn post_loop_action(
    State(state): State<Arc<LocalAppState>>,
    Path(loop_id): Path<String>,
    Json(body): Json<ActionRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let parsed_loop_id = parse_loop_id(&loop_id)?;

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

    // Get the bytes to write to the PTY from agentic manager
    let result = {
        let mut mgr = state.agentic_manager.lock().await;
        mgr.handle_user_action(&parsed_loop_id, body.action, body.payload.as_deref())
    };

    let Some((session_id, bytes)) = result else {
        return Err(AppError::NotFound(format!(
            "no active agentic loop found for {loop_id}"
        )));
    };

    // Write the action bytes to the session's PTY
    {
        let mut session_mgr = state.session_manager.lock().await;
        session_mgr
            .write_to(&session_id, &bytes)
            .map_err(|e| AppError::Internal(format!("failed to write to session: {e}")))?;
    }

    // Also resolve any pending permission requests for this loop
    state
        .hooks_permission_manager
        .resolve_any_pending(
            parsed_loop_id,
            match body.action {
                UserAction::Approve => {
                    crate::hooks::permission::PermissionDecision::Allow
                }
                UserAction::Reject => {
                    crate::hooks::permission::PermissionDecision::Deny
                }
                _ => crate::hooks::permission::PermissionDecision::Ask,
            },
        )
        .await;

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
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
            .route("/api/loops", get(list_loops))
            .route("/api/loops/{loop_id}", get(get_loop))
            .route("/api/loops/{loop_id}/tools", get(get_loop_tools))
            .route(
                "/api/loops/{loop_id}/transcript",
                get(get_loop_transcript),
            )
            .route("/api/loops/{loop_id}/metrics", get(get_loop_metrics))
            .route("/api/loops/{loop_id}/action", post(post_loop_action))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_loops_empty() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/loops")
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
    async fn list_loops_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4().to_string();
        let loop_id = Uuid::new_v4().to_string();

        // Insert a session and a loop
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) VALUES (?, ?, 'claude-code', 'working')",
        )
        .bind(&loop_id)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/loops")
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
        assert_eq!(json[0]["id"], loop_id);
        assert_eq!(json[0]["tool_name"], "claude-code");
    }

    #[tokio::test]
    async fn list_loops_with_status_filter() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4().to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        // Insert two loops with different statuses
        let loop1 = Uuid::new_v4().to_string();
        let loop2 = Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) VALUES (?, ?, 'claude-code', 'working')",
        )
        .bind(&loop1)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) VALUES (?, ?, 'claude-code', 'completed')",
        )
        .bind(&loop2)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/loops?status=working")
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
        assert_eq!(json[0]["status"], "working");
    }

    #[tokio::test]
    async fn get_loop_found() {
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
            "INSERT INTO agentic_loops (id, session_id, tool_name, status, model) VALUES (?, ?, 'claude-code', 'working', 'sonnet')",
        )
        .bind(&loop_id)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/loops/{loop_id}"))
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
        assert_eq!(json["id"], loop_id);
        assert_eq!(json["model"], "sonnet");
    }

    #[tokio::test]
    async fn get_loop_not_found() {
        let state = test_state().await;
        let loop_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/loops/{loop_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_loop_invalid_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/loops/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_loop_tools_empty() {
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
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) VALUES (?, ?, 'claude-code', 'working')",
        )
        .bind(&loop_id)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/loops/{loop_id}/tools"))
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
    async fn get_loop_tools_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4().to_string();
        let loop_id = Uuid::new_v4().to_string();
        let tool_call_id = Uuid::new_v4().to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) VALUES (?, ?, 'claude-code', 'working')",
        )
        .bind(&loop_id)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO tool_calls (id, loop_id, tool_name, status, arguments_json) VALUES (?, ?, 'Read', 'completed', '{}')",
        )
        .bind(&tool_call_id)
        .bind(&loop_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/loops/{loop_id}/tools"))
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
        assert_eq!(json[0]["tool_name"], "Read");
    }

    #[tokio::test]
    async fn get_loop_transcript_empty() {
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
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) VALUES (?, ?, 'claude-code', 'working')",
        )
        .bind(&loop_id)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/loops/{loop_id}/transcript"))
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
    async fn get_loop_transcript_with_data() {
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
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) VALUES (?, ?, 'claude-code', 'working')",
        )
        .bind(&loop_id)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO transcript_entries (loop_id, role, content, timestamp) VALUES (?, 'assistant', 'Hello', '2026-01-01T00:00:00Z')",
        )
        .bind(&loop_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/loops/{loop_id}/transcript"))
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
        assert_eq!(json[0]["role"], "assistant");
        assert_eq!(json[0]["content"], "Hello");
    }

    #[tokio::test]
    async fn get_loop_metrics_from_db() {
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
            "INSERT INTO agentic_loops (id, session_id, tool_name, status, total_tokens_in, total_tokens_out, estimated_cost_usd) \
             VALUES (?, ?, 'claude-code', 'completed', 1000, 500, 0.42)",
        )
        .bind(&loop_id)
        .bind(&session_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/loops/{loop_id}/metrics"))
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
        assert_eq!(json["loop_id"], loop_id);
        assert_eq!(json["total_tokens_in"], 1000);
        assert_eq!(json["total_tokens_out"], 500);
        assert_eq!(json["estimated_cost_usd"], 0.42);
        assert_eq!(json["pending_tool_calls"], 0);
    }

    #[tokio::test]
    async fn get_loop_metrics_from_memory() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let session_id = Uuid::new_v4();
        let session_id_str = session_id.to_string();
        let loop_id = Uuid::new_v4();
        let loop_id_str = loop_id.to_string();

        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'active')")
            .bind(&session_id_str)
            .bind(&host_id)
            .execute(&state.db)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, tool_name, status) VALUES (?, ?, 'claude-code', 'working')",
        )
        .bind(&loop_id_str)
        .bind(&session_id_str)
        .execute(&state.db)
        .await
        .unwrap();

        // Insert in-memory state
        state.agentic_loops.insert(
            loop_id,
            myremote_core::state::AgenticLoopState {
                loop_id,
                session_id,
                status: myremote_protocol::agentic::AgenticStatus::Working,
                pending_tool_calls: std::collections::VecDeque::new(),
                tokens_in: 2000,
                tokens_out: 800,
                estimated_cost_usd: 0.55,
                last_updated: tokio::time::Instant::now(),
            },
        );

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/loops/{loop_id}/metrics"))
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
        assert_eq!(json["total_tokens_in"], 2000);
        assert_eq!(json["total_tokens_out"], 800);
        assert_eq!(json["estimated_cost_usd"], 0.55);
        assert_eq!(json["status"], "working");
    }

    #[tokio::test]
    async fn post_action_no_active_loop() {
        let state = test_state().await;
        let loop_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/loops/{loop_id}/action"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "action": "approve"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_action_invalid_loop_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/loops/not-a-uuid/action")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "action": "approve"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
