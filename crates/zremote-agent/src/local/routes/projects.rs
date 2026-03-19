use std::collections::HashSet;
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

use crate::claude::{CommandBuilder, CommandOptions};
use crate::local::state::LocalAppState;
use crate::project::configure::build_configure_prompt;
use crate::project::git::GitInspector;
use crate::project::scanner::ProjectScanner;
use crate::project::settings::read_settings;

pub type ProjectResponse = q::ProjectRow;
pub type SessionResponse = sq::SessionRow;

/// Request body for manually adding a project.
#[derive(Debug, Deserialize)]
pub struct AddProjectRequest {
    pub path: String,
}

/// Request body for creating a worktree.
#[derive(Debug, Deserialize)]
pub struct CreateWorktreeRequest {
    pub branch: String,
    pub path: Option<String>,
    pub new_branch: Option<bool>,
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
    State(state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let _parsed = parse_host_id(&host_id)?;
    let projects = q::list_projects(&state.db, &host_id).await?;
    Ok(Json(projects))
}

/// `POST /api/hosts/:host_id/projects` - manually add a project.
pub async fn add_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
    AppJson(body): AppJson<AddProjectRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_host_id(&host_id)?;

    if body.path.is_empty() {
        return Err(AppError::BadRequest("path must not be empty".to_string()));
    }

    if !sq::host_exists(&state.db, &host_id).await? {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    // Detect project info directly from filesystem
    let path = Path::new(&body.path);
    let info = ProjectScanner::detect_at(path);

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

    // Update git info if detected
    if let Some(ref info) = info
        && let Some(ref git) = info.git_info
    {
        let remotes_json = serde_json::to_string(&git.remotes).unwrap_or_default();
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE projects SET project_type = ?, has_claude_config = ?, has_zremote_config = ?, \
             git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
             git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_remotes = ?, git_updated_at = ? \
             WHERE id = ?",
        )
        .bind(&info.project_type)
        .bind(info.has_claude_config)
        .bind(info.has_zremote_config)
        .bind(&git.branch)
        .bind(&git.commit_hash)
        .bind(&git.commit_message)
        .bind(git.is_dirty)
        .bind(git.ahead)
        .bind(git.behind)
        .bind(&remotes_json)
        .bind(&now)
        .bind(&project_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;
    }

    let project = q::get_project_by_host_and_path(&state.db, &host_id, &body.path).await?;

    // Broadcast event
    let _ = state.events.send(ServerEvent::ProjectsUpdated {
        host_id: host_id.clone(),
    });

    Ok((StatusCode::CREATED, Json(project)))
}

/// `POST /api/hosts/:host_id/projects/scan` - trigger project scan directly.
pub async fn trigger_scan(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_host_id(&host_id)?;

    // Run scan directly on this machine
    let projects = tokio::task::spawn_blocking(|| {
        let mut scanner = ProjectScanner::new();
        scanner.scan()
    })
    .await
    .map_err(|e| AppError::Internal(format!("scan task failed: {e}")))?;

    // Upsert each discovered project into the database
    for info in &projects {
        let pid = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:{}", host_id, info.path).as_bytes(),
        )
        .to_string();

        q::insert_project(&state.db, &pid, &host_id, &info.path, &info.name).await?;

        // Update project metadata
        let remotes_json = info
            .git_info
            .as_ref()
            .map(|g| serde_json::to_string(&g.remotes).unwrap_or_default());
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "UPDATE projects SET project_type = ?, has_claude_config = ?, has_zremote_config = ?, \
             git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
             git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_remotes = ?, git_updated_at = ? \
             WHERE id = ?",
        )
        .bind(&info.project_type)
        .bind(info.has_claude_config)
        .bind(info.has_zremote_config)
        .bind(info.git_info.as_ref().and_then(|g| g.branch.as_deref()))
        .bind(
            info.git_info
                .as_ref()
                .and_then(|g| g.commit_hash.as_deref()),
        )
        .bind(
            info.git_info
                .as_ref()
                .and_then(|g| g.commit_message.as_deref()),
        )
        .bind(info.git_info.as_ref().is_some_and(|g| g.is_dirty))
        .bind(info.git_info.as_ref().map_or(0, |g| g.ahead))
        .bind(info.git_info.as_ref().map_or(0, |g| g.behind))
        .bind(&remotes_json)
        .bind(&now)
        .bind(&pid)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;
    }

    // Broadcast event
    let _ = state.events.send(ServerEvent::ProjectsUpdated {
        host_id: host_id.clone(),
    });

    Ok(StatusCode::ACCEPTED)
}

/// `GET /api/projects/:project_id` - get project detail.
pub async fn get_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<ProjectResponse>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let project = q::get_project(&state.db, &project_id).await?;
    Ok(Json(project))
}

/// `DELETE /api/projects/:project_id` - unregister project.
pub async fn delete_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let rows = q::delete_project(&state.db, &project_id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "project {project_id} not found"
        )));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/projects/:project_id/sessions` - sessions for a project.
pub async fn list_project_sessions(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let sessions = sq::list_sessions_by_project(&state.db, &project_id).await?;
    Ok(Json(sessions))
}

/// `POST /api/projects/:project_id/git/refresh` - call `GitInspector::inspect()` directly.
pub async fn trigger_git_refresh(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (_, path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let path_clone = path.clone();
    let result = tokio::task::spawn_blocking(move || GitInspector::inspect(Path::new(&path_clone)))
        .await
        .map_err(|e| AppError::Internal(format!("git inspect task failed: {e}")))?;

    if let Some((git_info, worktrees)) = result {
        let remotes_json = serde_json::to_string(&git_info.remotes).unwrap_or_default();
        let now = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "UPDATE projects SET \
             git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
             git_is_dirty = ?, git_ahead = ?, git_behind = ?, git_remotes = ?, git_updated_at = ? \
             WHERE id = ?",
        )
        .bind(&git_info.branch)
        .bind(&git_info.commit_hash)
        .bind(&git_info.commit_message)
        .bind(git_info.is_dirty)
        .bind(git_info.ahead)
        .bind(git_info.behind)
        .bind(&remotes_json)
        .bind(&now)
        .bind(&project_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

        // Upsert worktree entries
        let host_id = state.host_id.to_string();
        for wt in &worktrees {
            let wt_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("{}:{}", host_id, wt.path).as_bytes(),
            )
            .to_string();
            let wt_name = wt.path.rsplit('/').next().unwrap_or("worktree").to_string();

            sqlx::query(
                "INSERT OR IGNORE INTO projects (id, host_id, path, name, parent_project_id, project_type) \
                 VALUES (?, ?, ?, ?, ?, 'worktree')",
            )
            .bind(&wt_id)
            .bind(&host_id)
            .bind(&wt.path)
            .bind(&wt_name)
            .bind(&project_id)
            .execute(&state.db)
            .await
            .map_err(AppError::Database)?;

            // Update worktree git info
            sqlx::query("UPDATE projects SET git_branch = ?, git_commit_hash = ? WHERE id = ?")
                .bind(&wt.branch)
                .bind(&wt.commit_hash)
                .bind(&wt_id)
                .execute(&state.db)
                .await
                .map_err(AppError::Database)?;
        }
    }

    let project = q::get_project(&state.db, &project_id).await?;
    Ok(Json(project))
}

/// `GET /api/projects/:project_id/worktrees` - list worktree children.
pub async fn list_worktrees(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let worktrees = q::list_worktrees(&state.db, &project_id).await?;
    Ok(Json(worktrees))
}

/// `POST /api/projects/:project_id/worktrees` - create worktree directly.
#[allow(clippy::too_many_lines)]
pub async fn create_worktree(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    AppJson(body): AppJson<CreateWorktreeRequest>,
) -> Result<axum::response::Response, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    // Check for custom create_command
    let wt_settings = read_worktree_settings(&project_path).await;
    if let Some(create_cmd) = wt_settings.as_ref().and_then(|s| s.create_command.as_ref()) {
        let worktree_name = body.branch.replace('/', "-");
        let cmd = create_cmd
            .replace("{{project_path}}", &project_path)
            .replace("{{branch}}", &body.branch)
            .replace("{{worktree_name}}", &worktree_name);

        let project_id_ref = sq::resolve_project_id(&state.db, &host_id_str, &project_path).await?;
        let (session_id_str, _session_uuid) = spawn_command_session(
            &state,
            &host_id_str,
            &format!("worktree: create {worktree_name}"),
            &project_path,
            project_id_ref.as_deref(),
            &cmd,
        )
        .await?;

        // Background task: monitor session completion, then update DB
        let events = state.events.clone();
        let db = state.db.clone();
        let sid = session_id_str.clone();
        let pp = project_path.clone();
        let hid = host_id_str.clone();
        let pid = project_id.clone();
        let branch = body.branch.clone();
        tokio::spawn(async move {
            let mut rx = events.subscribe();
            loop {
                match rx.recv().await {
                    Ok(ServerEvent::SessionClosed {
                        session_id,
                        exit_code,
                    }) if session_id == sid => {
                        if exit_code == Some(0) {
                            // Inspect git to find new worktrees
                            let pp_clone = pp.clone();
                            let inspect_result = tokio::task::spawn_blocking(move || {
                                GitInspector::inspect(Path::new(&pp_clone))
                            })
                            .await;

                            if let Ok(Some((_git_info, worktrees))) = inspect_result {
                                // Get existing worktree paths from DB
                                let existing =
                                    q::list_worktrees(&db, &pid).await.unwrap_or_default();
                                let existing_paths: HashSet<String> =
                                    existing.iter().map(|w| w.path.clone()).collect();

                                for wt in &worktrees {
                                    if !existing_paths.contains(&wt.path) && wt.path != pp {
                                        let wt_id = Uuid::new_v4().to_string();
                                        let wt_name = wt
                                            .path
                                            .rsplit('/')
                                            .next()
                                            .unwrap_or("worktree")
                                            .to_string();
                                        let _ = sqlx::query(
                                            "INSERT OR IGNORE INTO projects (id, host_id, path, name, parent_project_id, project_type) VALUES (?, ?, ?, ?, ?, 'worktree')"
                                        )
                                        .bind(&wt_id)
                                        .bind(&hid)
                                        .bind(&wt.path)
                                        .bind(&wt_name)
                                        .bind(&pid)
                                        .execute(&db)
                                        .await;

                                        let _ = sqlx::query("UPDATE projects SET git_branch = ?, git_commit_hash = ? WHERE id = ?")
                                            .bind(&wt.branch)
                                            .bind(&wt.commit_hash)
                                            .bind(&wt_id)
                                            .execute(&db)
                                            .await;
                                    }
                                }

                                let _ = events.send(ServerEvent::ProjectsUpdated {
                                    host_id: hid.clone(),
                                });

                                // Run on_create hook if configured
                                if let Some(on_create) =
                                    wt_settings.as_ref().and_then(|s| s.on_create.as_ref())
                                    && let Some(new_wt) = worktrees
                                        .iter()
                                        .find(|w| !existing_paths.contains(&w.path) && w.path != pp)
                                {
                                    let wt_name_for_hook = std::path::Path::new(&new_wt.path)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("");
                                    let hook_cmd = crate::project::hooks::expand_hook_template(
                                        on_create,
                                        &pp,
                                        &new_wt.path,
                                        new_wt.branch.as_deref().unwrap_or(&branch),
                                        wt_name_for_hook,
                                    );
                                    let _ = crate::project::hooks::execute_hook_async(
                                        hook_cmd,
                                        std::path::PathBuf::from(&new_wt.path),
                                        vec![],
                                        None,
                                    )
                                    .await;
                                }
                            }
                        } else {
                            tracing::warn!(
                                session_id = %sid,
                                exit_code = ?exit_code,
                                "custom create_command failed"
                            );
                        }
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    _ => continue,
                }
            }
        });

        return Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({
                "session_id": session_id_str,
                "mode": "custom_command",
            })),
        )
            .into_response());
    }

    // Default flow: existing GitInspector behavior
    let branch = body.branch.clone();
    let wt_path = body.path.clone();
    let new_branch = body.new_branch.unwrap_or(false);
    let repo_path = project_path.clone();

    let result = tokio::task::spawn_blocking(move || {
        GitInspector::create_worktree(
            Path::new(&repo_path),
            &branch,
            wt_path.as_deref().map(Path::new),
            new_branch,
        )
    })
    .await
    .map_err(|e| AppError::Internal(format!("worktree create task failed: {e}")))?
    .map_err(|e| AppError::Internal(format!("failed to create worktree: {e}")))?;

    // Insert worktree as a child project
    let wt_id = Uuid::new_v4().to_string();
    let wt_name = result
        .path
        .rsplit('/')
        .next()
        .unwrap_or("worktree")
        .to_string();

    sqlx::query(
        "INSERT OR IGNORE INTO projects (id, host_id, path, name, parent_project_id, project_type) \
         VALUES (?, ?, ?, ?, ?, 'worktree')",
    )
    .bind(&wt_id)
    .bind(&host_id_str)
    .bind(&result.path)
    .bind(&wt_name)
    .bind(&project_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    // Update git info on the new worktree
    sqlx::query("UPDATE projects SET git_branch = ?, git_commit_hash = ? WHERE id = ?")
        .bind(&result.branch)
        .bind(&result.commit_hash)
        .bind(&wt_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    // Run on_create hook if configured
    let hook_result = run_worktree_hook(
        &project_path,
        &result.path,
        result.branch.as_deref().unwrap_or_default(),
        |wt_settings| wt_settings.on_create.as_deref(),
    )
    .await;

    if let Some(ref hr) = hook_result {
        if hr.success {
            tracing::info!(worktree = %result.path, "on_create hook succeeded");
        } else {
            tracing::warn!(worktree = %result.path, output = %hr.output.as_deref().unwrap_or(""), "on_create hook failed");
        }
    }

    let mut project = serde_json::to_value(q::get_project(&state.db, &wt_id).await?)
        .map_err(|e| AppError::Internal(format!("serialization error: {e}")))?;
    if let Some(ref hr) = hook_result {
        project["hook_result"] = serde_json::json!({
            "success": hr.success,
            "output": hr.output,
            "duration_ms": hr.duration_ms,
        });
    }

    Ok((StatusCode::CREATED, Json(project)).into_response())
}

/// `DELETE /api/projects/:project_id/worktrees/:worktree_id` - delete worktree directly.
#[allow(clippy::too_many_lines)]
pub async fn delete_worktree(
    State(state): State<Arc<LocalAppState>>,
    AxumPath((project_id, worktree_id)): AxumPath<(String, String)>,
) -> Result<axum::response::Response, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let _parsed_wt = parse_project_id(&worktree_id)?;

    let (host_id_str, project_path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let worktree_path = q::get_worktree_path(&state.db, &worktree_id, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("worktree {worktree_id} not found")))?;

    // Check for custom delete_command
    let wt_settings = read_worktree_settings(&project_path).await;
    if let Some(delete_cmd) = wt_settings.as_ref().and_then(|s| s.delete_command.as_ref()) {
        // Run on_delete hook first (before custom command)
        let _ = run_worktree_hook(&project_path, &worktree_path, "", |wt_settings| {
            wt_settings.on_delete.as_deref()
        })
        .await;

        let worktree_name = std::path::Path::new(&worktree_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        // Get branch from DB for template expansion
        let wt_branch =
            sqlx::query_scalar::<_, Option<String>>("SELECT git_branch FROM projects WHERE id = ?")
                .bind(&worktree_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten()
                .flatten()
                .unwrap_or_default();

        let cmd = delete_cmd
            .replace("{{project_path}}", &project_path)
            .replace("{{worktree_path}}", &worktree_path)
            .replace("{{worktree_name}}", &worktree_name)
            .replace("{{branch}}", &wt_branch);

        let project_id_ref = sq::resolve_project_id(&state.db, &host_id_str, &project_path).await?;
        let (session_id_str, _session_uuid) = spawn_command_session(
            &state,
            &host_id_str,
            &format!("worktree: delete {worktree_name}"),
            &project_path,
            project_id_ref.as_deref(),
            &cmd,
        )
        .await?;

        // Background task: on success, remove worktree from DB
        let events = state.events.clone();
        let db = state.db.clone();
        let sid = session_id_str.clone();
        let wt_id = worktree_id.clone();
        let hid = host_id_str.clone();
        tokio::spawn(async move {
            let mut rx = events.subscribe();
            loop {
                match rx.recv().await {
                    Ok(ServerEvent::SessionClosed {
                        session_id,
                        exit_code,
                    }) if session_id == sid => {
                        if exit_code == Some(0) {
                            let _ = q::delete_project(&db, &wt_id).await;
                            let _ = events.send(ServerEvent::ProjectsUpdated {
                                host_id: hid.clone(),
                            });
                        } else {
                            tracing::warn!(
                                session_id = %sid,
                                exit_code = ?exit_code,
                                "custom delete_command failed"
                            );
                        }
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    _ => continue,
                }
            }
        });

        return Ok((
            StatusCode::OK,
            Json(serde_json::json!({
                "session_id": session_id_str,
                "mode": "custom_command",
            })),
        )
            .into_response());
    }

    // Default flow: existing behavior
    // Run on_delete hook before removing worktree
    let hook_result = run_worktree_hook(&project_path, &worktree_path, "", |wt_settings| {
        wt_settings.on_delete.as_deref()
    })
    .await;

    if let Some(ref hr) = hook_result {
        if hr.success {
            tracing::info!(worktree = %worktree_path, "on_delete hook succeeded");
        } else {
            tracing::warn!(worktree = %worktree_path, output = %hr.output.as_deref().unwrap_or(""), "on_delete hook failed");
        }
    }

    let repo = project_path.clone();
    let wt = worktree_path.clone();

    tokio::task::spawn_blocking(move || {
        GitInspector::remove_worktree(Path::new(&repo), Path::new(&wt), false)
    })
    .await
    .map_err(|e| AppError::Internal(format!("worktree delete task failed: {e}")))?
    .map_err(|e| AppError::Internal(format!("failed to delete worktree: {e}")))?;

    // Remove from DB
    q::delete_project(&state.db, &worktree_id).await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

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

    if query.path.is_empty() {
        return Err(AppError::BadRequest("path must not be empty".to_string()));
    }

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

/// Read worktree settings for a project, if configured.
async fn read_worktree_settings(
    project_path: &str,
) -> Option<zremote_protocol::project::WorktreeSettings> {
    let pp = project_path.to_string();
    let settings = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(Path::new(&pp))
    })
    .await
    .ok()?
    .ok()
    .flatten()?;
    settings.worktree
}

/// Spawn a PTY session and write a command to it. Returns (session_id_str, session_uuid).
async fn spawn_command_session(
    state: &Arc<LocalAppState>,
    host_id_str: &str,
    name: &str,
    working_dir: &str,
    project_id_ref: Option<&str>,
    command: &str,
) -> Result<(String, Uuid), AppError> {
    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();

    sq::insert_session(
        &state.db,
        &session_id_str,
        host_id_str,
        Some(name),
        Some(working_dir),
        project_id_ref,
    )
    .await?;

    let shell = super::sessions::default_shell();

    {
        let parsed_host_id: Uuid = host_id_str
            .parse()
            .map_err(|_| AppError::Internal("invalid host_id".to_string()))?;
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, parsed_host_id));
    }

    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(session_id, shell, 80, 24, Some(working_dir), None)
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
            s.status = "active".to_string();
        }
    }

    let _ = state.events.send(ServerEvent::SessionCreated {
        session: zremote_core::state::SessionInfo {
            id: session_id_str.clone(),
            host_id: host_id_str.to_string(),
            shell: Some(shell.to_string()),
            status: "active".to_string(),
        },
    });

    // Write command with 200ms delay for PTY stabilization
    let cmd_with_newline = format!("{command}\n");
    let state_clone = state.clone();
    let sid = session_id;
    let cmd_bytes = cmd_with_newline.into_bytes();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut mgr = state_clone.session_manager.lock().await;
        if let Err(e) = mgr.write_to(&sid, &cmd_bytes) {
            tracing::warn!(session_id = %sid, error = %e, "failed to write command to PTY");
        }
    });

    Ok((session_id_str, session_id))
}

/// Run a worktree lifecycle hook (on_create or on_delete) if configured in settings.
///
/// Returns `Some(HookResultInfo)` if a hook was executed, `None` if no hook is configured.
async fn run_worktree_hook(
    project_path: &str,
    worktree_path: &str,
    branch: &str,
    hook_selector: impl FnOnce(&zremote_protocol::project::WorktreeSettings) -> Option<&str>,
) -> Option<zremote_protocol::HookResultInfo> {
    let pp = project_path.to_string();
    let settings = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(Path::new(&pp))
    })
    .await
    .ok()?
    .ok()
    .flatten()?;

    let wt_settings = settings.worktree.as_ref()?;
    let template = hook_selector(wt_settings)?;

    let worktree_name = std::path::Path::new(worktree_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let cmd = crate::project::hooks::expand_hook_template(
        template,
        project_path,
        worktree_path,
        branch,
        worktree_name,
    );
    let result = crate::project::hooks::execute_hook_async(
        cmd,
        std::path::PathBuf::from(worktree_path),
        vec![],
        None,
    )
    .await;

    Some(zremote_protocol::HookResultInfo {
        success: result.success,
        output: if result.output.is_empty() {
            None
        } else {
            Some(result.output)
        },
        duration_ms: result.duration.as_millis() as u64,
    })
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

    let shell = super::sessions::default_shell();
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

    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(session_id, shell, cols, rows, Some(&working_dir), env_ref)
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
            s.status = "active".to_string();
        }
    }

    let _ = state.events.send(ServerEvent::SessionCreated {
        session: zremote_core::state::SessionInfo {
            id: session_id_str.clone(),
            host_id: host_id_str.clone(),
            shell: Some(shell.to_string()),
            status: "active".to_string(),
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
    let skip_permissions = body.skip_permissions.unwrap_or(true);

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

    // Build claude command via CommandBuilder (PTY injection path)
    let opts = CommandOptions {
        working_dir: &project_path,
        model,
        initial_prompt: Some(&prompt),
        resume_cc_session_id: None,
        continue_last: false,
        allowed_tools: &[],
        skip_permissions,
        output_format: None,
        custom_flags: None,
    };

    let cmd = CommandBuilder::build(&opts)
        .map_err(|e| AppError::BadRequest(format!("invalid command options: {e}")))?;

    // Spawn PTY session
    let shell = configure_default_shell();
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(session_id, shell, 120, 40, Some(&project_path), None)
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
    .map_err(|e| AppError::Internal(format!("settings read task failed: {e}")))?
    .map_err(AppError::Internal)?
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{delete, get, post};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use crate::local::upsert_local_host;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(pool, "test-host".to_string(), host_id, shutdown, false)
    }

    fn build_test_router(state: Arc<LocalAppState>) -> Router {
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
            .route(
                "/api/projects/{project_id}/actions/{action_name}/resolve-inputs",
                post(resolve_action_inputs_handler),
            )
            .route(
                "/api/projects/{project_id}/prompts/{prompt_name}/resolve",
                post(resolve_prompt),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn list_projects_empty() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/projects"))
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
    async fn list_projects_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/hosts/not-a-uuid/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_empty_path() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_project_invalid_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_project_sessions_empty() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Insert a project first
        q::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/sessions"))
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
    async fn list_worktrees_empty() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/worktrees"))
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
    async fn add_project_and_get() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Create a temp dir to act as a project
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        let app = build_test_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({ "path": project_path }))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["path"], project_path);
    }

    #[tokio::test]
    async fn trigger_git_refresh_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_worktree_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let worktree_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/api/projects/{project_id}/worktrees/{worktree_id}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_project_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify it's gone
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_project_invalid_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/projects/not-a-uuid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_project_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(
            &state.db,
            &project_id,
            &host_id,
            "/tmp/myproject",
            "myproject",
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}"))
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
        assert_eq!(json["id"], project_id);
        assert_eq!(json["path"], "/tmp/myproject");
        assert_eq!(json["name"], "myproject");
    }

    #[tokio::test]
    async fn list_projects_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        q::insert_project(
            &state.db,
            &Uuid::new_v4().to_string(),
            &host_id,
            "/tmp/proj1",
            "proj1",
        )
        .await
        .unwrap();
        q::insert_project(
            &state.db,
            &Uuid::new_v4().to_string(),
            &host_id,
            "/tmp/proj2",
            "proj2",
        )
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/projects"))
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
    }

    #[tokio::test]
    async fn add_project_host_not_found() {
        let state = test_state().await;
        let fake_host = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{fake_host}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": "/tmp/test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn add_project_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/hosts/not-a-uuid/projects")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": "/tmp/test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_project_sessions_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/not-a-uuid/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn trigger_git_refresh_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/git/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn trigger_git_refresh_on_non_git_dir() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Create a temp dir (not a git repo)
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Non-git dir returns the project without git info (still OK)
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], project_id);
        assert!(json["git_branch"].is_null());
    }

    #[tokio::test]
    async fn list_worktrees_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/not-a-uuid/worktrees")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_worktree_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/worktrees"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"branch": "feature"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_worktree_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/worktrees")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"branch": "feature"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_worktree_invalid_project_id() {
        let state = test_state().await;
        let worktree_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/not-a-uuid/worktrees/{worktree_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_worktree_invalid_worktree_id() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}/worktrees/not-a-uuid"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_invalid_body() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Missing required field 'path'
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_with_git_repo() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Create a temp dir and init a git repo
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        std::process::Command::new("git")
            .args(["init", &project_path])
            .output()
            .unwrap();

        // Configure git for the test repo
        std::process::Command::new("git")
            .args(["-C", &project_path, "config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "config", "user.name", "Test"])
            .output()
            .unwrap();

        // Create a commit so git has state
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "commit", "-m", "init"])
            .output()
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({ "path": project_path }))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["path"], project_path);
        // Should have git info populated
        assert!(!json["git_branch"].is_null());
    }

    #[tokio::test]
    async fn trigger_git_refresh_on_git_repo() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Create a temp dir and init a git repo
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        std::process::Command::new("git")
            .args(["init", &project_path])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "config", "user.name", "Test"])
            .output()
            .unwrap();

        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", &project_path, "commit", "-m", "initial commit"])
            .output()
            .unwrap();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/git/refresh"))
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
        assert_eq!(json["id"], project_id);
        // Should have git info
        assert!(!json["git_branch"].is_null());
        assert!(!json["git_commit_hash"].is_null());
        assert!(!json["git_commit_message"].is_null());
    }

    #[tokio::test]
    async fn trigger_scan_valid_host() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects/scan"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn delete_project_and_verify_gone() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/to-delete", "del")
            .await
            .unwrap();

        let app = build_test_router(state);

        // Delete
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // List should be empty
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/hosts/{host_id}/projects"))
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
    async fn create_worktree_invalid_body() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/worktrees"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Missing required field 'branch'
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn trigger_scan_invalid_host_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/hosts/not-a-uuid/projects/scan")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_project_sessions_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/test", "test")
            .await
            .unwrap();

        // Insert a session linked to this project
        let session_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, project_id) VALUES (?, ?, 'active', ?)",
        )
        .bind(&session_id)
        .bind(&host_id)
        .bind(&project_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/sessions"))
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
        assert_eq!(json[0]["id"], session_id);
    }

    #[tokio::test]
    async fn list_worktrees_with_data() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();
        let worktree_id = Uuid::new_v4().to_string();

        q::insert_project(&state.db, &project_id, &host_id, "/tmp/main", "main")
            .await
            .unwrap();

        // Insert a worktree child project
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, parent_project_id, project_type) \
             VALUES (?, ?, ?, ?, ?, 'worktree')",
        )
        .bind(&worktree_id)
        .bind(&host_id)
        .bind("/tmp/main-wt")
        .bind("main-wt")
        .bind(&project_id)
        .execute(&state.db)
        .await
        .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/worktrees"))
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
        assert_eq!(json[0]["id"], worktree_id);
        assert_eq!(json[0]["parent_project_id"], project_id);
    }

    #[tokio::test]
    async fn add_project_empty_path_returns_bad_request() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"path": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_project_without_git() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();

        // Create a temp dir that is NOT a git repo
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/hosts/{host_id}/projects"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&serde_json::json!({ "path": project_path }))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["path"], project_path);
        // No git info should be present
        assert!(json["git_branch"].is_null());
    }

    #[tokio::test]
    async fn delete_project_nonexistent() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_project_nonexistent() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn trigger_git_refresh_nonexistent_project() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/git/refresh"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_worktree_on_non_git_project() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Create a temp dir that is NOT a git repo
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/worktrees"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"branch": "feature"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should fail because dir is not a git repo
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn parse_host_id_valid() {
        let id = Uuid::new_v4().to_string();
        assert!(parse_host_id(&id).is_ok());
    }

    #[test]
    fn parse_host_id_invalid() {
        let result = parse_host_id("not-a-uuid");
        assert!(result.is_err());
    }

    #[test]
    fn parse_project_id_valid() {
        let id = Uuid::new_v4().to_string();
        assert!(parse_project_id(&id).is_ok());
    }

    #[test]
    fn parse_project_id_invalid() {
        let result = parse_project_id("invalid");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_actions_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/actions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_actions_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/projects/not-a-uuid/actions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_actions_no_settings_file() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Create a temp dir without .zremote/settings.json
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/actions"))
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
        assert_eq!(json["actions"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn list_actions_with_settings() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        // Create .zremote/settings.json with actions
        let settings_dir = dir.path().join(".zremote");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{
                "actions": [
                    {"name": "build", "command": "cargo build"},
                    {"name": "test", "command": "cargo test"}
                ]
            }"#,
        )
        .unwrap();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/projects/{project_id}/actions"))
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
        let actions = json["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0]["name"], "build");
        assert_eq!(actions[1]["name"], "test");
    }

    #[tokio::test]
    async fn run_action_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/actions/build/run"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_action_invalid_project_id() {
        let state = test_state().await;
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/projects/not-a-uuid/actions/build/run")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn run_action_no_settings() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        // Temp dir without settings file
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/actions/build/run"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // No settings file => 404 "no project settings found"
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_action_not_found() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        let settings_dir = dir.path().join(".zremote");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"actions": [{"name": "build", "command": "cargo build"}]}"#,
        )
        .unwrap();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/projects/{project_id}/actions/nonexistent/run"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_action_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        let settings_dir = dir.path().join(".zremote");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"actions": [{"name": "echo-test", "command": "echo hello"}]}"#,
        )
        .unwrap();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/actions/echo-test/run"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["action"], "echo-test");
        assert_eq!(json["command"], "echo hello");
        assert_eq!(json["status"], "active");
        assert!(json["session_id"].is_string());
        assert!(json["pid"].is_number());
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

    #[test]
    fn add_project_request_deserialize() {
        let json = r#"{"path": "/home/user/project"}"#;
        let req: AddProjectRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.path, "/home/user/project");
    }

    #[test]
    fn create_worktree_request_deserialize_minimal() {
        let json = r#"{"branch": "feature"}"#;
        let req: CreateWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.branch, "feature");
        assert!(req.path.is_none());
        assert!(req.new_branch.is_none());
    }

    #[test]
    fn create_worktree_request_deserialize_full() {
        let json = r#"{"branch": "feature", "path": "/tmp/wt", "new_branch": true}"#;
        let req: CreateWorktreeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.branch, "feature");
        assert_eq!(req.path.as_deref(), Some("/tmp/wt"));
        assert_eq!(req.new_branch, Some(true));
    }

    #[test]
    fn configure_request_deserialize_empty() {
        let json = r#"{}"#;
        let req: ConfigureRequest = serde_json::from_str(json).unwrap();
        assert!(req.model.is_none());
        assert!(req.skip_permissions.is_none());
    }

    #[test]
    fn configure_request_deserialize_full() {
        let json = r#"{"model": "opus", "skip_permissions": true}"#;
        let req: ConfigureRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model.as_deref(), Some("opus"));
        assert_eq!(req.skip_permissions, Some(true));
    }

    #[test]
    fn configure_request_deserialize_partial() {
        let json = r#"{"model": "sonnet"}"#;
        let req: ConfigureRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model.as_deref(), Some("sonnet"));
        assert!(req.skip_permissions.is_none());
    }

    #[test]
    fn run_action_request_deserialize_with_inputs() {
        let json = r#"{"inputs":{"tag":"0.2.4","message":"Release"}}"#;
        let body: RunActionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(body.inputs.get("tag").unwrap(), "0.2.4");
        assert_eq!(body.inputs.get("message").unwrap(), "Release");
    }

    #[test]
    fn run_action_request_deserialize_without_inputs() {
        let json = r#"{"worktree_path":"/tmp/wt"}"#;
        let body: RunActionRequest = serde_json::from_str(json).unwrap();
        assert!(body.inputs.is_empty());
    }

    #[tokio::test]
    async fn resolve_action_inputs_project_not_found() {
        let state = test_state().await;
        let project_id = Uuid::new_v4();
        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/projects/{project_id}/actions/build/resolve-inputs"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn resolve_action_inputs_action_not_found() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        let settings_dir = dir.path().join(".zremote");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"actions": [{"name": "build", "command": "cargo build"}]}"#,
        )
        .unwrap();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/projects/{project_id}/actions/nonexistent/resolve-inputs"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn resolve_action_inputs_success() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        let settings_dir = dir.path().join(".zremote");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"actions": [{"name": "deploy", "command": "echo deploy", "inputs": [{"name": "env", "label": "Environment", "options": ["staging", "production"]}]}]}"#,
        )
        .unwrap();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/projects/{project_id}/actions/deploy/resolve-inputs"
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
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let inputs = json["inputs"].as_array().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["name"], "env");
        let options = inputs[0]["options"].as_array().unwrap();
        assert_eq!(options.len(), 2);
    }

    #[tokio::test]
    async fn run_action_with_custom_inputs() {
        let state = test_state().await;
        let host_id = state.host_id.to_string();
        let project_id = Uuid::new_v4().to_string();

        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().to_str().unwrap().to_string();

        let settings_dir = dir.path().join(".zremote");
        std::fs::create_dir_all(&settings_dir).unwrap();
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"actions": [{"name": "tag", "command": "git tag {{tag}}"}]}"#,
        )
        .unwrap();

        q::insert_project(&state.db, &project_id, &host_id, &project_path, "test")
            .await
            .unwrap();

        let app = build_test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/projects/{project_id}/actions/tag/run"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"inputs":{"tag":"v1.0.0"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["command"], "git tag v1.0.0");
    }
}
