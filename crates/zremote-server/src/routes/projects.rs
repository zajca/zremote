use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::queries::claude_sessions as cq;
use zremote_core::queries::projects as q;
use zremote_core::queries::sessions as sq;
use zremote_protocol::ServerMessage;
use zremote_protocol::claude::ClaudeServerMessage;

use crate::error::{AppError, AppJson};
use crate::state::{AppState, ServerEvent, SessionState};

pub type ProjectResponse = q::ProjectRow;
pub type SessionResponse = sq::SessionRow;

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
    let projects = q::list_projects(&state.db, &host_id).await?;
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

    if !sq::host_exists(&state.db, &host_id).await? {
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

    let project_id = Uuid::new_v4().to_string();
    let name = body
        .path
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string();

    let inserted = q::insert_project(&state.db, &project_id, &host_id, &body.path, &name).await?;
    if !inserted {
        return Err(AppError::Conflict(
            "project path already exists".to_string(),
        ));
    }

    let project = q::get_project_by_host_and_path(&state.db, &host_id, &body.path).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `GET /api/projects/:project_id` - get project detail.
pub async fn get_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectResponse>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let project = q::get_project(&state.db, &project_id).await?;
    Ok(Json(project))
}

/// `GET /api/projects/:project_id/sessions` - list sessions linked to a project.
pub async fn list_project_sessions(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let sessions = sq::list_sessions_by_project(&state.db, &project_id).await?;
    Ok(Json(sessions))
}

/// `DELETE /api/projects/:project_id` - unregister project.
pub async fn delete_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    if let Some((host_id_str, path)) = q::get_project_host_and_path(&state.db, &project_id).await?
        && let Ok(host_id) = host_id_str.parse::<Uuid>()
        && let Some(sender) = state.connections.get_sender(&host_id).await
    {
        let _ = sender.send(ServerMessage::ProjectRemove { path }).await;
    }

    let rows = q::delete_project(&state.db, &project_id).await?;
    if rows == 0 {
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
}

/// `POST /api/projects/:project_id/worktrees` - request worktree creation.
pub async fn create_worktree(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<CreateWorktreeRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

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

/// `GET /api/projects/:project_id/settings` - get project settings.
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

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
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.settings_get_requests.insert(request_id, tx);

    sender
        .send(ServerMessage::ProjectGetSettings {
            request_id,
            project_path,
        })
        .await
        .map_err(|_| {
            state.settings_get_requests.remove(&request_id);
            AppError::Conflict("failed to send settings request to agent".to_string())
        })?;

    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(response)) => {
            if let Some(error) = response.error {
                Err(AppError::Internal(error))
            } else {
                Ok(Json(response.settings))
            }
        }
        Ok(Err(_)) => Err(AppError::Internal("response channel closed".to_string())),
        Err(_) => {
            state.settings_get_requests.remove(&request_id);
            Err(AppError::Internal(
                "settings request timed out after 10s".to_string(),
            ))
        }
    }
}

/// `PUT /api/projects/:project_id/settings` - save project settings.
pub async fn save_settings(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(settings): AppJson<zremote_protocol::project::ProjectSettings>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

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
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.settings_save_requests.insert(request_id, tx);

    sender
        .send(ServerMessage::ProjectSaveSettings {
            request_id,
            project_path,
            settings,
        })
        .await
        .map_err(|_| {
            state.settings_save_requests.remove(&request_id);
            AppError::Conflict("failed to send settings save to agent".to_string())
        })?;

    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(response)) => {
            if let Some(error) = response.error {
                Err(AppError::Internal(error))
            } else {
                Ok(StatusCode::NO_CONTENT)
            }
        }
        Ok(Err(_)) => Err(AppError::Internal("response channel closed".to_string())),
        Err(_) => {
            state.settings_save_requests.remove(&request_id);
            Err(AppError::Internal(
                "settings save timed out after 10s".to_string(),
            ))
        }
    }
}

/// Request body for running a project action.
#[derive(Debug, Deserialize)]
pub struct RunActionRequest {
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

/// `GET /api/projects/:project_id/actions` - list available actions for a project.
///
/// Fetches project settings from the agent and returns the configured actions.
pub async fn list_actions(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

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
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.settings_get_requests.insert(request_id, tx);

    sender
        .send(ServerMessage::ProjectGetSettings {
            request_id,
            project_path,
        })
        .await
        .map_err(|_| {
            state.settings_get_requests.remove(&request_id);
            AppError::Conflict("failed to send settings request to agent".to_string())
        })?;

    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(response)) => {
            if let Some(error) = response.error {
                Err(AppError::Internal(error))
            } else {
                let actions = response.settings.map(|s| s.actions).unwrap_or_default();
                Ok(Json(serde_json::json!({ "actions": actions })))
            }
        }
        Ok(Err(_)) => Err(AppError::Internal("response channel closed".to_string())),
        Err(_) => {
            state.settings_get_requests.remove(&request_id);
            Err(AppError::Internal(
                "settings request timed out after 10s".to_string(),
            ))
        }
    }
}

/// `POST /api/projects/:project_id/actions/:action_name/run` - run a project action.
///
/// Fetches project settings from the agent, finds the named action, expands
/// the command template, and creates a session on the agent with `initial_command`.
pub async fn run_action(
    State(state): State<Arc<AppState>>,
    Path((project_id, action_name)): Path<(String, String)>,
    AppJson(body): AppJson<RunActionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

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

    // Fetch settings from agent
    let request_id = Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.settings_get_requests.insert(request_id, tx);

    sender
        .send(ServerMessage::ProjectGetSettings {
            request_id,
            project_path: project_path.clone(),
        })
        .await
        .map_err(|_| {
            state.settings_get_requests.remove(&request_id);
            AppError::Conflict("failed to send settings request to agent".to_string())
        })?;

    let settings = match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(response)) => {
            if let Some(error) = response.error {
                return Err(AppError::Internal(error));
            }
            response
                .settings
                .ok_or_else(|| AppError::NotFound("no project settings found".to_string()))?
        }
        Ok(Err(_)) => return Err(AppError::Internal("response channel closed".to_string())),
        Err(_) => {
            state.settings_get_requests.remove(&request_id);
            return Err(AppError::Internal(
                "settings request timed out after 10s".to_string(),
            ));
        }
    };

    // Find action and expand template
    let action = settings
        .actions
        .iter()
        .find(|a| a.name == action_name)
        .ok_or_else(|| AppError::NotFound(format!("action '{action_name}' not found")))?;

    let expanded_command = expand_action_template(&action.command, &project_path, &body);
    let working_dir = resolve_action_working_dir(action, &project_path, &body);
    let env = build_action_env_map(&settings.env, action, &project_path, &body);

    // Create session with initial_command
    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();

    let name = format!("action: {action_name}");
    let project_id_ref = sq::resolve_project_id(&state.db, &host_id_str, &working_dir).await?;

    sq::insert_session(
        &state.db,
        &session_id_str,
        &host_id_str,
        Some(&name),
        Some(&working_dir),
        project_id_ref.as_deref(),
    )
    .await?;

    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(
            session_id,
            crate::state::SessionState::new(session_id, host_id),
        );
    }

    let msg = ServerMessage::SessionCreate {
        session_id,
        shell: None,
        cols: body.cols.unwrap_or(80),
        rows: body.rows.unwrap_or(24),
        working_dir: Some(working_dir.clone()),
        env: Some(env),
        initial_command: Some(expanded_command.clone()),
    };

    if sender.send(msg).await.is_err() {
        return Err(AppError::Conflict(
            "host went offline, cannot create session".to_string(),
        ));
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "session_id": session_id_str,
            "action": action_name,
            "command": expanded_command,
            "working_dir": working_dir,
            "status": "creating",
        })),
    ))
}

/// Expand template placeholders in a command string.
fn expand_action_template(template: &str, project_path: &str, body: &RunActionRequest) -> String {
    let mut result = template.replace("{{project_path}}", project_path);
    if let Some(ref wt) = body.worktree_path {
        result = result.replace("{{worktree_path}}", wt);
    }
    if let Some(ref branch) = body.branch {
        result = result.replace("{{branch}}", branch);
    }
    result
}

/// Resolve working directory for an action.
fn resolve_action_working_dir(
    action: &zremote_protocol::project::ProjectAction,
    project_path: &str,
    body: &RunActionRequest,
) -> String {
    if let Some(ref wd) = action.working_dir {
        return expand_action_template(wd, project_path, body);
    }
    if action.worktree_scoped
        && let Some(ref wt) = body.worktree_path
    {
        return wt.clone();
    }
    project_path.to_string()
}

/// Build environment variables for action execution.
fn build_action_env_map(
    project_env: &std::collections::HashMap<String, String>,
    action: &zremote_protocol::project::ProjectAction,
    project_path: &str,
    body: &RunActionRequest,
) -> std::collections::HashMap<String, String> {
    let mut env = project_env.clone();

    // Action env overrides project env
    for (k, v) in &action.env {
        env.insert(k.clone(), v.clone());
    }

    // Add ZREMOTE context variables
    env.insert("ZREMOTE_PROJECT_PATH".to_string(), project_path.to_string());
    if let Some(ref wt) = body.worktree_path {
        env.insert("ZREMOTE_WORKTREE_PATH".to_string(), wt.clone());
    }
    if let Some(ref branch) = body.branch {
        env.insert("ZREMOTE_BRANCH".to_string(), branch.clone());
    }

    env
}

/// Query parameters for directory browsing.
#[derive(Debug, Deserialize)]
pub struct BrowseQuery {
    pub path: String,
}

/// `GET /api/hosts/:host_id/browse?path=` - browse directory on host.
pub async fn browse_directory(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
    Query(query): Query<BrowseQuery>,
) -> Result<impl IntoResponse, AppError> {
    let parsed = parse_host_id(&host_id)?;

    if query.path.is_empty() {
        return Err(AppError::BadRequest("path must not be empty".to_string()));
    }

    let sender = state
        .connections
        .get_sender(&parsed)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let request_id = Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.directory_requests.insert(request_id, tx);

    sender
        .send(ServerMessage::ListDirectory {
            request_id,
            path: query.path,
        })
        .await
        .map_err(|_| {
            state.directory_requests.remove(&request_id);
            AppError::Conflict("failed to send browse request to agent".to_string())
        })?;

    match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
        Ok(Ok(response)) => {
            if let Some(error) = response.error {
                Err(AppError::BadRequest(error))
            } else {
                Ok(Json(response.entries))
            }
        }
        Ok(Err(_)) => Err(AppError::Internal("response channel closed".to_string())),
        Err(_) => {
            state.directory_requests.remove(&request_id);
            Err(AppError::Internal(
                "directory listing timed out after 10s".to_string(),
            ))
        }
    }
}

/// Request body for configure with Claude.
#[derive(Debug, Deserialize)]
pub struct ConfigureRequest {
    pub model: Option<String>,
    pub skip_permissions: Option<bool>,
}

/// `POST /api/projects/:project_id/configure` - configure project with Claude.
#[allow(clippy::too_many_lines)]
pub async fn configure_with_claude(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
    AppJson(body): AppJson<ConfigureRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT host_id, path, project_type FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;
    let (host_id_str, project_path, project_type) = row;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    // Fetch current settings from agent
    let request_id = Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.settings_get_requests.insert(request_id, tx);

    sender
        .send(ServerMessage::ProjectGetSettings {
            request_id,
            project_path: project_path.clone(),
        })
        .await
        .map_err(|_| {
            state.settings_get_requests.remove(&request_id);
            AppError::Conflict("failed to send settings request to agent".to_string())
        })?;

    let existing_json = if let Ok(Ok(response)) =
        tokio::time::timeout(std::time::Duration::from_secs(10), rx).await
    {
        if response.error.is_some() {
            None
        } else {
            response
                .settings
                .and_then(|s| serde_json::to_string_pretty(&s).ok())
        }
    } else {
        state.settings_get_requests.remove(&request_id);
        None
    };

    // Build the configure prompt
    let prompt = zremote_core::configure::build_configure_prompt(
        &project_path,
        &project_type,
        existing_json.as_deref(),
    );

    // Create Claude task
    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();
    let claude_task_id = Uuid::new_v4();
    let claude_task_id_str = claude_task_id.to_string();

    cq::insert_session_for_task(
        &state.db,
        &session_id_str,
        &host_id_str,
        &project_path,
        Some(&project_id),
    )
    .await?;

    cq::insert_claude_task(
        &state.db,
        &claude_task_id_str,
        &session_id_str,
        &host_id_str,
        &project_path,
        Some(&project_id),
        body.model.as_deref(),
        Some(&prompt),
        None,
    )
    .await?;

    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, host_id));
    }

    let msg = ServerMessage::ClaudeAction(ClaudeServerMessage::StartSession {
        session_id,
        claude_task_id,
        working_dir: project_path.clone(),
        model: body.model.clone(),
        initial_prompt: Some(prompt),
        resume_cc_session_id: None,
        allowed_tools: vec![],
        skip_permissions: body.skip_permissions.unwrap_or(false),
        output_format: None,
        custom_flags: None,
        continue_last: false,
    });

    if sender.send(msg).await.is_err() {
        return Err(AppError::Conflict(
            "host went offline, cannot start Claude task".to_string(),
        ));
    }

    let _ = state.events.send(ServerEvent::ClaudeTaskStarted {
        task_id: claude_task_id_str.clone(),
        session_id: session_id_str.clone(),
        host_id: host_id_str.clone(),
        project_path: project_path.clone(),
    });

    let task = cq::get_claude_task(&state.db, &claude_task_id_str).await?;
    Ok((StatusCode::CREATED, Json(task)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
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
        })
    }

    async fn insert_host(state: &AppState, id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, agent_version, os, arch, \
             status, last_seen_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'h', '0.1', 'linux', 'x86_64', 'online', \
             '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(id)
        .bind(id)
        .execute(&state.db)
        .await
        .unwrap();
    }

    async fn insert_project(state: &AppState, id: &str, host_id: &str, path: &str, name: &str) {
        sqlx::query("INSERT INTO projects (id, host_id, path, name) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(host_id)
            .bind(path)
            .bind(name)
            .execute(&state.db)
            .await
            .unwrap();
    }

    fn build_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/api/hosts/{host_id}/projects",
                get(list_projects).post(add_project),
            )
            .route("/api/hosts/{host_id}/projects/scan", post(trigger_scan))
            .route(
                "/api/projects/{project_id}",
                get(get_project).delete(delete_project),
            )
            .route(
                "/api/projects/{project_id}/sessions",
                get(list_project_sessions),
            )
            .route(
                "/api/projects/{project_id}/git/refresh",
                post(trigger_git_refresh),
            )
            .route(
                "/api/projects/{project_id}/worktrees",
                get(list_worktrees).post(create_worktree),
            )
            .route(
                "/api/projects/{project_id}/worktrees/{worktree_id}",
                delete(delete_worktree),
            )
            .route("/api/projects/{project_id}/actions", get(list_actions))
            .route(
                "/api/projects/{project_id}/actions/{action_name}/run",
                post(run_action),
            )
            .route("/api/hosts/{host_id}/browse", get(browse_directory))
            .route(
                "/api/projects/{project_id}/configure",
                post(configure_with_claude),
            )
            .with_state(state)
    }

    async fn insert_project_with_type(
        state: &AppState,
        id: &str,
        host_id: &str,
        path: &str,
        name: &str,
        project_type: &str,
    ) {
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, project_type) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(host_id)
        .bind(path)
        .bind(name)
        .bind(project_type)
        .execute(&state.db)
        .await
        .unwrap();
    }

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

    #[tokio::test]
    async fn list_projects_empty() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}/projects"))
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
    async fn list_projects_with_data() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}/projects"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["name"], "test");
    }

    #[tokio::test]
    async fn list_projects_invalid_host_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/hosts/bad-id/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_project_found() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/myapp", "myapp").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/projects/{proj_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "myapp");
        assert_eq!(json["path"], "/home/myapp");
    }

    #[tokio::test]
    async fn get_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/projects/{proj_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_project_invalid_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/projects/not-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_project_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::delete(format!("/api/projects/{proj_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::delete(format!("/api/projects/{proj_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_worktrees_empty() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/projects/{proj_id}/worktrees"))
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
    async fn trigger_scan_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/hosts/{host_id}/projects/scan"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn add_project_empty_path() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_host_not_found() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": "/home/test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn add_project_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": "/home/user/myproject"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "myproject");
        assert_eq!(json["path"], "/home/user/myproject");
    }

    #[tokio::test]
    async fn trigger_git_refresh_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/projects/{proj_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn trigger_git_refresh_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/projects/{proj_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_worktree_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/projects/{proj_id}/worktrees"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"branch": "feature"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_worktree_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let wt_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::delete(format!("/api/projects/{proj_id}/worktrees/{wt_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_actions_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/projects/{proj_id}/actions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_actions_invalid_project_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/projects/not-uuid/actions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_actions_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/projects/{proj_id}/actions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn run_action_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/projects/{proj_id}/actions/build/run"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_action_invalid_project_id() {
        let state = test_state().await;
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post("/api/projects/not-uuid/actions/build/run")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn run_action_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project(&state, &proj_id, &host_id, "/home/test", "test").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/projects/{proj_id}/actions/build/run"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn expand_action_template_basic() {
        let body = RunActionRequest {
            worktree_path: None,
            branch: None,
            cols: None,
            rows: None,
        };
        let result = expand_action_template(
            "cd {{project_path}} && cargo build",
            "/home/user/proj",
            &body,
        );
        assert_eq!(result, "cd /home/user/proj && cargo build");
    }

    #[test]
    fn expand_action_template_with_worktree_and_branch() {
        let body = RunActionRequest {
            worktree_path: Some("/tmp/wt".to_string()),
            branch: Some("feature".to_string()),
            cols: None,
            rows: None,
        };
        let result = expand_action_template(
            "cd {{worktree_path}} && git checkout {{branch}}",
            "/home/user/proj",
            &body,
        );
        assert_eq!(result, "cd /tmp/wt && git checkout feature");
    }

    #[test]
    fn expand_action_template_no_replacement_when_none() {
        let body = RunActionRequest {
            worktree_path: None,
            branch: None,
            cols: None,
            rows: None,
        };
        let result = expand_action_template("echo {{worktree_path}} {{branch}}", "/proj", &body);
        // Placeholders remain when no value provided
        assert_eq!(result, "echo {{worktree_path}} {{branch}}");
    }

    #[test]
    fn resolve_action_working_dir_explicit() {
        use zremote_protocol::project::ProjectAction;
        let action = ProjectAction {
            name: "test".to_string(),
            command: "cargo test".to_string(),
            description: None,
            icon: None,
            working_dir: Some("{{project_path}}/sub".to_string()),
            env: std::collections::HashMap::new(),
            worktree_scoped: false,
        };
        let body = RunActionRequest {
            worktree_path: None,
            branch: None,
            cols: None,
            rows: None,
        };
        let result = resolve_action_working_dir(&action, "/proj", &body);
        assert_eq!(result, "/proj/sub");
    }

    #[test]
    fn resolve_action_working_dir_worktree_scoped() {
        use zremote_protocol::project::ProjectAction;
        let action = ProjectAction {
            name: "test".to_string(),
            command: "cargo test".to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: std::collections::HashMap::new(),
            worktree_scoped: true,
        };
        let body = RunActionRequest {
            worktree_path: Some("/tmp/wt".to_string()),
            branch: None,
            cols: None,
            rows: None,
        };
        let result = resolve_action_working_dir(&action, "/proj", &body);
        assert_eq!(result, "/tmp/wt");
    }

    #[test]
    fn resolve_action_working_dir_fallback_to_project() {
        use zremote_protocol::project::ProjectAction;
        let action = ProjectAction {
            name: "test".to_string(),
            command: "cargo test".to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: std::collections::HashMap::new(),
            worktree_scoped: false,
        };
        let body = RunActionRequest {
            worktree_path: None,
            branch: None,
            cols: None,
            rows: None,
        };
        let result = resolve_action_working_dir(&action, "/proj", &body);
        assert_eq!(result, "/proj");
    }

    #[test]
    fn build_action_env_map_merges_correctly() {
        use zremote_protocol::project::ProjectAction;
        let project_env = std::collections::HashMap::from([
            ("KEY1".to_string(), "val1".to_string()),
            ("KEY2".to_string(), "val2".to_string()),
        ]);
        let action = ProjectAction {
            name: "test".to_string(),
            command: "echo".to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: std::collections::HashMap::from([
                ("KEY2".to_string(), "overridden".to_string()),
                ("KEY3".to_string(), "val3".to_string()),
            ]),
            worktree_scoped: false,
        };
        let body = RunActionRequest {
            worktree_path: Some("/tmp/wt".to_string()),
            branch: Some("feat".to_string()),
            cols: None,
            rows: None,
        };
        let env = build_action_env_map(&project_env, &action, "/proj", &body);
        assert_eq!(env["KEY1"], "val1");
        assert_eq!(env["KEY2"], "overridden");
        assert_eq!(env["KEY3"], "val3");
        assert_eq!(env["ZREMOTE_PROJECT_PATH"], "/proj");
        assert_eq!(env["ZREMOTE_WORKTREE_PATH"], "/tmp/wt");
        assert_eq!(env["ZREMOTE_BRANCH"], "feat");
    }

    #[test]
    fn run_action_request_deserialize_empty() {
        let req: RunActionRequest = serde_json::from_str("{}").unwrap();
        assert!(req.worktree_path.is_none());
        assert!(req.branch.is_none());
        assert!(req.cols.is_none());
        assert!(req.rows.is_none());
    }

    #[test]
    fn run_action_request_deserialize_full() {
        let json = r#"{"worktree_path": "/tmp/wt", "branch": "feat", "cols": 120, "rows": 40}"#;
        let req: RunActionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.worktree_path.as_deref(), Some("/tmp/wt"));
        assert_eq!(req.branch.as_deref(), Some("feat"));
        assert_eq!(req.cols, Some(120));
        assert_eq!(req.rows, Some(40));
    }

    #[tokio::test]
    async fn browse_directory_empty_path_returns_400() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}/browse?path="))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn browse_directory_host_offline_returns_conflict() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::get(format!("/api/hosts/{host_id}/browse?path=/home/user"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn configure_project_not_found() {
        let state = test_state().await;
        let proj_id = Uuid::new_v4().to_string();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/projects/{proj_id}/configure"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn configure_host_offline() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        insert_project_with_type(&state, &proj_id, &host_id, "/home/test", "test", "rust").await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::post(format!("/api/projects/{proj_id}/configure"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn configure_success() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let host_id_str = host_id.to_string();
        let proj_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id_str).await;
        insert_project_with_type(
            &state,
            &proj_id,
            &host_id_str,
            "/home/user/project",
            "project",
            "rust",
        )
        .await;
        let mut rx = register_host_connection(&state, host_id).await;

        // Spawn a task that responds to the settings request from the handler
        let settings_requests = Arc::clone(&state.settings_get_requests);
        tokio::spawn(async move {
            for _ in 0..500 {
                if !settings_requests.is_empty() {
                    let key = settings_requests.iter().next().map(|e| *e.key());
                    if let Some(request_id) = key {
                        if let Some((_, tx)) = settings_requests.remove(&request_id) {
                            let _ = tx.send(crate::state::SettingsGetResponse {
                                settings: None,
                                error: None,
                            });
                            return;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });

        let body = serde_json::json!({
            "model": "sonnet",
            "skip_permissions": false,
        });
        let app = build_router(Arc::clone(&state));
        let resp = app
            .oneshot(
                Request::post(format!("/api/projects/{proj_id}/configure"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
        assert_eq!(json["host_id"], host_id_str);
        assert_eq!(json["project_path"], "/home/user/project");
        assert_eq!(json["model"], "sonnet");
        assert_eq!(json["status"], "starting");

        // Verify that the agent received messages
        let msg = rx.try_recv();
        assert!(msg.is_ok(), "agent should have received a message");
    }
}
