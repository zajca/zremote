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
use zremote_core::state::ServerEvent;
use zremote_core::validation::validate_path_no_traversal;

use crate::local::state::LocalAppState;
use crate::project::scanner::ProjectScanner;

use super::{parse_host_id, parse_project_id};

pub type ProjectResponse = q::ProjectRow;
pub type SessionResponse = sq::SessionRow;

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

    validate_path_no_traversal(&body.path)?;

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

/// `GET /api/projects/:project_id` - get project detail.
pub async fn get_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
) -> Result<Json<ProjectResponse>, AppError> {
    let _parsed = parse_project_id(&project_id)?;
    let project = q::get_project(&state.db, &project_id).await?;
    Ok(Json(project))
}

#[derive(Debug, Deserialize)]
pub struct UpdateProjectRequest {
    pub pinned: Option<bool>,
}

/// `PATCH /api/projects/:project_id` - update project properties.
pub async fn update_project(
    State(state): State<Arc<LocalAppState>>,
    AxumPath(project_id): AxumPath<String>,
    AppJson(body): AppJson<UpdateProjectRequest>,
) -> Result<Json<ProjectResponse>, AppError> {
    let _parsed = parse_project_id(&project_id)?;

    if let Some(pinned) = body.pinned {
        let rows = q::set_project_pinned(&state.db, &project_id, pinned).await?;
        if rows == 0 {
            return Err(AppError::NotFound(format!(
                "project {project_id} not found"
            )));
        }
    }

    let project = q::get_project(&state.db, &project_id).await?;

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
