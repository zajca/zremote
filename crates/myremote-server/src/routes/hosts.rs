use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppJson};
use crate::state::AppState;

/// Host representation for API responses.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct HostResponse {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub status: String,
    pub last_seen_at: Option<String>,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Request body for `PATCH /api/hosts/:host_id`.
#[derive(Debug, Deserialize)]
pub struct UpdateHostRequest {
    pub name: String,
}

/// GET /api/hosts - list all hosts.
pub async fn list_hosts(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<HostResponse>>, AppError> {
    let hosts: Vec<HostResponse> = sqlx::query_as(
        "SELECT id, name, hostname, status, last_seen_at, agent_version, os, arch, \
         created_at, updated_at FROM hosts ORDER BY name",
    )
    .fetch_all(&state.db)
    .await?;

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

    let host: HostResponse = sqlx::query_as(
        "SELECT id, name, hostname, status, last_seen_at, agent_version, os, arch, \
         created_at, updated_at FROM hosts WHERE id = ?",
    )
    .bind(&host_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("host {host_id} not found")))?;

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

    let result = sqlx::query("UPDATE hosts SET name = ?, updated_at = ? WHERE id = ?")
        .bind(&body.name)
        .bind(&now)
        .bind(&host_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    // Return the updated host
    let host: HostResponse = sqlx::query_as(
        "SELECT id, name, hostname, status, last_seen_at, agent_version, os, arch, \
         created_at, updated_at FROM hosts WHERE id = ?",
    )
    .bind(&host_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(host))
}

/// `DELETE /api/hosts/:host_id` - remove host.
pub async fn delete_host(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // If agent is connected, close the WebSocket by dropping the sender
    let parsed_id = parse_host_id(&host_id)?;

    if let Some(sender) = state.connections.get_sender(&parsed_id).await {
        // Send an error message to notify the agent before disconnecting
        let _ = sender
            .try_send(myremote_protocol::ServerMessage::Error {
                message: "host deleted".to_string(),
            });
        // Unregister will drop the sender, closing the channel
        state.connections.unregister(&parsed_id).await;
    }

    let result = sqlx::query("DELETE FROM hosts WHERE id = ?")
        .bind(&host_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    Ok(StatusCode::NO_CONTENT)
}
