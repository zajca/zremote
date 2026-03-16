use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use myremote_core::queries::hosts as q;
use serde::Deserialize;

use crate::error::{AppError, AppJson};
use crate::state::AppState;

// Re-export the core row type as the API response type.
pub type HostResponse = q::HostRow;

/// Request body for `PATCH /api/hosts/:host_id`.
#[derive(Debug, Deserialize)]
pub struct UpdateHostRequest {
    pub name: String,
}

/// GET /api/hosts - list all hosts.
pub async fn list_hosts(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<HostResponse>>, AppError> {
    let hosts = q::list_hosts(&state.db).await?;
    Ok(Json(hosts))
}

/// Parse and validate a host ID path parameter as UUID.
fn parse_host_id(host_id: &str) -> Result<uuid::Uuid, AppError> {
    host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))
}

/// Validate the update host request body.
fn validate_update_host(body: &UpdateHostRequest) -> Result<(), AppError> {
    if body.name.is_empty() {
        return Err(AppError::BadRequest("name must not be empty".to_string()));
    }
    if body.name.len() > 255 {
        return Err(AppError::BadRequest("name must not exceed 255 characters".to_string()));
    }
    Ok(())
}

/// `GET /api/hosts/:host_id` - get host detail.
pub async fn get_host(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<HostResponse>, AppError> {
    let _parsed = parse_host_id(&host_id)?;
    let host = q::get_host(&state.db, &host_id).await?;
    Ok(Json(host))
}

/// `PATCH /api/hosts/:host_id` - rename host.
pub async fn update_host(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
    AppJson(body): AppJson<UpdateHostRequest>,
) -> Result<Json<HostResponse>, AppError> {
    let _parsed = parse_host_id(&host_id)?;
    validate_update_host(&body)?;

    let now = Utc::now().to_rfc3339();
    let rows = q::update_host_name(&state.db, &host_id, &body.name, &now).await?;

    if rows == 0 {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    let host = q::get_host(&state.db, &host_id).await?;
    Ok(Json(host))
}

/// `DELETE /api/hosts/:host_id` - remove host.
pub async fn delete_host(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_id = parse_host_id(&host_id)?;

    if let Some(sender) = state.connections.get_sender(&parsed_id).await {
        let _ = sender
            .try_send(myremote_protocol::ServerMessage::Error {
                message: "host deleted".to_string(),
            });
        state.connections.unregister(&parsed_id).await;
    }

    let rows = q::delete_host(&state.db, &host_id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    Ok(StatusCode::NO_CONTENT)
}
