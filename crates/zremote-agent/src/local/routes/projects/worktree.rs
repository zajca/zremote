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
use crate::worktree::service::{WorktreeCreateFailure, WorktreeCreateInput, run_worktree_create};

use super::ProjectResponse;
use super::parse_project_id;
use zremote_protocol::ProjectSettings;
use zremote_protocol::events::WorktreeCreationStage;
use zremote_protocol::project::{WorktreeError, WorktreeErrorCode};

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
pub(crate) fn reject_leading_dash(field: &str, value: &str) -> Result<(), WorktreeError> {
    if value.starts_with('-') {
        return Err(WorktreeError::new(
            WorktreeErrorCode::InvalidRef,
            format!("{field} must not start with '-'"),
            format!("rejected {field}: leading dash not allowed"),
        ));
    }
    Ok(())
}

/// Build a JSON response body for a structured worktree error. Delegates to
/// the shared helper in `zremote-core` so local-mode and server-mode use one
/// status-code mapping.
fn worktree_error_response(err: WorktreeError) -> axum::response::Response {
    let (status, body) = zremote_core::worktree_http::worktree_error_response(err);
    (status, body).into_response()
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

    // Default flow: delegate to the shared helper. The helper runs the
    // CWE-88 leading-dash guard (defence-in-depth), emits Init/Creating/
    // Finalizing/Done progress through the callback, bounds the git call
    // with the shared `WORKTREE_CREATE_TIMEOUT`, and fires `post_create`
    // when settings configure one.
    //
    // HTTP-only concerns kept in this handler:
    //   * translating the callback into a broadcast `ServerEvent`,
    //   * DB insert + `UPDATE projects`,
    //   * mapping the structured error to an HTTP status.
    let job_id = Uuid::new_v4().to_string();
    let events_cb = state.events.clone();
    let project_id_cb = project_id.clone();
    let job_id_cb = job_id.clone();

    let service_input = WorktreeCreateInput {
        project_path: std::path::PathBuf::from(&project_path),
        branch: body.branch.clone(),
        path: body.path.as_deref().map(std::path::PathBuf::from),
        new_branch: body.new_branch.unwrap_or(false),
        base_ref: body.base_ref.clone(),
    };

    let service_result = run_worktree_create(service_input, move |stage, percent, message| {
        // Best-effort broadcast — a full channel must not abort the git call.
        let _ = events_cb.send(ServerEvent::WorktreeCreationProgress {
            project_id: project_id_cb.clone(),
            job_id: job_id_cb.clone(),
            stage,
            percent,
            message,
        });
    })
    .await;

    let output = match service_result {
        Ok(out) => out,
        Err(WorktreeCreateFailure::Structured(err)) => {
            return Ok(worktree_error_response(err));
        }
        Err(WorktreeCreateFailure::Timeout { seconds }) => {
            tracing::warn!(job_id = %job_id, timeout_secs = seconds, "worktree create timed out");
            return Ok(worktree_error_response(WorktreeError::new(
                WorktreeErrorCode::Internal,
                "Worktree creation timed out — the repository may be very large or git may be stuck.",
                format!("timed out after {seconds}s"),
            )));
        }
    };

    let wt_id = Uuid::new_v4().to_string();
    let wt_name = output
        .path
        .rsplit('/')
        .next()
        .unwrap_or("worktree")
        .to_string();

    q::insert_project_with_parent(
        &state.db,
        &wt_id,
        &host_id_str,
        &output.path,
        &wt_name,
        Some(&project_id),
        "worktree",
    )
    .await?;

    sqlx::query("UPDATE projects SET git_branch = ?, git_commit_hash = ? WHERE id = ?")
        .bind(&output.branch)
        .bind(&output.commit_hash)
        .bind(&wt_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    if let Some(ref hr) = output.hook_result {
        log_hook_result("post_create", &output.path, hr);
    }

    let mut project = serde_json::to_value(q::get_project(&state.db, &wt_id).await?)
        .map_err(|e| AppError::Internal(format!("serialization error: {e}")))?;
    if let Some(ref hr) = output.hook_result {
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
