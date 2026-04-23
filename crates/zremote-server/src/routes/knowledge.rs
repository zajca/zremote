use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::queries::knowledge as q;
use zremote_core::queries::projects as pq;
use zremote_protocol::ServerMessage;
use zremote_protocol::knowledge::{
    KnowledgeAgentMessage, KnowledgeServerMessage, SearchTier, ServiceAction,
};

use crate::error::{AppError, AppJson};
use crate::state::AppState;

// Re-export core row types as API response types.
pub type KnowledgeBaseResponse = q::KnowledgeBaseRow;
pub type MemoryResponse = q::MemoryRow;

// --- Request types ---

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub tier: Option<String>,
    pub max_results: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct IndexRequest {
    #[serde(default)]
    pub force_reindex: bool,
}

#[derive(Debug, Deserialize)]
pub struct ExtractRequest {
    pub loop_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ServiceControlRequest {
    pub action: String,
}

#[derive(Debug, Deserialize)]
pub struct MemoriesQuery {
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMemoryRequest {
    pub content: Option<String>,
    pub category: Option<String>,
}

// --- Helpers ---

fn parse_project_id(id: &str) -> Result<Uuid, AppError> {
    id.parse()
        .map_err(|_| AppError::BadRequest(format!("invalid project ID: {id}")))
}

fn parse_host_id(id: &str) -> Result<Uuid, AppError> {
    id.parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {id}")))
}

// --- Endpoints ---

/// `GET /api/projects/{project_id}/knowledge/status` - Get KB status for a project's host.
pub async fn get_status(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<Option<KnowledgeBaseResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, host_id, _path) = pq::get_project_info(&state.db, &project_id).await?;
    let kb = q::get_kb_status(&state.db, &host_id).await?;
    Ok(Json(kb))
}

/// `POST /api/projects/{project_id}/knowledge/index` - Trigger indexing.
pub async fn trigger_index(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<IndexRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, host_id_str, path) = pq::get_project_info(&state.db, &project_id).await?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let msg = ServerMessage::KnowledgeAction(KnowledgeServerMessage::IndexProject {
        project_path: path,
        force_reindex: body.force_reindex,
    });

    sender
        .send(msg)
        .await
        .map_err(|_| AppError::Conflict("failed to send index request to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/{project_id}/knowledge/search` - Semantic search.
pub async fn search(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<SearchRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, host_id_str, path) = pq::get_project_info(&state.db, &project_id).await?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let request_id = Uuid::new_v4();
    let tier = match body.tier.as_deref() {
        Some("l0") => SearchTier::L0,
        Some("l2") => SearchTier::L2,
        _ => SearchTier::L1,
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .knowledge_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

    let msg = ServerMessage::KnowledgeAction(KnowledgeServerMessage::Search {
        project_path: path,
        request_id,
        query: body.query,
        tier,
        max_results: body.max_results,
    });

    sender.send(msg).await.map_err(|_| {
        state.knowledge_requests.remove(&request_id);
        AppError::Conflict("failed to send search request to agent".to_string())
    })?;

    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
        Ok(Ok(KnowledgeAgentMessage::SearchResults {
            results,
            duration_ms,
            ..
        })) => Ok(Json(serde_json::json!({
            "results": results,
            "duration_ms": duration_ms,
        }))),
        Ok(Ok(_)) => Err(AppError::Internal("unexpected response type".to_string())),
        Ok(Err(_)) => Err(AppError::Internal("response channel closed".to_string())),
        Err(_) => {
            state.knowledge_requests.remove(&request_id);
            Err(AppError::Internal(
                "search request timed out after 30s".to_string(),
            ))
        }
    }
}

/// `GET /api/projects/{project_id}/knowledge/memories` - List memories.
pub async fn list_memories(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    Query(query): Query<MemoriesQuery>,
) -> Result<Json<Vec<MemoryResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let memories = q::list_memories(&state.db, &project_id, query.category.as_deref()).await?;
    Ok(Json(memories))
}

/// `POST /api/projects/{project_id}/knowledge/extract` - Trigger memory extraction from a loop.
pub async fn extract_memories(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<ExtractRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, host_id_str, path) = pq::get_project_info(&state.db, &project_id).await?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let loop_id: Uuid = body
        .loop_id
        .parse()
        .map_err(|_| AppError::BadRequest("invalid loop_id".to_string()))?;

    // Transcript storage was removed; extraction is no longer supported from stored data.
    let transcript = Vec::<zremote_protocol::knowledge::TranscriptFragment>::new();

    if transcript.is_empty() {
        return Err(AppError::NotFound(
            "no transcript entries for this loop".to_string(),
        ));
    }

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let msg = ServerMessage::KnowledgeAction(KnowledgeServerMessage::ExtractMemory {
        loop_id,
        project_path: path,
        transcript,
    });

    sender
        .send(msg)
        .await
        .map_err(|_| AppError::Conflict("failed to send extract request to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/{project_id}/knowledge/generate-instructions` - Generate CLAUDE.md content.
pub async fn generate_instructions(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, host_id_str, path) = pq::get_project_info(&state.db, &project_id).await?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let request_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("instructions:{host_id_str}:{path}").as_bytes(),
    );

    state.knowledge_requests.remove(&request_id);

    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .knowledge_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

    let msg = ServerMessage::KnowledgeAction(KnowledgeServerMessage::GenerateInstructions {
        project_path: path,
    });

    sender.send(msg).await.map_err(|_| {
        state.knowledge_requests.remove(&request_id);
        AppError::Conflict("failed to send generate request to agent".to_string())
    })?;

    match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
        Ok(Ok(KnowledgeAgentMessage::InstructionsGenerated {
            content,
            memories_used,
            ..
        })) => Ok(Json(serde_json::json!({
            "content": content,
            "memories_used": memories_used,
        }))),
        Ok(Ok(_)) => Err(AppError::Internal("unexpected response type".to_string())),
        Ok(Err(_)) => Err(AppError::Internal("response channel closed".to_string())),
        Err(_) => {
            state.knowledge_requests.remove(&request_id);
            Err(AppError::Internal(
                "instruction generation timed out after 60s".to_string(),
            ))
        }
    }
}

/// `POST /api/hosts/{host_id}/knowledge/service` - Control OV service.
pub async fn control_service(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
    AppJson(body): AppJson<ServiceControlRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed = parse_host_id(&host_id)?;

    let action = match body.action.as_str() {
        "start" => ServiceAction::Start,
        "stop" => ServiceAction::Stop,
        "restart" => ServiceAction::Restart,
        other => {
            return Err(AppError::BadRequest(format!(
                "invalid action: {other}, must be start/stop/restart"
            )));
        }
    };

    let sender = state
        .connections
        .get_sender(&parsed)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let msg = ServerMessage::KnowledgeAction(KnowledgeServerMessage::ServiceControl { action });

    sender
        .send(msg)
        .await
        .map_err(|_| AppError::Conflict("failed to send service control to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /api/projects/{project_id}/knowledge/memories/{memory_id}` - Delete a memory.
pub async fn delete_memory(
    State(state): State<Arc<AppState>>,
    Path((project_id, memory_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let rows = q::delete_memory(&state.db, &memory_id, &project_id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!("memory {memory_id} not found")));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/projects/{project_id}/knowledge/memories/{memory_id}` - Update a memory.
pub async fn update_memory(
    State(state): State<Arc<AppState>>,
    Path((project_id, memory_id)): Path<(String, String)>,
    AppJson(body): AppJson<UpdateMemoryRequest>,
) -> Result<Json<MemoryResponse>, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let now = chrono::Utc::now().to_rfc3339();

    if let Some(ref content) = body.content {
        q::update_memory_content(&state.db, &memory_id, &project_id, content, &now).await?;
    }

    if let Some(ref category) = body.category {
        q::update_memory_category(&state.db, &memory_id, &project_id, category, &now).await?;
    }

    let memory = q::get_memory(&state.db, &memory_id, &project_id).await?;
    Ok(Json(memory))
}

/// `POST /api/projects/{project_id}/knowledge/write-claude-md` - Write knowledge to CLAUDE.md.
pub async fn write_claude_md(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, host_id_str, path) = pq::get_project_info(&state.db, &project_id).await?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let gen_request_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("instructions:{host_id_str}:{path}").as_bytes(),
    );
    state.knowledge_requests.remove(&gen_request_id);
    let (gen_tx, gen_rx) = tokio::sync::oneshot::channel();
    state
        .knowledge_requests
        .insert(gen_request_id, crate::state::PendingRequest::new(gen_tx));

    let gen_msg = ServerMessage::KnowledgeAction(KnowledgeServerMessage::GenerateInstructions {
        project_path: path.clone(),
    });
    sender.send(gen_msg).await.map_err(|_| {
        state.knowledge_requests.remove(&gen_request_id);
        AppError::Conflict("failed to send generate request to agent".to_string())
    })?;

    let content = match tokio::time::timeout(std::time::Duration::from_secs(60), gen_rx).await {
        Ok(Ok(KnowledgeAgentMessage::InstructionsGenerated { content, .. })) => content,
        Ok(Ok(_)) => return Err(AppError::Internal("unexpected response type".to_string())),
        Ok(Err(_)) => return Err(AppError::Internal("response channel closed".to_string())),
        Err(_) => {
            state.knowledge_requests.remove(&gen_request_id);
            return Err(AppError::Internal(
                "instruction generation timed out after 60s".to_string(),
            ));
        }
    };

    let write_request_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("write-claude-md:{host_id_str}:{path}").as_bytes(),
    );
    state.knowledge_requests.remove(&write_request_id);
    let (write_tx, write_rx) = tokio::sync::oneshot::channel();
    state.knowledge_requests.insert(
        write_request_id,
        crate::state::PendingRequest::new(write_tx),
    );

    let write_msg = ServerMessage::KnowledgeAction(KnowledgeServerMessage::WriteClaudeMd {
        project_path: path,
        content,
        mode: zremote_protocol::knowledge::WriteMdMode::Section,
    });
    sender.send(write_msg).await.map_err(|_| {
        state.knowledge_requests.remove(&write_request_id);
        AppError::Conflict("failed to send write request to agent".to_string())
    })?;

    match tokio::time::timeout(std::time::Duration::from_secs(10), write_rx).await {
        Ok(Ok(KnowledgeAgentMessage::ClaudeMdWritten {
            bytes_written,
            error,
            ..
        })) => {
            if let Some(err) = error {
                Err(AppError::Internal(format!(
                    "failed to write CLAUDE.md: {err}"
                )))
            } else {
                Ok(Json(serde_json::json!({
                    "written": true,
                    "bytes": bytes_written,
                })))
            }
        }
        Ok(Ok(_)) => Err(AppError::Internal("unexpected response type".to_string())),
        Ok(Err(_)) => Err(AppError::Internal("response channel closed".to_string())),
        Err(_) => {
            state.knowledge_requests.remove(&write_request_id);
            Err(AppError::Internal(
                "write CLAUDE.md timed out after 10s".to_string(),
            ))
        }
    }
}

/// `POST /api/projects/{project_id}/knowledge/bootstrap` - Bootstrap knowledge for a project.
pub async fn bootstrap_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, host_id_str, path) = pq::get_project_info(&state.db, &project_id).await?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let msg = ServerMessage::KnowledgeAction(KnowledgeServerMessage::BootstrapProject {
        project_path: path,
        existing_claude_md: None,
    });

    sender
        .send(msg)
        .await
        .map_err(|_| AppError::Conflict("failed to send bootstrap request to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{delete, get, post};
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
            .route(
                "/api/projects/{project_id}/knowledge/status",
                get(get_status),
            )
            .route(
                "/api/projects/{project_id}/knowledge/index",
                post(trigger_index),
            )
            .route("/api/projects/{project_id}/knowledge/search", post(search))
            .route(
                "/api/projects/{project_id}/knowledge/memories",
                get(list_memories),
            )
            .route(
                "/api/projects/{project_id}/knowledge/memories/{memory_id}",
                delete(delete_memory).put(update_memory),
            )
            .route(
                "/api/projects/{project_id}/knowledge/extract",
                post(extract_memories),
            )
            .route(
                "/api/projects/{project_id}/knowledge/generate-instructions",
                post(generate_instructions),
            )
            .route(
                "/api/projects/{project_id}/knowledge/write-claude-md",
                post(write_claude_md),
            )
            .route(
                "/api/projects/{project_id}/knowledge/bootstrap",
                post(bootstrap_project),
            )
            .route(
                "/api/hosts/{host_id}/knowledge/service",
                post(control_service),
            )
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

    async fn insert_project(state: &AppState, project_id: &str, host_id: &str, path: &str) {
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name) VALUES (?, ?, ?, 'test-project')",
        )
        .bind(project_id)
        .bind(host_id)
        .bind(path)
        .execute(&state.db)
        .await
        .unwrap();
    }

    async fn insert_memory(state: &AppState, memory_id: &str, project_id: &str) {
        sqlx::query(
            "INSERT INTO knowledge_memories (id, project_id, key, content, category, confidence, created_at, updated_at) \
             VALUES (?, ?, 'test-key', 'test content', 'pattern', 0.9, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(memory_id)
        .bind(project_id)
        .execute(&state.db)
        .await
        .unwrap();
    }

    /// Register a host connection so that `get_sender` returns a sender.
    async fn register_host_connection(
        state: &AppState,
        host_id: Uuid,
    ) -> tokio::sync::mpsc::Receiver<ServerMessage> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "test-host".to_string(), tx, false)
            .await;
        rx
    }

    // --- get_status tests ---

    #[tokio::test]
    async fn get_status_no_kb() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/home/user/proj").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/projects/{project_id}/knowledge/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.is_null());
    }

    #[tokio::test]
    async fn get_status_invalid_project_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/projects/not-a-uuid/knowledge/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_status_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/projects/{project_id}/knowledge/status"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- list_memories tests ---

    #[tokio::test]
    async fn list_memories_empty() {
        let state = test_state().await;
        let project_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/projects/{project_id}/knowledge/memories"))
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
    async fn list_memories_with_data() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;
        insert_memory(&state, &Uuid::new_v4().to_string(), &project_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/projects/{project_id}/knowledge/memories"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["category"], "pattern");
    }

    #[tokio::test]
    async fn list_memories_with_category_filter() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;
        insert_memory(&state, &Uuid::new_v4().to_string(), &project_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!(
                    "/api/projects/{project_id}/knowledge/memories?category=nonexistent"
                ))
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
    async fn list_memories_invalid_project_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/projects/not-uuid/knowledge/memories")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // --- delete_memory tests ---

    #[tokio::test]
    async fn delete_memory_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        let memory_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;
        insert_memory(&state, &memory_id, &project_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories/{memory_id}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_memory_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4().to_string();
        let memory_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories/{memory_id}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_memory_invalid_project_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/projects/not-uuid/knowledge/memories/some-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // --- update_memory tests ---

    #[tokio::test]
    async fn update_memory_content() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        let memory_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;
        insert_memory(&state, &memory_id, &project_id).await;

        let body = serde_json::json!({ "content": "updated content" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories/{memory_id}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(json["content"], "updated content");
    }

    #[tokio::test]
    async fn update_memory_category() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        let memory_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;
        insert_memory(&state, &memory_id, &project_id).await;

        let body = serde_json::json!({ "category": "convention" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories/{memory_id}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(json["category"], "convention");
    }

    #[tokio::test]
    async fn update_memory_invalid_project_id() {
        let state = test_state().await;
        let body = serde_json::json!({ "content": "x" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/projects/not-uuid/knowledge/memories/mem-id")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // --- trigger_index tests ---

    #[tokio::test]
    async fn trigger_index_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;

        let body = serde_json::json!({ "force_reindex": false });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/index"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn trigger_index_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id.to_string()).await;
        insert_project(&state, &project_id, &host_id.to_string(), "/proj").await;
        let mut _rx = register_host_connection(&state, host_id).await;

        let body = serde_json::json!({ "force_reindex": true });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/index"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn trigger_index_invalid_project_id() {
        let state = test_state().await;
        let body = serde_json::json!({ "force_reindex": false });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-uuid/knowledge/index")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // --- control_service tests ---

    #[tokio::test]
    async fn control_service_invalid_action() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        insert_host(&state, &host_id.to_string()).await;
        let mut _rx = register_host_connection(&state, host_id).await;

        let body = serde_json::json!({ "action": "invalid" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/knowledge/service"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn control_service_invalid_host_id() {
        let state = test_state().await;
        let body = serde_json::json!({ "action": "start" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/hosts/not-uuid/knowledge/service")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn control_service_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        insert_host(&state, &host_id.to_string()).await;

        let body = serde_json::json!({ "action": "start" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/knowledge/service"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn control_service_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        insert_host(&state, &host_id.to_string()).await;
        let mut _rx = register_host_connection(&state, host_id).await;

        for action in &["start", "stop", "restart"] {
            let body = serde_json::json!({ "action": action });
            let app = build_router(Arc::clone(&state));
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/hosts/{host_id}/knowledge/service"))
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::ACCEPTED,
                "failed for action: {action}"
            );
        }
    }

    // --- bootstrap_project tests ---

    #[tokio::test]
    async fn bootstrap_project_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/bootstrap"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn bootstrap_project_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id.to_string()).await;
        insert_project(&state, &project_id, &host_id.to_string(), "/proj").await;
        let mut _rx = register_host_connection(&state, host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/bootstrap"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn bootstrap_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/bootstrap"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- extract_memories tests ---

    #[tokio::test]
    async fn extract_memories_invalid_project() {
        let state = test_state().await;
        let body = serde_json::json!({ "loop_id": Uuid::new_v4().to_string() });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-uuid/knowledge/extract")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn extract_memories_invalid_loop_id() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;

        let body = serde_json::json!({ "loop_id": "not-a-uuid" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/extract"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn extract_memories_no_transcript() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        let loop_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;

        let body = serde_json::json!({ "loop_id": loop_id });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/extract"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- search tests ---

    #[tokio::test]
    async fn search_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;

        let body = serde_json::json!({ "query": "test query" });
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/search"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // --- generate_instructions tests ---

    #[tokio::test]
    async fn generate_instructions_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/generate-instructions"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // --- write_claude_md tests ---

    #[tokio::test]
    async fn write_claude_md_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let project_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &project_id, &host_id, "/proj").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/write-claude-md"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }
}
