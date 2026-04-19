use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_core::queries::projects as q;
use zremote_protocol::project::{BranchList, WorktreeError, WorktreeErrorCode};

/// Maximum wall time for the blocking `git for-each-ref` + rev-list calls
/// that back the branch listing endpoint. A pathological repo (huge ref
/// count, hung filesystem, broken credential helper) must not pin the
/// request thread forever.
const LIST_BRANCHES_TIMEOUT: Duration = Duration::from_secs(30);

use crate::local::state::LocalAppState;
use crate::project::git::GitInspector;

use super::ProjectResponse;
use super::parse_project_id;

/// `POST /api/projects/:project_id/git/refresh` - call `GitInspector::inspect()` directly.
pub async fn trigger_git_refresh(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<ProjectResponse>, AppError> {
    // Canonicalise the id so the DB query and any downstream logging all
    // see the same form (stringified UUID). parse_project_id rejects
    // malformed ids with 400 before we hit the DB.
    let project_id = parse_project_id(&project_id)?.to_string();

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

/// `GET /api/projects/:project_id/git/branches` — list local and remote
/// branches with ahead/behind counts against the current branch. Returns
/// empty lists for empty/fresh repos that have no commits yet.
pub async fn list_branches(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<axum::response::Response, AppError> {
    list_branches_with_timeout(state, project_id, LIST_BRANCHES_TIMEOUT).await
}

/// Inner implementation parameterised by timeout so tests can exercise the
/// 504 path without waiting 30 real seconds. Production callers use the
/// public handler which forwards `LIST_BRANCHES_TIMEOUT`.
pub(crate) async fn list_branches_with_timeout(
    state: Arc<LocalAppState>,
    project_id: String,
    timeout: Duration,
) -> Result<axum::response::Response, AppError> {
    // Canonicalise before the DB query so logs and the lookup see the same
    // UUID form. parse_project_id returns 400 for malformed ids.
    let project_id = parse_project_id(&project_id)?.to_string();

    let (_, path) = q::get_project_host_and_path(&state.db, &project_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("project {project_id} not found")))?;

    let path_clone = path.clone();
    let mut handle =
        tokio::task::spawn_blocking(move || GitInspector::list_branches(Path::new(&path_clone)));

    // Bound the wall time so a hung git (broken credential helper, massive
    // ref set, network-mounted repo) cannot pin the request thread. `&mut
    // handle` keeps ownership so we can abort on timeout.
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(join_result) => {
            let join = join_result
                .map_err(|e| AppError::Internal(format!("branch list task failed: {e}")))?;
            match join {
                Ok(result) => Ok(Json(result).into_response()),
                Err(stderr) => {
                    // Classify via the shared stderr→WorktreeError mapper so
                    // "path missing" surfaces to the GUI/CLI as a structured
                    // 404 with an actionable hint instead of a generic 500.
                    let classified = WorktreeError::from_git_stderr(&stderr);
                    if matches!(classified.code, WorktreeErrorCode::PathMissing) {
                        tracing::warn!(
                            project_id = %project_id,
                            error = %stderr,
                            "list_branches: project path missing"
                        );
                        return Ok((StatusCode::NOT_FOUND, Json(classified)).into_response());
                    }
                    Err(AppError::Internal(format!(
                        "failed to list branches: {stderr}"
                    )))
                }
            }
        }
        Err(_) => {
            handle.abort();
            tracing::warn!(
                project_id = %project_id,
                timeout_secs = timeout.as_secs(),
                "list_branches timed out"
            );
            Ok((
                StatusCode::GATEWAY_TIMEOUT,
                Json(serde_json::json!({
                    "error": "timeout",
                    "hint": "branches query timed out",
                })),
            )
                .into_response())
        }
    }
}
