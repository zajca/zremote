use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::error::{AppError, AppJson};
use zremote_core::queries::knowledge as q;
use zremote_core::queries::projects as pq;
use zremote_protocol::knowledge::{KnowledgeServerMessage, SearchTier, ServiceAction};

use crate::local::state::LocalAppState;

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

/// Send a `KnowledgeServerMessage` to the knowledge manager via its channel.
async fn send_knowledge_msg(
    state: &LocalAppState,
    msg: KnowledgeServerMessage,
) -> Result<(), AppError> {
    let tx = state
        .knowledge_tx
        .as_ref()
        .ok_or_else(|| AppError::Conflict("knowledge service is not configured".to_string()))?;

    tx.send(msg)
        .await
        .map_err(|_| AppError::Internal("knowledge service channel closed".to_string()))
}

// --- Endpoints ---

/// `GET /api/projects/{project_id}/knowledge/status` - Get KB status.
pub async fn get_status(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<Option<KnowledgeBaseResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let host_id = state.host_id.to_string();
    let kb = q::get_kb_status(&state.db, &host_id).await?;
    Ok(Json(kb))
}

/// `POST /api/projects/{project_id}/knowledge/index` - Trigger indexing.
pub async fn trigger_index(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<IndexRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, _host_id, path) = pq::get_project_info(&state.db, &project_id).await?;

    send_knowledge_msg(
        &state,
        KnowledgeServerMessage::IndexProject {
            project_path: path,
            force_reindex: body.force_reindex,
        },
    )
    .await?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/{project_id}/knowledge/search` - Semantic search.
pub async fn search(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<SearchRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, _host_id, path) = pq::get_project_info(&state.db, &project_id).await?;

    let request_id = Uuid::new_v4();
    let tier = match body.tier.as_deref() {
        Some("l0") => SearchTier::L0,
        Some("l2") => SearchTier::L2,
        _ => SearchTier::L1,
    };

    send_knowledge_msg(
        &state,
        KnowledgeServerMessage::Search {
            project_path: path,
            request_id,
            query: body.query,
            tier,
            max_results: body.max_results,
        },
    )
    .await?;

    // In local mode, the knowledge manager processes messages asynchronously.
    // Return accepted; results come via events.
    Ok(StatusCode::ACCEPTED)
}

/// `GET /api/projects/{project_id}/knowledge/memories` - List memories.
pub async fn list_memories(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
    Query(query): Query<MemoriesQuery>,
) -> Result<Json<Vec<MemoryResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let memories = q::list_memories(&state.db, &project_id, query.category.as_deref()).await?;
    Ok(Json(memories))
}

/// `POST /api/projects/{project_id}/knowledge/extract` - Trigger memory extraction.
pub async fn extract_memories(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<ExtractRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, _host_id, path) = pq::get_project_info(&state.db, &project_id).await?;

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

    send_knowledge_msg(
        &state,
        KnowledgeServerMessage::ExtractMemory {
            loop_id,
            project_path: path,
            transcript,
        },
    )
    .await?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/{project_id}/knowledge/generate-instructions` - Generate CLAUDE.md content.
pub async fn generate_instructions(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, _host_id, path) = pq::get_project_info(&state.db, &project_id).await?;

    send_knowledge_msg(
        &state,
        KnowledgeServerMessage::GenerateInstructions { project_path: path },
    )
    .await?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/{project_id}/knowledge/write-claude-md` - Write knowledge to CLAUDE.md.
pub async fn write_claude_md(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, _host_id, path) = pq::get_project_info(&state.db, &project_id).await?;

    send_knowledge_msg(
        &state,
        KnowledgeServerMessage::WriteClaudeMd {
            project_path: path,
            content: String::new(), // Empty content triggers auto-generate
            mode: zremote_protocol::knowledge::WriteMdMode::Section,
        },
    )
    .await?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/{project_id}/knowledge/bootstrap` - Bootstrap knowledge.
pub async fn bootstrap_project(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, _host_id, path) = pq::get_project_info(&state.db, &project_id).await?;

    send_knowledge_msg(
        &state,
        KnowledgeServerMessage::BootstrapProject {
            project_path: path,
            existing_claude_md: None,
        },
    )
    .await?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/{project_id}/knowledge/generate-skills` - Generate skill files.
pub async fn generate_skills(
    State(state): State<Arc<LocalAppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let (_id, _host_id, path) = pq::get_project_info(&state.db, &project_id).await?;

    send_knowledge_msg(
        &state,
        KnowledgeServerMessage::GenerateSkills { project_path: path },
    )
    .await?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/hosts/{host_id}/knowledge/service` - Control OV service.
pub async fn control_service(
    State(state): State<Arc<LocalAppState>>,
    Path(host_id): Path<String>,
    AppJson(body): AppJson<ServiceControlRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_host_id(&host_id)?;

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

    send_knowledge_msg(&state, KnowledgeServerMessage::ServiceControl { action }).await?;

    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /api/projects/{project_id}/knowledge/memories/{memory_id}` - Delete a memory.
pub async fn delete_memory(
    State(state): State<Arc<LocalAppState>>,
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
    State(state): State<Arc<LocalAppState>>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{delete, get, post};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use zremote_core::queries::projects as pq;

    use crate::local::upsert_local_host;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        )
    }

    fn build_test_router(state: Arc<LocalAppState>) -> Router {
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
                "/api/projects/{project_id}/knowledge/generate-skills",
                post(generate_skills),
            )
            .route(
                "/api/projects/{project_id}/knowledge/memories/{memory_id}",
                delete(delete_memory).put(update_memory),
            )
            .route(
                "/api/hosts/{host_id}/knowledge/service",
                post(control_service),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn get_status_empty() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/knowledge/status"))
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
        assert!(json.is_null());
    }

    #[tokio::test]
    async fn get_status_invalid_project() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/not-a-uuid/knowledge/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_memories_empty() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/knowledge/memories"))
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
    async fn index_without_knowledge_service() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/index"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"force_reindex": false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return conflict since knowledge_tx is None
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn control_service_invalid_action() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/knowledge/service"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"action": "invalid"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_memory_not_found() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories/nonexistent"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn extract_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/extract"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "loop_id": Uuid::new_v4().to_string()
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
    async fn control_service_valid_actions_without_knowledge_tx() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        for action in &["start", "stop", "restart"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/hosts/{host_id}/knowledge/service"))
                        .header("content-type", "application/json")
                        .body(Body::from(format!(r#"{{"action": "{action}"}}"#)))
                        .unwrap(),
                )
                .await
                .unwrap();

            // Returns conflict because knowledge_tx is None
            assert_eq!(response.status(), StatusCode::CONFLICT);
        }
    }

    #[tokio::test]
    async fn control_service_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/hosts/not-a-uuid/knowledge/service")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"action": "start"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn search_without_knowledge_service() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/search"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query": "test search"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn search_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/knowledge/search")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query": "test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn search_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/search"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query": "test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn generate_instructions_without_knowledge_service() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
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

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn write_claude_md_without_knowledge_service() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
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

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn bootstrap_project_without_knowledge_service() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/bootstrap"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn generate_skills_without_knowledge_service() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/generate-skills"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn extract_invalid_loop_id() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/extract"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"loop_id": "not-a-uuid"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn extract_empty_transcript() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();
        let loop_id = Uuid::new_v4();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/extract"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "loop_id": loop_id.to_string()
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // No transcript entries => 404
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_memories_with_category_filter() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        // Insert memories directly
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO knowledge_memories (id, project_id, key, content, category, confidence, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&project_id)
        .bind("key1")
        .bind("content1")
        .bind("pattern")
        .bind(0.9)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO knowledge_memories (id, project_id, key, content, category, confidence, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&project_id)
        .bind("key2")
        .bind("content2")
        .bind("convention")
        .bind(0.8)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        // All memories
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/knowledge/memories"))
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
        assert_eq!(json.len(), 2);

        // Filter by category
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories?category=pattern"
                    ))
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
        assert_eq!(json[0]["category"], "pattern");
    }

    #[tokio::test]
    async fn delete_memory_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();
        let memory_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO knowledge_memories (id, project_id, key, content, category, confidence, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&memory_id)
        .bind(&project_id)
        .bind("key1")
        .bind("content1")
        .bind("pattern")
        .bind(0.9)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
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

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn update_memory_content() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();
        let memory_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO knowledge_memories (id, project_id, key, content, category, confidence, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&memory_id)
        .bind(&project_id)
        .bind("key1")
        .bind("old content")
        .bind("pattern")
        .bind(0.9)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories/{memory_id}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"content": "new content"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["content"], "new content");
        assert_eq!(json["category"], "pattern");
    }

    #[tokio::test]
    async fn update_memory_category() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();
        let memory_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO knowledge_memories (id, project_id, key, content, category, confidence, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&memory_id)
        .bind(&project_id)
        .bind("key1")
        .bind("content")
        .bind("pattern")
        .bind(0.9)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories/{memory_id}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"category": "convention"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["category"], "convention");
        assert_eq!(json["content"], "content");
    }

    #[tokio::test]
    async fn update_memory_not_found() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories/nonexistent"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"content": "new"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_memory_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/projects/not-a-uuid/knowledge/memories/some-id")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"content": "new"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_memory_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/projects/not-a-uuid/knowledge/memories/some-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn index_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/knowledge/index")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"force_reindex": false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn index_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/index"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"force_reindex": true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn generate_instructions_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/knowledge/generate-instructions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn generate_instructions_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
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

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn write_claude_md_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
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

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn bootstrap_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/bootstrap"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn generate_skills_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/generate-skills"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_memories_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/not-a-uuid/knowledge/memories")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn search_with_explicit_tiers() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        // l0 tier
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/search"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query": "test", "tier": "l0"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // No knowledge_tx => conflict
        assert_eq!(response.status(), StatusCode::CONFLICT);

        // l2 tier
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/search"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"query": "test", "tier": "l2", "max_results": 5}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        // Default tier (no tier specified)
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/search"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query": "test", "tier": "unknown"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn index_with_force_reindex() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/index"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"force_reindex": true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // No knowledge_tx => conflict
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn extract_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/knowledge/extract")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "loop_id": Uuid::new_v4().to_string()
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn write_claude_md_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/knowledge/write-claude-md")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn bootstrap_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/knowledge/bootstrap")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn generate_skills_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/knowledge/generate-skills")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_status_with_kb_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        // Insert a KB record
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO knowledge_bases (id, host_id, status, openviking_version, updated_at) \
             VALUES (?, ?, 'active', '1.0', ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&host_id)
        .bind(&now)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/knowledge/status"))
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
        assert!(!json.is_null());
        assert_eq!(json["status"], "active");
    }

    #[tokio::test]
    async fn extract_with_no_transcript_storage() {
        // Transcript storage was removed; extract always returns 404.
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();
        let loop_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/knowledge/extract"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({
                            "loop_id": loop_id
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
    async fn list_memories_no_category_returns_all() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        for (key, cat) in &[("k1", "pattern"), ("k2", "convention"), ("k3", "pattern")] {
            sqlx::query(
                "INSERT INTO knowledge_memories (id, project_id, key, content, category, confidence, created_at, updated_at) \
                 VALUES (?, ?, ?, 'c', ?, 0.9, ?, ?)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(&project_id)
            .bind(key)
            .bind(cat)
            .bind(&now)
            .bind(&now)
            .execute(&state.db)
            .await
            .unwrap();
        }

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/knowledge/memories"))
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
        assert_eq!(json.len(), 3);
    }

    #[tokio::test]
    async fn update_memory_content_and_category() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();
        let memory_id = Uuid::new_v4().to_string();

        pq::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO knowledge_memories (id, project_id, key, content, category, confidence, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&memory_id)
        .bind(&project_id)
        .bind("key1")
        .bind("old content")
        .bind("pattern")
        .bind(0.9)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/api/projects/{project_id}/knowledge/memories/{memory_id}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"content": "new content", "category": "convention"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["content"], "new content");
        assert_eq!(json["category"], "convention");
    }
}
