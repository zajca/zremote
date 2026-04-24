use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use uuid::Uuid;
use zremote_core::queries::projects as q;
use zremote_core::worktree_http::worktree_error_response;
use zremote_protocol::ServerMessage;
use zremote_protocol::project::{WorktreeError, WorktreeErrorCode};

use crate::error::AppError;
use crate::state::{AppState, BranchListResponse, PendingRequest};

use super::parse_project_id;

/// Wall-clock timeout for awaiting the agent's `BranchListResponse`. The agent
/// side bounds its own git call with a shorter timeout; this ceiling is our
/// safety net against a silently-wedged agent that accepted the request but
/// never replies. Kept at or below `BRANCH_LIST_STALE_THRESHOLD` (30s) so the
/// pending-entry reaper doesn't race with us.
const BRANCH_LIST_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// `POST /api/projects/:project_id/git/refresh` - trigger git status refresh.
pub async fn trigger_git_refresh(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (host_id_str, path) = q::get_project_host_and_path(&state.db, &project_id)
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

    sender
        .send(ServerMessage::ProjectGitStatus { path })
        .await
        .map_err(|_| AppError::Conflict("failed to send git refresh to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

/// `GET /api/projects/:project_id/git/branches` — proxy the agent's branch
/// listing through the server's WebSocket. Mirrors the response shape of
/// local-mode's handler: `200 { local, remote, current, remote_truncated }`
/// on success, structured `WorktreeError` JSON on classified failures, or
/// `504` with a structured Internal error on timeout.
pub async fn list_branches(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<axum::response::Response, AppError> {
    list_branches_with_timeout(state, project_id, BRANCH_LIST_RESPONSE_TIMEOUT).await
}

/// Inner implementation parameterised by timeout so tests can exercise the
/// 504 path without waiting 30 real seconds. Production callers go through
/// `list_branches` which supplies `BRANCH_LIST_RESPONSE_TIMEOUT`.
pub(crate) async fn list_branches_with_timeout(
    state: Arc<AppState>,
    project_id: String,
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

    let request_id = Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel::<BranchListResponse>();
    state
        .branch_list_requests
        .insert(request_id, PendingRequest::new(tx));

    if let Err(send_err) = sender
        .send(ServerMessage::BranchListRequest {
            request_id,
            project_path,
        })
        .await
    {
        // Clean up the pending entry so a future request_id collision (or the
        // reaper) doesn't see a stale entry.
        state.branch_list_requests.remove(&request_id);
        tracing::warn!(
            host_id = %host_id,
            request_id = %request_id,
            error = %send_err,
            "failed to send BranchListRequest to agent (outbound channel closed)"
        );
        return Err(AppError::Conflict(
            "failed to send branch list request".to_string(),
        ));
    }

    match tokio::time::timeout(response_timeout, rx).await {
        Ok(Ok(BranchListResponse {
            branches: Some(list),
            error: None,
        })) => Ok(Json(list).into_response()),
        Ok(Ok(BranchListResponse {
            error: Some(err), ..
        })) => {
            let (status, body) = worktree_error_response(err);
            Ok((status, body).into_response())
        }
        Ok(Ok(BranchListResponse {
            branches: None,
            error: None,
        })) => {
            // Agent returned neither branches nor error — treat as an internal
            // protocol violation rather than silently returning an empty list.
            Err(AppError::Internal(
                "agent returned an empty BranchListResponse (no branches, no error)".to_string(),
            ))
        }
        Ok(Err(_recv)) => Err(AppError::Internal(
            "branch list response channel closed".to_string(),
        )),
        Err(_timeout) => {
            // Drop the pending entry so a late reply doesn't fire into a dead
            // oneshot; the reaper would catch it eventually but explicit
            // cleanup keeps the map small under repeated timeouts.
            state.branch_list_requests.remove(&request_id);
            tracing::warn!(
                host_id = %host_id,
                request_id = %request_id,
                "BranchListRequest timed out waiting for agent response"
            );
            let err = WorktreeError::new(
                WorktreeErrorCode::Internal,
                "Branch list timed out — the agent did not respond in 30s.",
                "agent response timeout",
            );
            Ok((StatusCode::GATEWAY_TIMEOUT, Json(err)).into_response())
        }
    }
}
