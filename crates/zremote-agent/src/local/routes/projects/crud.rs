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
use zremote_core::services::projects as project_service;
use zremote_core::state::ServerEvent;
use zremote_core::validation::validate_path_no_traversal;

use crate::local::state::LocalAppState;
use crate::project::metadata;
use crate::project::scanner::ProjectScanner;

use super::parse_host_id;

pub type ProjectResponse = project_service::ProjectResponse;
pub type SessionResponse = project_service::SessionResponse;
pub type UpdateProjectRequest = project_service::UpdateProjectRequest;

/// Request body for manually adding a project.
#[derive(Debug, Deserialize)]
pub struct AddProjectRequest {
    pub path: String,
}

/// `GET /api/hosts/:host_id/projects` - list projects for a host.
pub async fn list_projects(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let projects = project_service::list_projects(&state.db, &host_id).await?;
    Ok(Json(projects))
}

/// `POST /api/hosts/:host_id/projects` - manually add a project.
pub async fn add_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(host_id): AxumPath<String>,
    AppJson(body): AppJson<AddProjectRequest>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed = parse_host_id(&host_id)?;

    validate_path_no_traversal(&body.path)?;

    // Check host existence BEFORE the FS probe so an unknown host fails with
    // 404 regardless of what path the caller supplied. Running the FS check
    // first would otherwise mask the real "host not registered" error behind
    // a 400 (bad path), which is what callers and tests key off of.
    if !sq::host_exists(&state.db, &host_id).await? {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    // Reject non-existent paths up front. Without this, the DB row is created
    // regardless, the sidebar shows a ghost project, and every later
    // operation (git refresh, list branches, worktree create) fails with
    // confusing errors — the user has to reach for sqlite to recover.
    let candidate = Path::new(&body.path);
    if !candidate.exists() {
        return Err(AppError::BadRequest(format!(
            "path does not exist on disk: {}",
            body.path
        )));
    }
    if !candidate.is_dir() {
        return Err(AppError::BadRequest(format!(
            "path is not a directory: {}",
            body.path
        )));
    }

    // Detect project info directly from filesystem on a blocking thread
    // (ProjectScanner::detect_at performs FS reads + `git` subprocess spawns).
    let body_path_owned = body.path.clone();
    let info =
        tokio::task::spawn_blocking(move || ProjectScanner::detect_at(Path::new(&body_path_owned)))
            .await
            .map_err(|e| AppError::Internal(format!("project detection task failed: {e}")))?;

    let project_id = Uuid::new_v4().to_string();
    let name = body
        .path
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string();

    // If this is a linked worktree, resolve or auto-register the parent project.
    let parent_project_id = match info.as_ref().and_then(|i| i.main_repo_path.as_ref()) {
        Some(main_path) => {
            match q::get_project_by_host_and_path(&state.db, &host_id, main_path).await {
                Ok(parent) => Some(parent.id),
                Err(AppError::Database(sqlx::Error::RowNotFound)) => {
                    let parent_id = Uuid::new_v4().to_string();
                    let parent_name = main_path
                        .rsplit('/')
                        .next()
                        .unwrap_or("unknown")
                        .to_string();
                    let inserted =
                        q::insert_project(&state.db, &parent_id, &host_id, main_path, &parent_name)
                            .await?;
                    // INSERT OR IGNORE: on a race a row already exists with a
                    // different UUID. Always re-fetch the canonical id so the
                    // worktree FK points at a real row rather than a phantom.
                    let canonical_parent_id = if inserted {
                        parent_id
                    } else {
                        q::get_project_by_host_and_path(&state.db, &host_id, main_path)
                            .await?
                            .id
                    };
                    let main_path_owned = main_path.clone();
                    let parent_info = tokio::task::spawn_blocking(move || {
                        ProjectScanner::detect_at(Path::new(&main_path_owned))
                    })
                    .await
                    .map_err(|e| {
                        AppError::Internal(format!("parent detection task failed: {e}"))
                    })?;
                    if let Some(parent_info) = parent_info {
                        metadata::update_from_info(&state.db, &canonical_parent_id, &parent_info)
                            .await?;
                    }
                    Some(canonical_parent_id)
                }
                Err(e) => return Err(e),
            }
        }
        None => None,
    };

    let inserted = if parent_project_id.is_some() {
        q::insert_project_with_parent(
            &state.db,
            &project_id,
            &host_id,
            &body.path,
            &name,
            parent_project_id.as_deref(),
            "worktree",
        )
        .await?
    } else {
        q::insert_project(&state.db, &project_id, &host_id, &body.path, &name).await?
    };
    if !inserted {
        return Err(AppError::Conflict(
            "project path already exists".to_string(),
        ));
    }

    // Update project metadata from detection
    if let Some(ref info) = info {
        metadata::update_from_info(&state.db, &project_id, info).await?;
    }

    let project = q::get_project_by_host_and_path(&state.db, &host_id, &body.path).await?;

    // Broadcast event
    let _ = state.events.send(ServerEvent::ProjectsUpdated {
        host_id: host_id.clone(),
    });

    Ok((StatusCode::CREATED, Json(project)))
}

/// `GET /api/projects/:project_id` - get project detail.
pub async fn get_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<ProjectResponse>, AppError> {
    let project = project_service::get_project(&state.db, &project_id).await?;
    Ok(Json(project))
}

/// `PATCH /api/projects/:project_id` - update project properties.
pub async fn update_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    AppJson(body): AppJson<UpdateProjectRequest>,
) -> Result<Json<ProjectResponse>, AppError> {
    let project = project_service::update_project(&state.db, &project_id, body).await?;

    // Broadcast event so sidebar refreshes
    let _ = state.events.send(ServerEvent::ProjectsUpdated {
        host_id: project.host_id.clone(),
    });

    Ok(Json(project))
}

/// `DELETE /api/projects/:project_id` - unregister project.
pub async fn delete_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<impl IntoResponse, AppError> {
    project_service::delete_project(&state.db, &project_id).await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/projects/:project_id/sessions` - sessions for a project.
pub async fn list_project_sessions(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let sessions = project_service::list_project_sessions(&state.db, &project_id).await?;
    Ok(Json(sessions))
}
