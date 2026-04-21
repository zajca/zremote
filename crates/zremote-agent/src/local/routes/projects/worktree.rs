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
use zremote_core::state::ServerEvent;

use crate::local::state::LocalAppState;
use crate::project::action_runner::ActionRunContext;
use crate::project::git::GitInspector;
use crate::project::hook_dispatcher::{WorktreeSlot, run_worktree_hook, run_worktree_override};

use super::ProjectResponse;
use super::parse_project_id;
use zremote_protocol::ProjectSettings;
use zremote_protocol::events::WorktreeCreationStage;
use zremote_protocol::project::{WorktreeError, WorktreeErrorCode};

/// Maximum wall time for the blocking git worktree add call. Chosen to
/// tolerate large-repo worktree creation (where git's staged checkout can
/// legitimately take tens of seconds) while still putting a hard ceiling on
/// the request so the client isn't left hanging forever.
const WORKTREE_CREATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Maximum wall time for the `pre_delete` captured hook. The delete handler
/// blocks on this hook before touching git, so an unbounded run would pin the
/// HTTP request indefinitely (a stuck teardown script would hang the GUI).
/// 2 minutes matches typical Docker/compose teardowns while still guaranteeing
/// forward progress.
const PRE_DELETE_HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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
        // The project directory is gone — the caller has to fix the project
        // registration, not the worktree inputs. 404 matches the semantics
        // (the referenced resource no longer exists on this host).
        WorktreeErrorCode::PathMissing => StatusCode::NOT_FOUND,
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

/// Read full project settings via spawn_blocking. Returns `None` when no
/// settings file exists or reading fails (treated as "no hooks configured").
async fn read_project_settings(project_path: &str) -> Option<ProjectSettings> {
    let pp = project_path.to_string();
    tokio::task::spawn_blocking(move || crate::project::settings::read_settings(Path::new(&pp)))
        .await
        .ok()?
        .ok()
        .flatten()
}

fn log_hook_result(slot: &str, target: &str, hr: &zremote_protocol::HookResultInfo) {
    if hr.success {
        tracing::info!(slot, worktree = target, "worktree hook succeeded");
    } else {
        tracing::warn!(
            slot,
            worktree = target,
            output = %hr.output.as_deref().unwrap_or(""),
            "worktree hook failed"
        );
    }
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

    // Validate user-controlled git inputs *before* any path branches. Without
    // this guard, a caller with a configured `hooks.worktree.create` could
    // smuggle leading-dash values through `{{branch}}` into the override
    // action's command line (CWE-88). Enforcing at the entry point means the
    // check cannot be bypassed by any downstream code path.
    if let Err(err) = reject_leading_dash("branch", &body.branch) {
        return Ok(worktree_error_response(err));
    }
    if let Some(ref p) = body.path
        && let Err(err) = reject_leading_dash("path", p)
    {
        return Ok(worktree_error_response(err));
    }
    if let Some(ref b) = body.base_ref
        && let Err(err) = reject_leading_dash("base_ref", b)
    {
        return Ok(worktree_error_response(err));
    }

    // Read settings once up front — covers both the create override resolution
    // and the post_create hook that fires after a successful default flow.
    let settings = read_project_settings(&project_path).await;

    // Check for custom create override
    if let Some(ref sett) = settings {
        let worktree_name = body.branch.replace('/', "-");
        let ctx = ActionRunContext {
            project_path: project_path.clone(),
            worktree_path: None,
            branch: Some(body.branch.clone()),
            worktree_name: Some(worktree_name.clone()),
            inputs: std::collections::HashMap::new(),
        };
        let session_name = format!("worktree: create {worktree_name}");
        let spawned = run_worktree_override(
            &state,
            &host_id_str,
            sett,
            WorktreeSlot::Create,
            ctx,
            &session_name,
        )
        .await?;

        if let Some(spawned) = spawned {
            let session_id_str = spawned.session_id.clone();

            // Background task: monitor session completion, then update DB and
            // fire the post_create captured hook.
            let events = state.events.clone();
            let db = state.db.clone();
            let sid = session_id_str.clone();
            let pp = project_path.clone();
            let hid = host_id_str.clone();
            let pid = project_id.clone();
            let branch = body.branch.clone();
            let settings_for_task = sett.clone();
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
                                    let existing =
                                        q::list_worktrees(&db, &pid).await.unwrap_or_default();
                                    let existing_paths: HashSet<String> =
                                        existing.iter().map(|w| w.path.clone()).collect();

                                    let mut created_wt: Option<(String, Option<String>)> = None;
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

                                            if created_wt.is_none() {
                                                created_wt =
                                                    Some((wt.path.clone(), wt.branch.clone()));
                                            }
                                        }
                                    }

                                    let _ = events.send(ServerEvent::ProjectsUpdated {
                                        host_id: hid.clone(),
                                    });

                                    // Fire post_create captured hook for the new worktree
                                    if let Some((wt_path, wt_branch)) = created_wt {
                                        let wt_name = Path::new(&wt_path)
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .map(String::from);
                                        let ctx = ActionRunContext {
                                            project_path: pp.clone(),
                                            worktree_path: Some(wt_path.clone()),
                                            branch: wt_branch.or(Some(branch.clone())),
                                            worktree_name: wt_name,
                                            inputs: std::collections::HashMap::new(),
                                        };
                                        match run_worktree_hook(
                                            &settings_for_task,
                                            WorktreeSlot::PostCreate,
                                            ctx,
                                            None,
                                        )
                                        .await
                                        {
                                            Ok(Some(hr)) => {
                                                log_hook_result("post_create", &wt_path, &hr);
                                            }
                                            Ok(None) => {}
                                            Err(e) => {
                                                tracing::warn!(
                                                    worktree = %wt_path,
                                                    error = %e,
                                                    "post_create hook resolution failed"
                                                );
                                            }
                                        }
                                    }
                                }
                            } else {
                                tracing::warn!(
                                    session_id = %sid,
                                    exit_code = ?exit_code,
                                    "worktree create override failed"
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
    }

    // Default flow: existing GitInspector behavior, wrapped in a 60s timeout
    // and bracketed by progress events so the GUI can show a pending spinner
    // for large-repo creations. Leading-dash inputs were already rejected at
    // the top of the handler so the blocking task can trust these values.
    let branch = body.branch.clone();
    let wt_path = body.path.clone();
    let new_branch = body.new_branch.unwrap_or(false);
    let base_ref = body.base_ref.clone();
    let repo_path = project_path.clone();

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

    // git has returned successfully; the work left is DB insert + post_create
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

    sqlx::query("UPDATE projects SET git_branch = ?, git_commit_hash = ? WHERE id = ?")
        .bind(&result.branch)
        .bind(&result.commit_hash)
        .bind(&wt_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    // Run post_create captured hook (new or legacy on_create) if configured.
    // Resolution errors (e.g. hook references a missing action) surface as
    // 400s so misconfiguration is visible immediately, not silently dropped.
    let hook_result = if let Some(ref sett) = settings {
        let ctx = ActionRunContext {
            project_path: project_path.clone(),
            worktree_path: Some(result.path.clone()),
            branch: result.branch.clone(),
            worktree_name: Some(wt_name.clone()),
            inputs: std::collections::HashMap::new(),
        };
        run_worktree_hook(sett, WorktreeSlot::PostCreate, ctx, None).await?
    } else {
        None
    };

    if let Some(ref hr) = hook_result {
        log_hook_result("post_create", &result.path, hr);
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

    // Gracefully close any active sessions bound to this worktree before we
    // touch the filesystem. Each session is shut down via the same code path
    // as `DELETE /api/sessions/:id` so the GUI observes a consistent
    // `SessionClosed` event instead of a terminal that silently stops
    // responding.
    let closed = crate::local::routes::sessions::close_sessions_for_project(
        &state,
        &host_id_str,
        &worktree_id,
        Some(&worktree_path),
    )
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(
            worktree_id = %worktree_id,
            error = %e,
            "failed to close sessions for worktree before delete"
        );
        0
    });
    if closed > 0 {
        tracing::info!(
            worktree_id = %worktree_id,
            count = closed,
            "closed active sessions before worktree deletion"
        );
    }

    // Read settings once — drives both pre_delete hook and delete override.
    let settings = read_project_settings(&project_path).await;

    // Branch (if any) used for hook context.
    let wt_branch: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT git_branch FROM projects WHERE id = ?")
            .bind(&worktree_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .flatten();

    let wt_name = Path::new(&worktree_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(String::from);

    // Always run pre_delete hook first (if any). A stuck teardown would
    // otherwise block the delete request indefinitely, so we impose a hard
    // wall-time ceiling (PRE_DELETE_HOOK_TIMEOUT). Command failure and timeout
    // are logged but do not abort the delete — the caller asked for deletion.
    // Resolution failure (missing action) *does* abort with 400 so the
    // misconfiguration surfaces.
    if let Some(ref sett) = settings {
        let ctx = ActionRunContext {
            project_path: project_path.clone(),
            worktree_path: Some(worktree_path.clone()),
            branch: wt_branch.clone(),
            worktree_name: wt_name.clone(),
            inputs: std::collections::HashMap::new(),
        };
        if let Some(hr) = run_worktree_hook(
            sett,
            WorktreeSlot::PreDelete,
            ctx,
            Some(PRE_DELETE_HOOK_TIMEOUT),
        )
        .await?
        {
            log_hook_result("pre_delete", &worktree_path, &hr);
        }
    }

    // Delete override
    if let Some(ref sett) = settings {
        let ctx = ActionRunContext {
            project_path: project_path.clone(),
            worktree_path: Some(worktree_path.clone()),
            branch: wt_branch.clone(),
            worktree_name: wt_name.clone(),
            inputs: std::collections::HashMap::new(),
        };
        let session_name = format!(
            "worktree: delete {}",
            wt_name.as_deref().unwrap_or("worktree")
        );
        let spawned = run_worktree_override(
            &state,
            &host_id_str,
            sett,
            WorktreeSlot::Delete,
            ctx,
            &session_name,
        )
        .await?;

        if let Some(spawned) = spawned {
            let session_id_str = spawned.session_id.clone();

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
                                    "worktree delete override failed"
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
    }

    // Default flow
    let repo = project_path.clone();
    let wt = worktree_path.clone();

    // Never auto-escalate to `git worktree remove --force`: force would
    // silently discard any uncommitted changes in the worktree. We already
    // closed every session inside the worktree above, so the typical
    // remaining failure reason is a dirty tree — which the user should see
    // and decide about explicitly.
    tokio::task::spawn_blocking(move || {
        GitInspector::remove_worktree(Path::new(&repo), Path::new(&wt), false)
    })
    .await
    .map_err(|e| AppError::Internal(format!("worktree delete task failed: {e}")))?
    .map_err(|e| AppError::BadRequest(format!("failed to delete worktree: {e}")))?;

    q::delete_project(&state.db, &worktree_id).await?;

    let _ = state.events.send(ServerEvent::ProjectsUpdated {
        host_id: host_id_str.clone(),
    });

    Ok(StatusCode::NO_CONTENT.into_response())
}
