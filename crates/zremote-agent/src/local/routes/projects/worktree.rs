use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::error::{AppError, AppJson};
use zremote_core::queries::projects as q;
use zremote_core::queries::sessions as sq;
use zremote_core::state::{ServerEvent, SessionState};

use crate::local::state::LocalAppState;
use crate::project::git::GitInspector;

use super::ProjectResponse;
use super::parse_project_id;
use zremote_protocol::events::WorktreeCreationStage;
use zremote_protocol::project::{WorktreeError, WorktreeErrorCode};

/// Maximum wall time for the blocking git worktree add call. Chosen to
/// tolerate large-repo worktree creation (where git's staged checkout can
/// legitimately take tens of seconds) while still putting a hard ceiling on
/// the request so the client isn't left hanging forever.
const WORKTREE_CREATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Reject user-controlled git inputs that start with `-`. Without this guard
/// a caller could smuggle additional git options through the worktree
/// endpoint (CWE-88) — for example, passing `--upload-pack=evil` as a branch
/// name. Enforced at the API boundary so the check is centralised and the
/// git layer can assume its arguments are safe.
fn reject_leading_dash(field: &str, value: &str) -> Result<(), WorktreeError> {
    if value.starts_with('-') {
        return Err(WorktreeError::new(
            WorktreeErrorCode::InvalidRef,
            format!("{field} must not start with '-'"),
            format!("rejected {field}: leading dash not allowed"),
        ));
    }
    Ok(())
}

/// Emit a `WorktreeCreationProgress` event for the given job/stage. Broadcast
/// is best-effort — a full broadcast channel should not abort the operation.
fn emit_progress(
    state: &LocalAppState,
    project_id: &str,
    job_id: &str,
    stage: WorktreeCreationStage,
    percent: u8,
    message: Option<String>,
) {
    let _ = state.events.send(ServerEvent::WorktreeCreationProgress {
        project_id: project_id.to_string(),
        job_id: job_id.to_string(),
        stage,
        percent,
        message,
    });
}

/// Map a `WorktreeErrorCode` to the HTTP status that best conveys the class of
/// failure. We keep 500 for true internal errors so monitoring/alerting can
/// still distinguish them, but use 4xx for issues the caller can correct.
fn status_for_code(code: &WorktreeErrorCode) -> StatusCode {
    match code {
        WorktreeErrorCode::BranchExists | WorktreeErrorCode::PathCollision => StatusCode::CONFLICT,
        WorktreeErrorCode::DetachedHead
        | WorktreeErrorCode::Locked
        | WorktreeErrorCode::Unmerged
        | WorktreeErrorCode::InvalidRef => StatusCode::BAD_REQUEST,
        WorktreeErrorCode::Internal | WorktreeErrorCode::Unknown => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Build a JSON response body for a structured worktree error.
fn worktree_error_response(err: WorktreeError) -> axum::response::Response {
    let status = status_for_code(&err.code);
    (status, Json(err)).into_response()
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

/// Request body for creating a worktree.
#[derive(Debug, Deserialize)]
pub struct CreateWorktreeRequest {
    pub branch: String,
    pub path: Option<String>,
    pub new_branch: Option<bool>,
    /// Optional base ref (commit SHA, branch, or tag) to create the new branch
    /// from. Only meaningful when `new_branch` is `true`. When `None`, git
    /// uses the current HEAD of the repo.
    #[serde(default)]
    pub base_ref: Option<String>,
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
                                        let _ = q::insert_project_with_parent(
                                            &db,
                                            &wt_id,
                                            &hid,
                                            &wt.path,
                                            &wt_name,
                                            Some(&pid),
                                            "worktree",
                                        )
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

    // Default flow: existing GitInspector behavior, wrapped in a 60s timeout
    // and bracketed by progress events so the GUI can show a pending spinner
    // for large-repo creations.
    let branch = body.branch.clone();
    let wt_path = body.path.clone();
    let new_branch = body.new_branch.unwrap_or(false);
    let base_ref = body.base_ref.clone();
    let repo_path = project_path.clone();

    // Validate user-controlled git inputs at the API boundary. Leading-dash
    // values would otherwise be interpreted as additional git options
    // (CWE-88). Rejected inputs never reach the blocking task.
    if let Err(err) = reject_leading_dash("branch", &branch) {
        return Ok(worktree_error_response(err));
    }
    if let Some(ref p) = wt_path
        && let Err(err) = reject_leading_dash("path", p)
    {
        return Ok(worktree_error_response(err));
    }
    if let Some(ref b) = base_ref
        && let Err(err) = reject_leading_dash("base_ref", b)
    {
        return Ok(worktree_error_response(err));
    }

    let job_id = Uuid::new_v4().to_string();
    emit_progress(
        &state,
        &project_id,
        &job_id,
        WorktreeCreationStage::Init,
        0,
        None,
    );

    // Emit Creating from *inside* the blocking task so the event fires when
    // git actually starts running, not at the moment we scheduled it. That
    // gives the GUI a progress signal that reflects reality under load.
    let events_for_task = state.events.clone();
    let project_id_for_task = project_id.clone();
    let job_id_for_task = job_id.clone();

    let mut handle = tokio::task::spawn_blocking(move || {
        // Best-effort broadcast: a full channel must not abort the git call.
        let _ = events_for_task.send(ServerEvent::WorktreeCreationProgress {
            project_id: project_id_for_task,
            job_id: job_id_for_task,
            stage: WorktreeCreationStage::Creating,
            percent: 25,
            message: Some("running git worktree add".to_string()),
        });
        GitInspector::create_worktree(
            Path::new(&repo_path),
            &branch,
            wt_path.as_deref().map(Path::new),
            new_branch,
            base_ref.as_deref(),
        )
    });

    // Passing `&mut handle` keeps ownership of the JoinHandle so we can abort
    // the task if the timeout fires; otherwise the handle would be moved into
    // the timeout future and we would leak the spawned task on timeout.
    let join_result = match tokio::time::timeout(WORKTREE_CREATE_TIMEOUT, &mut handle).await {
        Ok(res) => {
            res.map_err(|e| AppError::Internal(format!("worktree create task failed: {e}")))?
        }
        Err(_) => {
            // Timeout fired: tell tokio to drop the blocking task (it will
            // exit on next yield; synchronous git cannot be preempted, but
            // the HTTP caller gets a prompt structured error either way).
            handle.abort();
            tracing::warn!(
                job_id = %job_id,
                timeout_secs = WORKTREE_CREATE_TIMEOUT.as_secs(),
                "worktree create timed out"
            );
            emit_progress(
                &state,
                &project_id,
                &job_id,
                WorktreeCreationStage::Failed,
                100,
                Some(format!(
                    "timed out after {}s",
                    WORKTREE_CREATE_TIMEOUT.as_secs()
                )),
            );
            return Ok(worktree_error_response(WorktreeError::new(
                WorktreeErrorCode::Internal,
                "Worktree creation timed out — the repository may be very large or git may be stuck.",
                format!("timed out after {}s", WORKTREE_CREATE_TIMEOUT.as_secs()),
            )));
        }
    };

    let result = match join_result {
        Ok(info) => info,
        Err(stderr) => {
            tracing::warn!(error = %stderr, job_id = %job_id, "worktree create failed");
            let err = WorktreeError::from_git_stderr(&stderr);
            emit_progress(
                &state,
                &project_id,
                &job_id,
                WorktreeCreationStage::Failed,
                100,
                Some(err.message.clone()),
            );
            return Ok(worktree_error_response(err));
        }
    };

    // git has returned successfully; the work left is DB insert + on_create
    // hook. Surface that as Finalizing so the GUI can show a "wrapping up"
    // state distinct from the active git call.
    emit_progress(
        &state,
        &project_id,
        &job_id,
        WorktreeCreationStage::Finalizing,
        75,
        None,
    );

    // Insert worktree as a child project
    let wt_id = Uuid::new_v4().to_string();
    let wt_name = result
        .path
        .rsplit('/')
        .next()
        .unwrap_or("worktree")
        .to_string();

    q::insert_project_with_parent(
        &state.db,
        &wt_id,
        &host_id_str,
        &result.path,
        &wt_name,
        Some(&project_id),
        "worktree",
    )
    .await?;

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

    emit_progress(
        &state,
        &project_id,
        &job_id,
        WorktreeCreationStage::Done,
        100,
        None,
    );

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

/// Read worktree settings for a project, if configured.
pub(super) async fn read_worktree_settings(
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
pub(super) async fn spawn_command_session(
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

    let shell = super::super::sessions::default_shell();

    {
        let parsed_host_id: Uuid = host_id_str
            .parse()
            .map_err(|_| AppError::Internal("invalid host_id".to_string()))?;
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, parsed_host_id));
    }

    let manual_config = crate::pty::shell_integration::ShellIntegrationConfig::for_manual_session();
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(
            session_id,
            shell,
            80,
            24,
            Some(working_dir),
            None,
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
            host_id: host_id_str.to_string(),
            shell: Some(shell.to_string()),
            status: zremote_protocol::status::SessionStatus::Active,
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
pub(super) async fn run_worktree_hook(
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
