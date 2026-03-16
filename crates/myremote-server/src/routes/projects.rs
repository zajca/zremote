use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_protocol::ServerMessage;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AppError, AppJson};
use crate::routes::sessions::SessionResponse;
use crate::state::AppState;

/// Project representation for API responses.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProjectResponse {
    pub id: String,
    pub host_id: String,
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
    pub project_type: String,
    pub created_at: String,
    pub parent_project_id: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit_hash: Option<String>,
    pub git_commit_message: Option<String>,
    #[serde(default)]
    pub git_is_dirty: bool,
    #[serde(default)]
    pub git_ahead: i32,
    #[serde(default)]
    pub git_behind: i32,
    pub git_remotes: Option<String>,
    pub git_updated_at: Option<String>,
}

/// Request body for manually adding a project.
#[derive(Debug, Deserialize)]
pub struct AddProjectRequest {
    pub path: String,
}

fn parse_host_id(host_id: &str) -> Result<Uuid, AppError> {
    host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))
}

fn parse_project_id(project_id: &str) -> Result<Uuid, AppError> {
    project_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid project ID: {project_id}")))
}

/// `GET /api/hosts/:host_id/projects` - list projects for a host.
pub async fn list_projects(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let _parsed = parse_host_id(&host_id)?;

    let projects: Vec<ProjectResponse> = sqlx::query_as(
        "SELECT id, host_id, path, name, has_claude_config, project_type, created_at, \
         parent_project_id, git_branch, git_commit_hash, git_commit_message, \
         git_is_dirty, git_ahead, git_behind, git_remotes, git_updated_at \
         FROM projects WHERE host_id = ? ORDER BY name",
    )
    .bind(&host_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(projects))
}

/// `POST /api/hosts/:host_id/projects/scan` - trigger project scan on agent.
pub async fn trigger_scan(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let parsed = parse_host_id(&host_id)?;

    let sender = state
        .connections
        .get_sender(&parsed)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    sender
        .send(ServerMessage::ProjectScan)
        .await
        .map_err(|_| AppError::Conflict("failed to send scan request to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/hosts/:host_id/projects` - manually add a project.
pub async fn add_project(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
    AppJson(body): AppJson<AddProjectRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed = parse_host_id(&host_id)?;

    if body.path.is_empty() {
        return Err(AppError::BadRequest("path must not be empty".to_string()));
    }

    // Check host exists
    let host_exists: Option<(String,)> = sqlx::query_as("SELECT id FROM hosts WHERE id = ?")
        .bind(&host_id)
        .fetch_optional(&state.db)
        .await?;

    if host_exists.is_none() {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    // Send ProjectRegister to agent to validate and discover project info
    if let Some(sender) = state.connections.get_sender(&parsed).await {
        let _ = sender
            .send(ServerMessage::ProjectRegister {
                path: body.path.clone(),
            })
            .await;
    }

    // Insert into DB (agent will send ProjectDiscovered to update details)
    let project_id = Uuid::new_v4().to_string();
    let name = body
        .path
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string();

    sqlx::query(
        "INSERT OR IGNORE INTO projects (id, host_id, path, name) VALUES (?, ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(&host_id)
    .bind(&body.path)
    .bind(&name)
    .execute(&state.db)
    .await?;

    // Return the project (may be existing if path was already registered)
    let project: ProjectResponse = sqlx::query_as(
        "SELECT id, host_id, path, name, has_claude_config, project_type, created_at, \
         parent_project_id, git_branch, git_commit_hash, git_commit_message, \
         git_is_dirty, git_ahead, git_behind, git_remotes, git_updated_at \
         FROM projects WHERE host_id = ? AND path = ?",
    )
    .bind(&host_id)
    .bind(&body.path)
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(project)))
}

/// `GET /api/projects/:project_id` - get project detail.
pub async fn get_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectResponse>, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let project: ProjectResponse = sqlx::query_as(
        "SELECT id, host_id, path, name, has_claude_config, project_type, created_at, \
         parent_project_id, git_branch, git_commit_hash, git_commit_message, \
         git_is_dirty, git_ahead, git_behind, git_remotes, git_updated_at \
         FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    Ok(Json(project))
}

/// `GET /api/projects/:project_id/sessions` - list sessions linked to a project.
pub async fn list_project_sessions(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let sessions: Vec<SessionResponse> = sqlx::query_as(
        "SELECT id, host_id, name, shell, status, working_dir, project_id, pid, exit_code, created_at, closed_at \
         FROM sessions WHERE project_id = ? ORDER BY created_at DESC",
    )
    .bind(&project_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(sessions))
}

/// `DELETE /api/projects/:project_id` - unregister project.
pub async fn delete_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    // Look up the project to find its host and path for notification
    let project: Option<(String, String)> =
        sqlx::query_as("SELECT host_id, path FROM projects WHERE id = ?")
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await?;

    if let Some((host_id_str, path)) = project {
        // Notify the agent
        if let Ok(host_id) = host_id_str.parse::<Uuid>()
            && let Some(sender) = state.connections.get_sender(&host_id).await
        {
            let _ = sender.send(ServerMessage::ProjectRemove { path }).await;
        }
    }

    let result = sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(&project_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "project {project_id} not found"
        )));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/projects/:project_id/git/refresh` - trigger git status refresh.
pub async fn trigger_git_refresh(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let project: Option<(String, String)> =
        sqlx::query_as("SELECT host_id, path FROM projects WHERE id = ?")
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await?;

    let (host_id_str, path) =
        project.ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

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

/// `GET /api/projects/:project_id/worktrees` - list worktree children.
pub async fn list_worktrees(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let worktrees: Vec<ProjectResponse> = sqlx::query_as(
        "SELECT id, host_id, path, name, has_claude_config, project_type, created_at, \
         parent_project_id, git_branch, git_commit_hash, git_commit_message, \
         git_is_dirty, git_ahead, git_behind, git_remotes, git_updated_at \
         FROM projects WHERE parent_project_id = ? ORDER BY name",
    )
    .bind(&project_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(worktrees))
}

/// Request body for creating a worktree.
#[derive(Debug, Deserialize)]
pub struct CreateWorktreeRequest {
    pub branch: String,
    pub path: Option<String>,
    pub new_branch: Option<bool>,
}

/// `POST /api/projects/:project_id/worktrees` - request worktree creation.
pub async fn create_worktree(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<CreateWorktreeRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let project: Option<(String, String)> =
        sqlx::query_as("SELECT host_id, path FROM projects WHERE id = ?")
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await?;

    let (host_id_str, project_path) =
        project.ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    sender
        .send(ServerMessage::WorktreeCreate {
            project_path,
            branch: body.branch,
            path: body.path,
            new_branch: body.new_branch.unwrap_or(false),
        })
        .await
        .map_err(|_| AppError::Conflict("failed to send worktree create to agent".to_string()))?;

    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /api/projects/:project_id/worktrees/:worktree_id` - request worktree deletion.
pub async fn delete_worktree(
    State(state): State<Arc<AppState>>,
    Path((project_id, worktree_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let _parsed_wt = parse_project_id(&worktree_id)?;

    // Look up the parent project to get host_id and path
    let parent: Option<(String, String)> =
        sqlx::query_as("SELECT host_id, path FROM projects WHERE id = ?")
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await?;

    let (host_id_str, project_path) =
        parent.ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    // Look up the worktree child project path
    let worktree: Option<(String,)> =
        sqlx::query_as("SELECT path FROM projects WHERE id = ? AND parent_project_id = ?")
            .bind(&worktree_id)
            .bind(&project_id)
            .fetch_optional(&state.db)
            .await?;

    let (worktree_path,) =
        worktree.ok_or_else(|| AppError::NotFound(format!("worktree {worktree_id} not found")))?;

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
