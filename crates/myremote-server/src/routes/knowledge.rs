use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_core::queries::knowledge as q;
use myremote_core::queries::projects as pq;
use myremote_protocol::ServerMessage;
use myremote_protocol::knowledge::{KnowledgeAgentMessage, KnowledgeServerMessage, SearchTier, ServiceAction};
use serde::Deserialize;
use uuid::Uuid;

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
    state.knowledge_requests.insert(request_id, tx);

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

    let transcript_rows = q::get_transcript_for_loop(&state.db, &body.loop_id).await?;

    if transcript_rows.is_empty() {
        return Err(AppError::NotFound(
            "no transcript entries for this loop".to_string(),
        ));
    }

    let transcript: Vec<myremote_protocol::knowledge::TranscriptFragment> = transcript_rows
        .into_iter()
        .map(|(role, content, timestamp)| {
            myremote_protocol::knowledge::TranscriptFragment {
                role,
                content,
                timestamp: timestamp
                    .parse()
                    .unwrap_or_else(|_| chrono::Utc::now()),
            }
        })
        .collect();

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
    state.knowledge_requests.insert(request_id, tx);

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
        return Err(AppError::NotFound(format!(
            "memory {memory_id} not found"
        )));
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
    state.knowledge_requests.insert(gen_request_id, gen_tx);

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
            return Err(AppError::Internal("instruction generation timed out after 60s".to_string()));
        }
    };

    let write_request_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("write-claude-md:{host_id_str}:{path}").as_bytes(),
    );
    state.knowledge_requests.remove(&write_request_id);
    let (write_tx, write_rx) = tokio::sync::oneshot::channel();
    state.knowledge_requests.insert(write_request_id, write_tx);

    let write_msg = ServerMessage::KnowledgeAction(KnowledgeServerMessage::WriteClaudeMd {
        project_path: path,
        content,
        mode: myremote_protocol::knowledge::WriteMdMode::Section,
    });
    sender.send(write_msg).await.map_err(|_| {
        state.knowledge_requests.remove(&write_request_id);
        AppError::Conflict("failed to send write request to agent".to_string())
    })?;

    match tokio::time::timeout(std::time::Duration::from_secs(10), write_rx).await {
        Ok(Ok(KnowledgeAgentMessage::ClaudeMdWritten { bytes_written, error, .. })) => {
            if let Some(err) = error {
                Err(AppError::Internal(format!("failed to write CLAUDE.md: {err}")))
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
            Err(AppError::Internal("write CLAUDE.md timed out after 10s".to_string()))
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
