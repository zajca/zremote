use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::queries::projects as q;
use zremote_core::worktree_http::worktree_error_response;
use zremote_protocol::ServerMessage;
use zremote_protocol::project::{WorktreeError, WorktreeErrorCode};

use crate::error::{AppError, AppJson};
use crate::state::{
    AppState, MAX_PENDING_WORKTREE_CREATE, PendingWorktreeCreate, WorktreeCreateResponse,
};

use super::crud::ProjectResponse;
use super::parse_project_id;

/// Wall-clock timeout for awaiting the agent's `WorktreeCreateResponse`. The
/// agent bounds its git call with a shorter per-call timeout, but the overall
/// flow (validation + spawn_blocking + hook) can approach that ceiling on
/// large repos. Kept below `WORKTREE_CREATE_STALE_THRESHOLD` (180s) so the
/// pending-entry reaper doesn't race with us.
const WORKTREE_CREATE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

/// `GET /api/projects/:project_id/worktrees` - list worktree children.
pub async fn list_worktrees(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let worktrees = q::list_worktrees(&state.db, &project_id).await?;
    Ok(Json(worktrees))
}

/// Request body for creating a worktree.
#[derive(Debug, Deserialize)]
pub struct CreateWorktreeRequest {
    pub branch: String,
    pub path: Option<String>,
    pub new_branch: Option<bool>,
    /// Optional base ref (commit SHA, branch, or tag). Only meaningful when
    /// `new_branch` is `true`; ignored otherwise. Forwarded to the agent.
    #[serde(default)]
    pub base_ref: Option<String>,
}

/// `POST /api/projects/:project_id/worktrees` - synchronous worktree create
/// proxied through the agent's WebSocket. Mirrors local-mode's response
/// body: `201 Created` with the full `ProjectResponse` (plus an optional
/// `hook_result` key) on success, structured `WorktreeError` on classified
/// failures, `504` on agent timeout.
pub async fn create_worktree(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<CreateWorktreeRequest>,
) -> Result<axum::response::Response, AppError> {
    create_worktree_with_timeout(state, project_id, body, WORKTREE_CREATE_RESPONSE_TIMEOUT).await
}

/// Inner implementation parameterised by timeout so tests can exercise the
/// 504 path without waiting 2 real minutes. Production callers go through
/// `create_worktree` which supplies `WORKTREE_CREATE_RESPONSE_TIMEOUT`.
pub(crate) async fn create_worktree_with_timeout(
    state: Arc<AppState>,
    project_id: String,
    body: CreateWorktreeRequest,
    response_timeout: Duration,
) -> Result<axum::response::Response, AppError> {
    let project_id = parse_project_id(&project_id)?.to_string();

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    // DoS mitigation (complements future REST auth middleware): refuse new
    // requests once the pending map is saturated so a misbehaving or
    // unauthenticated caller cannot grow it without bound.
    if state.worktree_create_requests.len() >= MAX_PENDING_WORKTREE_CREATE {
        tracing::warn!(
            host_id = %host_id,
            pending = state.worktree_create_requests.len(),
            "worktree_create_requests map saturated, rejecting"
        );
        let err = WorktreeError::new(
            WorktreeErrorCode::Internal,
            "Server temporarily overloaded — try again shortly.",
            "pending-map cap reached",
        );
        return Ok((StatusCode::SERVICE_UNAVAILABLE, Json(err)).into_response());
    }

    let request_id = Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel::<WorktreeCreateResponse>();
    state.worktree_create_requests.insert(
        request_id,
        PendingWorktreeCreate::new(tx, project_path.clone()),
    );

    if let Err(send_err) = sender
        .send(ServerMessage::WorktreeCreateRequest {
            request_id,
            project_path,
            branch: body.branch,
            path: body.path,
            new_branch: body.new_branch.unwrap_or(false),
            base_ref: body.base_ref,
        })
        .await
    {
        state.worktree_create_requests.remove(&request_id);
        tracing::warn!(
            host_id = %host_id,
            request_id = %request_id,
            error = %send_err,
            "failed to send WorktreeCreateRequest to agent (outbound channel closed)"
        );
        return Err(AppError::Conflict(
            "failed to send worktree create request".to_string(),
        ));
    }

    let timeout_result = tokio::time::timeout(response_timeout, rx).await;

    match timeout_result {
        Ok(Ok(WorktreeCreateResponse {
            worktree: Some(payload),
            error: None,
            project_id: Some(new_project_id),
        })) => {
            let project_row = q::get_project(&state.db, &new_project_id).await?;
            let mut body_json = serde_json::to_value(&project_row)
                .map_err(|e| AppError::Internal(format!("serialization error: {e}")))?;
            if let Some(ref hr) = payload.hook_result {
                body_json["hook_result"] = serde_json::json!({
                    "success": hr.success,
                    "output": hr.output,
                    "duration_ms": hr.duration_ms,
                });
            }
            Ok((StatusCode::CREATED, Json(body_json)).into_response())
        }
        Ok(Ok(WorktreeCreateResponse {
            worktree: Some(_),
            error: None,
            project_id: None,
        })) => {
            // Dispatch reported success but could not upsert the DB row. This
            // happens when the parent project was deleted mid-flight or the
            // upsert itself failed. Return a structured `WorktreeError` (same
            // shape as every other error path) so the client's
            // `create_worktree_structured` routes through
            // `WorktreeCreateError::Structured` consistently.
            tracing::error!(
                request_id = %request_id,
                "WorktreeCreateResponse success without project_id — parent missing or upsert failed"
            );
            let err = WorktreeError::new(
                WorktreeErrorCode::Internal,
                "Worktree was created but the server couldn't link it back — refresh to see the new entry.",
                "worktree created but project row upsert failed or parent was missing",
            );
            Ok((StatusCode::INTERNAL_SERVER_ERROR, Json(err)).into_response())
        }
        Ok(Ok(WorktreeCreateResponse {
            error: Some(err), ..
        })) => {
            let (status, body_json) = worktree_error_response(err);
            Ok((status, body_json).into_response())
        }
        Ok(Ok(WorktreeCreateResponse {
            worktree: None,
            error: None,
            ..
        })) => Err(AppError::Internal(
            "agent returned an empty WorktreeCreateResponse (no worktree, no error)".to_string(),
        )),
        Ok(Err(_recv)) => Err(AppError::Internal(
            "worktree create response channel closed".to_string(),
        )),
        Err(_timeout) => {
            state.worktree_create_requests.remove(&request_id);
            tracing::warn!(
                host_id = %host_id,
                request_id = %request_id,
                "WorktreeCreateRequest timed out waiting for agent response"
            );
            let err = WorktreeError::new(
                WorktreeErrorCode::Internal,
                "Worktree creation timed out — the agent may still be working or may have disconnected. Reopen the modal to try again.",
                format!(
                    "agent response timeout after {}s",
                    response_timeout.as_secs()
                ),
            );
            Ok((StatusCode::GATEWAY_TIMEOUT, Json(err)).into_response())
        }
    }
}

/// `DELETE /api/projects/:project_id/worktrees/:worktree_id` - request worktree deletion.
pub async fn delete_worktree(
    State(state): State<Arc<AppState>>,
    Path((project_id, worktree_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let _parsed_wt = parse_project_id(&worktree_id)?;

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let worktree_path = q::get_worktree_path(&state.db, &worktree_id, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("worktree {worktree_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    sender
        .send(ServerMessage::WorktreeDelete {
            project_path,
            worktree_path,
            force: false,
        })
        .await
        .map_err(|_| AppError::Conflict("failed to send worktree delete to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}
