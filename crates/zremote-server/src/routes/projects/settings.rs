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

use super::parse_host_id;
use super::parse_project_id;

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
    state
        .settings_get_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

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
    state
        .settings_save_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

    sender
        .send(ServerMessage::ProjectSaveSettings {
            request_id,
            project_path,
            settings: Box::new(settings),
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
    #[serde(default)]
    pub inputs: std::collections::HashMap<String, String>,
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
    state
        .settings_get_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

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
                let (actions, prompts) = response
                    .settings
                    .map(|s| (s.actions, s.prompts))
                    .unwrap_or_default();
                Ok(Json(
                    serde_json::json!({ "actions": actions, "prompts": prompts }),
                ))
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

/// Request body for resolving a prompt template.
#[derive(Debug, Deserialize)]
pub struct ResolvePromptRequest {
    #[serde(default)]
    pub inputs: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}

/// `POST /api/projects/:project_id/prompts/:prompt_name/resolve` - resolve a prompt template.
///
/// Fetches project settings from the agent, finds the named prompt template,
/// and resolves it with the provided inputs. Only inline templates are supported
/// in server mode; file-based templates require direct agent access.
pub async fn resolve_prompt(
    State(state): State<Arc<AppState>>,
    Path((project_id, prompt_name)): Path<(String, String)>,
    AppJson(body): AppJson<ResolvePromptRequest>,
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
    state
        .settings_get_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

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

    let template = settings
        .prompts
        .iter()
        .find(|p| p.name == prompt_name)
        .ok_or_else(|| AppError::NotFound(format!("prompt template '{prompt_name}' not found")))?;

    // Resolve body - only inline supported in server mode
    let template_body = match &template.body {
        zremote_protocol::project::PromptBody::Inline(text) => text.clone(),
        zremote_protocol::project::PromptBody::File { .. } => {
            return Err(AppError::BadRequest(
                "file-based prompt templates are not supported in server mode; use inline body or connect directly to the agent".to_string(),
            ));
        }
    };

    let worktree_name = body
        .worktree_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .map(String::from);

    // Render template with simple inline replacement
    let mut rendered = template_body;
    for (key, value) in &body.inputs {
        rendered = rendered.replace(&format!("{{{{{key}}}}}"), value);
    }
    rendered = rendered.replace("{{project_path}}", &project_path);
    if let Some(ref wt) = body.worktree_path {
        rendered = rendered.replace("{{worktree_path}}", wt);
    }
    if let Some(ref branch) = body.branch {
        rendered = rendered.replace("{{branch}}", branch);
    }
    if let Some(ref wt_name) = worktree_name {
        rendered = rendered.replace("{{worktree_name}}", wt_name);
    }

    Ok(Json(serde_json::json!({ "prompt": rendered })))
}

/// `POST /api/projects/:project_id/actions/:action_name/resolve-inputs` - resolve action inputs.
///
/// Sends a `ResolveActionInputs` message to the agent and waits for the response.
pub async fn resolve_action_inputs(
    State(state): State<Arc<AppState>>,
    Path((project_id, action_name)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Conflict("invalid host_id in database".to_string()))?;

    let sender = state
        .connections
        .get_sender(&host_id)
        .await
        .ok_or_else(|| AppError::Conflict("host is offline".to_string()))?;

    let request_id = Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .action_inputs_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

    sender
        .send(ServerMessage::ResolveActionInputs {
            request_id,
            project_path,
            action_name,
        })
        .await
        .map_err(|_| {
            state.action_inputs_requests.remove(&request_id);
            AppError::Conflict("failed to send resolve-inputs request to agent".to_string())
        })?;

    match tokio::time::timeout(std::time::Duration::from_secs(15), rx).await {
        Ok(Ok(response)) => {
            if let Some(error) = response.error {
                Err(AppError::BadRequest(error))
            } else {
                Ok(Json(serde_json::json!({ "inputs": response.inputs })))
            }
        }
        Ok(Err(_)) => {
            state.action_inputs_requests.remove(&request_id);
            Err(AppError::Conflict(
                "agent disconnected while resolving inputs".to_string(),
            ))
        }
        Err(_) => {
            state.action_inputs_requests.remove(&request_id);
            Err(AppError::Conflict(
                "resolve action inputs timed out after 15s".to_string(),
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
    state
        .settings_get_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

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
pub(super) fn expand_action_template(
    template: &str,
    project_path: &str,
    body: &RunActionRequest,
) -> String {
    let mut result = template.replace("{{project_path}}", project_path);
    if let Some(ref wt) = body.worktree_path {
        result = result.replace("{{worktree_path}}", wt);
        if let Some(name) = std::path::Path::new(wt)
            .file_name()
            .and_then(|n| n.to_str())
        {
            result = result.replace("{{worktree_name}}", name);
        }
    }
    if let Some(ref branch) = body.branch {
        result = result.replace("{{branch}}", branch);
    }
    for (key, value) in &body.inputs {
        result = result.replace(&format!("{{{{{key}}}}}"), value);
    }
    result
}

/// Resolve working directory for an action.
pub(super) fn resolve_action_working_dir(
    action: &zremote_protocol::project::ProjectAction,
    project_path: &str,
    body: &RunActionRequest,
) -> String {
    if let Some(ref wd) = action.working_dir {
        return expand_action_template(wd, project_path, body);
    }
    let is_worktree = if action.scopes.is_empty() {
        action.worktree_scoped
    } else {
        action
            .scopes
            .contains(&zremote_protocol::project::ActionScope::Worktree)
    };
    if is_worktree && let Some(ref wt) = body.worktree_path {
        return wt.clone();
    }
    project_path.to_string()
}

/// Build environment variables for action execution.
pub(super) fn build_action_env_map(
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
    state
        .directory_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

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
    state
        .settings_get_requests
        .insert(request_id, crate::state::PendingRequest::new(tx));

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
        channel_enabled: false,
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
