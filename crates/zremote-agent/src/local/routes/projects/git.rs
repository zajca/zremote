use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as AxumPath, State};
use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_core::queries::projects as q;

use crate::local::state::LocalAppState;
use crate::project::git::GitInspector;

use super::ProjectResponse;
use super::parse_project_id;

/// `POST /api/projects/:project_id/git/refresh` - call `GitInspector::inspect()` directly.
pub async fn trigger_git_refresh(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<ProjectResponse>, AppError> {
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
