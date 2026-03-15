use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use chrono::Utc;
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
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT key, value, updated_at FROM config_global WHERE key = ?")
            .bind(&key)
            .fetch_optional(&state.db)
            .await?;

    let (key, value, updated_at) =
        row.ok_or_else(|| AppError::NotFound(format!("config key '{key}' not found")))?;

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

    sqlx::query(
        "INSERT INTO config_global (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(&key)
    .bind(&body.value)
    .bind(&now)
    .execute(&state.db)
    .await?;

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

    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT key, value, updated_at FROM config_host WHERE host_id = ? AND key = ?",
    )
    .bind(&host_id)
    .bind(&key)
    .fetch_optional(&state.db)
    .await?;

    let (key, value, updated_at) = row
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

    // Verify host exists
    let host_exists: Option<(String,)> = sqlx::query_as("SELECT id FROM hosts WHERE id = ?")
        .bind(&host_id)
        .fetch_optional(&state.db)
        .await?;

    if host_exists.is_none() {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO config_host (host_id, key, value, updated_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(host_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(&host_id)
    .bind(&key)
    .bind(&body.value)
    .bind(&now)
    .execute(&state.db)
    .await?;

    Ok(Json(ConfigResponse {
        key,
        value: body.value,
        updated_at: now,
    }))
}
