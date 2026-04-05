use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::error::{AppError, AppJson};
use zremote_core::queries::claude_sessions as cq;
use zremote_core::queries::projects as q;
use zremote_core::queries::sessions as sq;
use zremote_core::state::{ServerEvent, SessionState};
use zremote_core::validation::validate_path_no_traversal;

use crate::claude::{CommandBuilder, CommandOptions};
use crate::local::state::LocalAppState;
use crate::project::configure::build_configure_prompt;
use crate::project::settings::read_settings;

use super::parse_host_id;
use super::parse_project_id;

/// `GET /api/projects/:project_id/settings` - get project settings.
pub async fn get_settings(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (_, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let result = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(Path::new(&project_path))
    })
    .await
    .map_err(|e| AppError::Internal(format!("settings read task failed: {e}")))?;

    match result {
        Ok(settings) => Ok(Json(settings)),
        Err(e) => Err(AppError::Internal(e)),
    }
}

/// `PUT /api/projects/:project_id/settings` - save project settings.
pub async fn save_settings(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    AppJson(settings): AppJson<zremote_protocol::project::ProjectSettings>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (_, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let result = tokio::task::spawn_blocking(move || {
        crate::project::settings::write_settings(Path::new(&project_path), &settings)
    })
    .await
    .map_err(|e| AppError::Internal(format!("settings write task failed: {e}")))?;

    match result {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) => Err(AppError::Internal(e)),
    }
}

/// Query parameters for directory browsing.
#[derive(Debug, Deserialize)]
pub struct BrowseQuery {
    pub path: String,
}

/// `GET /api/hosts/:host_id/browse?path=` - browse directory on host.
pub async fn browse_directory(
    State(_state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
    Query(query): Query<BrowseQuery>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_host_id(&host_id)?;

    validate_path_no_traversal(&query.path)?;

    let path = query.path;
    let result = tokio::task::spawn_blocking(move || {
        crate::project::settings::list_directory(Path::new(&path))
    })
    .await
    .map_err(|e| AppError::Internal(format!("directory listing task failed: {e}")))?;

    match result {
        Ok(entries) => Ok(Json(entries)),
        Err(e) => Err(AppError::BadRequest(e)),
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
pub async fn list_actions(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (_, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let result = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(Path::new(&project_path))
    })
    .await
    .map_err(|e| AppError::Internal(format!("settings read task failed: {e}")))?;

    let (actions, prompts) = match result {
        Ok(Some(settings)) => (settings.actions, settings.prompts),
        Ok(None) => (Vec::new(), Vec::new()),
        Err(e) => return Err(AppError::Internal(e)),
    };

    Ok(Json(
        serde_json::json!({ "actions": actions, "prompts": prompts }),
    ))
}

/// `POST /api/projects/:project_id/actions/:action_name/run` - run a project action.
pub async fn run_action(
    State(state): State<Arc<LocalAppState>>,
    AxumPath((project_id, action_name)): AxumPath<(String, String)>,
    AppJson(body): AppJson<RunActionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let path_for_settings = project_path.clone();
    let settings = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(Path::new(&path_for_settings))
    })
    .await
    .map_err(|e| AppError::Internal(format!("settings read task failed: {e}")))?
    .map_err(AppError::Internal)?
    .ok_or_else(|| AppError::NotFound("no project settings found".to_string()))?;

    let action = crate::project::actions::find_action(&settings.actions, &action_name)
        .ok_or_else(|| AppError::NotFound(format!("action '{action_name}' not found")))?
        .clone();

    let worktree_name = body
        .worktree_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .map(String::from);
    let ctx = crate::project::actions::TemplateContext {
        project_path: project_path.clone(),
        worktree_path: body.worktree_path.clone(),
        branch: body.branch.clone(),
        worktree_name,
        custom_inputs: body.inputs.clone(),
    };

    let expanded_command = crate::project::actions::expand_template(&action.command, &ctx);
    let working_dir = crate::project::actions::resolve_working_dir(&action, &ctx);
    let env = crate::project::actions::build_action_env(&settings.env, &action, &ctx);

    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();
    let name = format!("action: {action_name}");
    let cols = body.cols.unwrap_or(80);
    let rows = body.rows.unwrap_or(24);

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

    let shell = super::super::sessions::default_shell();
    let env_map: std::collections::HashMap<String, String> = env.into_iter().collect();
    let env_ref = if env_map.is_empty() {
        None
    } else {
        Some(&env_map)
    };

    {
        let parsed_host_id: Uuid = host_id_str
            .parse()
            .map_err(|_| AppError::Internal("invalid host_id".to_string()))?;
        let mut sessions = state.sessions.write().await;
        sessions.insert(
            session_id,
            zremote_core::state::SessionState::new(session_id, parsed_host_id),
        );
    }

    let manual_config = crate::pty::shell_integration::ShellIntegrationConfig::for_manual_session();
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(
            session_id,
            shell,
            cols,
            rows,
            Some(&working_dir),
            env_ref,
            Some(&manual_config),
        )
        .await
        .map_err(|e| AppError::Internal(format!("failed to spawn PTY: {e}")))?
    };

    sqlx::query("UPDATE sessions SET status = 'active', shell = ?, pid = ? WHERE id = ?")
        .bind(shell)
        .bind(i64::from(pid))
        .bind(&session_id_str)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    {
        let mut sessions = state.sessions.write().await;
        if let Some(s) = sessions.get_mut(&session_id) {
            s.status = zremote_protocol::status::SessionStatus::Active;
        }
    }

    let _ = state.events.send(ServerEvent::SessionCreated {
        session: zremote_core::state::SessionInfo {
            id: session_id_str.clone(),
            host_id: host_id_str.clone(),
            shell: Some(shell.to_string()),
            status: zremote_protocol::status::SessionStatus::Active,
        },
    });

    {
        let cmd_with_newline = format!("{expanded_command}\n");
        let state_clone = state.clone();
        let sid = session_id;
        let cmd_bytes = cmd_with_newline.into_bytes();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let mut mgr = state_clone.session_manager.lock().await;
            if let Err(e) = mgr.write_to(&sid, &cmd_bytes) {
                tracing::warn!(session_id = %sid, error = %e, "failed to write action command to PTY");
            }
        });
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "session_id": session_id_str,
            "action": action_name,
            "command": expanded_command,
            "working_dir": working_dir,
            "status": "active",
            "pid": pid,
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct ConfigureRequest {
    pub model: Option<String>,
    pub skip_permissions: Option<bool>,
}

/// Resolve the default shell (same logic as sessions.rs).
fn configure_default_shell() -> &'static str {
    static SHELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SHELL.get_or_init(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()))
}

/// `POST /api/projects/:project_id/configure` - Configure project with Claude.
#[allow(clippy::too_many_lines)]
pub async fn configure_with_claude(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    AppJson(body): AppJson<ConfigureRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let project_row = sqlx::query_as::<_, (String, String)>(
        "SELECT path, project_type FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("project not found".to_string()))?;

    let (project_path, project_type) = project_row;

    // Read existing settings
    let path_for_settings = project_path.clone();
    let existing_json =
        tokio::task::spawn_blocking(move || read_settings(Path::new(&path_for_settings)))
            .await
            .map_err(|e| AppError::Internal(format!("settings read task failed: {e}")))?
            .ok()
            .flatten()
            .and_then(|s| serde_json::to_string_pretty(&s).ok());

    // Build configure prompt
    let prompt = build_configure_prompt(&project_path, &project_type, existing_json.as_deref());

    let host_id = state.host_id.to_string();
    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();
    let claude_task_id = Uuid::new_v4();
    let claude_task_id_str = claude_task_id.to_string();

    let model = body.model.as_deref();
    let skip_permissions = body.skip_permissions.unwrap_or(false);

    // Insert DB rows
    cq::insert_session_for_task(
        &state.db,
        &session_id_str,
        &host_id,
        &project_path,
        Some(&project_id),
    )
    .await?;

    cq::insert_claude_task(
        &state.db,
        &claude_task_id_str,
        &session_id_str,
        &host_id,
        &project_path,
        Some(&project_id),
        model,
        Some(&prompt),
        None,
    )
    .await?;

    // Create in-memory session state
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, state.host_id));
    }

    // Write prompt to temp file to avoid PTY buffer overflow (prompt is ~4KB)
    let prompt_file_path = crate::claude::write_prompt_file(&prompt)
        .map_err(|e| AppError::Internal(format!("failed to write prompt file: {e}")))?;

    // Build claude command via CommandBuilder (PTY injection path)
    let opts = CommandOptions {
        working_dir: &project_path,
        model,
        initial_prompt: None,
        prompt_file: Some(&prompt_file_path),
        resume_cc_session_id: None,
        continue_last: false,
        allowed_tools: &[],
        skip_permissions,
        output_format: None,
        custom_flags: None,
        channel_enabled: false,
        print_mode: false,
    };

    let cmd = CommandBuilder::build(&opts)
        .map_err(|e| AppError::BadRequest(format!("invalid command options: {e}")))?;

    // Spawn PTY session
    let shell = configure_default_shell();
    let ai_config = crate::pty::shell_integration::ShellIntegrationConfig::for_ai_session();
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(
            session_id,
            shell,
            120,
            40,
            Some(&project_path),
            None,
            Some(&ai_config),
        )
        .await
        .map_err(|e| AppError::Internal(format!("failed to spawn PTY: {e}")))?
    };

    // Update session status in DB
    sqlx::query("UPDATE sessions SET status = 'active', shell = ?, pid = ? WHERE id = ?")
        .bind(shell)
        .bind(i64::from(pid))
        .bind(&session_id_str)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    // Brief delay to let the shell initialize before writing the command
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Write the claude command into the PTY
    {
        let mut mgr = state.session_manager.lock().await;
        mgr.write_to(&session_id, cmd.as_bytes())
            .map_err(|e| AppError::Internal(format!("failed to write command to PTY: {e}")))?;
    }

    // Broadcast event
    let _ = state.events.send(ServerEvent::ClaudeTaskStarted {
        task_id: claude_task_id_str.clone(),
        session_id: session_id_str.clone(),
        host_id: host_id.clone(),
        project_path: project_path.clone(),
    });

    let task = cq::get_claude_task(&state.db, &claude_task_id_str).await?;
    Ok((StatusCode::CREATED, Json(task)))
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
pub async fn resolve_prompt(
    State(state): State<Arc<LocalAppState>>,
    AxumPath((project_id, prompt_name)): AxumPath<(String, String)>,
    AppJson(body): AppJson<ResolvePromptRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (_, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let path_for_settings = project_path.clone();
    let settings = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(Path::new(&path_for_settings))
    })
    .await
    .map_err(|e| AppError::Internal(format!("settings read task failed: {e}")))?
    .map_err(AppError::Internal)?
    .ok_or_else(|| AppError::NotFound("no project settings found".to_string()))?;

    let template = settings
        .prompts
        .iter()
        .find(|p| p.name == prompt_name)
        .ok_or_else(|| AppError::NotFound(format!("prompt template '{prompt_name}' not found")))?;

    let project_path_clone = project_path.clone();
    let body_clone = template.body.clone();
    let template_body = tokio::task::spawn_blocking(move || {
        crate::project::prompts::resolve_body(Path::new(&project_path_clone), &body_clone)
    })
    .await
    .map_err(|e| AppError::Internal(format!("template resolve task failed: {e}")))?
    .map_err(AppError::Internal)?;

    let worktree_name = body
        .worktree_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .map(String::from);
    let ctx = crate::project::actions::TemplateContext {
        project_path,
        worktree_path: body.worktree_path,
        branch: body.branch,
        worktree_name,
        custom_inputs: std::collections::HashMap::new(),
    };

    let rendered = crate::project::prompts::render_prompt(&template_body, &body.inputs, &ctx);

    Ok(Json(serde_json::json!({ "prompt": rendered })))
}

/// `POST /api/projects/:project_id/actions/:action_name/resolve-inputs` - resolve action inputs.
pub async fn resolve_action_inputs_handler(
    State(state): State<Arc<LocalAppState>>,
    AxumPath((project_id, action_name)): AxumPath<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (_, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let path_for_settings = project_path.clone();
    let settings = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(Path::new(&path_for_settings))
    })
    .await
    .map_err(|e| AppError::Conflict(format!("settings read task failed: {e}")))?
    .map_err(AppError::Conflict)?
    .ok_or_else(|| AppError::NotFound("no project settings found".to_string()))?;

    let action = crate::project::actions::find_action(&settings.actions, &action_name)
        .ok_or_else(|| AppError::NotFound(format!("action '{action_name}' not found")))?
        .clone();

    let project_env = settings.env.clone();
    let inputs = crate::project::action_inputs::resolve_action_inputs(
        &action,
        Path::new(&project_path),
        &project_env,
    )
    .await;

    Ok(Json(serde_json::json!({ "inputs": inputs })))
}
