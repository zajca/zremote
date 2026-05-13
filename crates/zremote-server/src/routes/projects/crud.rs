use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use zremote_core::queries::projects as q;
use zremote_core::queries::sessions as sq;
use zremote_core::services::projects as project_service;

use crate::error::{AppError, AppJson};
use crate::state::{AppState, ServerEvent};

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
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<Vec<ProjectResponse>>, AppError> {
    let projects = project_service::list_projects(&state.db, &host_id).await?;
    Ok(Json(projects))
}

/// `POST /api/hosts/:host_id/projects` - manually add a project.
pub async fn add_project(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
    AppJson(body): AppJson<AddProjectRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed = parse_host_id(&host_id)?;

    if body.path.is_empty() {
        return Err(AppError::BadRequest("path must not be empty".to_string()));
    }

    zremote_core::validation::validate_path_no_traversal(&body.path)?;

    if !sq::host_exists(&state.db, &host_id).await? {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    // Send ProjectRegister to agent to validate and discover project info
    if let Some(sender) = state.connections.get_sender(&parsed).await {
        let _ = sender
            .send(zremote_protocol::ServerMessage::ProjectRegister {
                path: body.path.clone(),
            })
            .await;
    }

    let project_id = uuid::Uuid::new_v4().to_string();
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

    let project = q::get_project_by_host_and_path(&state.db, &host_id, &body.path).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `GET /api/projects/:project_id` - get project detail.
pub async fn get_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectResponse>, AppError> {
    let project = project_service::get_project(&state.db, &project_id).await?;
    Ok(Json(project))
}

/// `GET /api/projects/:project_id/sessions` - list sessions linked to a project.
pub async fn list_project_sessions(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let sessions = project_service::list_project_sessions(&state.db, &project_id).await?;
    Ok(Json(sessions))
}

/// `PATCH /api/projects/:project_id` - update project properties.
pub async fn update_project(
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
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
    State(state): State<Arc<AppState>>,
    Path(project_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    if let Some(target) = project_service::project_removal_target(&state.db, &project_id).await?
        && let Ok(host_id) = target.host_id.parse::<uuid::Uuid>()
        && let Some(sender) = state.connections.get_sender(&host_id).await
    {
        let _ = sender
            .send(zremote_protocol::ServerMessage::ProjectRemove { path: target.path })
            .await;
    }

    project_service::delete_project(&state.db, &project_id).await?;

    Ok(StatusCode::NO_CONTENT)
}
