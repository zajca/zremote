use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use chrono::Utc;
use myremote_core::queries::config as q;
use myremote_core::queries::sessions as sq;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppJson};
use crate::state::AppState;

/// Config value response.
#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

/// Request body for setting a config value.
#[derive(Debug, Deserialize)]
pub struct SetConfigRequest {
    pub value: String,
}

/// `GET /api/config/:key` - get global config value.
pub async fn get_global_config(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<ConfigResponse>, AppError> {
    let (key, value, updated_at) = q::get_global_config(&state.db, &key)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("config key '{key}' not found")))?;

    Ok(Json(ConfigResponse {
        key,
        value,
        updated_at,
    }))
}

/// `PUT /api/config/:key` - set global config value.
pub async fn set_global_config(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    AppJson(body): AppJson<SetConfigRequest>,
) -> Result<Json<ConfigResponse>, AppError> {
    let now = Utc::now().to_rfc3339();
    q::set_global_config(&state.db, &key, &body.value, &now).await?;

    Ok(Json(ConfigResponse {
        key,
        value: body.value,
        updated_at: now,
    }))
}

/// `GET /api/hosts/:host_id/config/:key` - get host config value.
pub async fn get_host_config(
    State(state): State<Arc<AppState>>,
    Path((host_id, key)): Path<(String, String)>,
) -> Result<Json<ConfigResponse>, AppError> {
    let _parsed: uuid::Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    let (key, value, updated_at) = q::get_host_config(&state.db, &host_id, &key)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("config key '{key}' not found for host")))?;

    Ok(Json(ConfigResponse {
        key,
        value,
        updated_at,
    }))
}

/// `PUT /api/hosts/:host_id/config/:key` - set host config value.
pub async fn set_host_config(
    State(state): State<Arc<AppState>>,
    Path((host_id, key)): Path<(String, String)>,
    AppJson(body): AppJson<SetConfigRequest>,
) -> Result<Json<ConfigResponse>, AppError> {
    let _parsed: uuid::Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    if !sq::host_exists(&state.db, &host_id).await? {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    let now = Utc::now().to_rfc3339();
    q::set_host_config(&state.db, &host_id, &key, &body.value, &now).await?;

    Ok(Json(ConfigResponse {
        key,
        value: body.value,
        updated_at: now,
    }))
}
