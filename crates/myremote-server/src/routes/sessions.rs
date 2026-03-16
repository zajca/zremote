use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_protocol::ServerMessage;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::{AppState, SessionState};

/// Request body for creating a new session.
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub shell: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub working_dir: Option<String>,
    pub name: Option<String>,
}

/// Session representation for API responses.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct SessionResponse {
    pub id: String,
    pub host_id: String,
    pub name: Option<String>,
    pub shell: Option<String>,
    pub status: String,
    pub working_dir: Option<String>,
    pub project_id: Option<String>,
    pub pid: Option<i64>,
    pub exit_code: Option<i32>,
    pub created_at: String,
    pub closed_at: Option<String>,
}

/// `POST /api/hosts/:host_id/sessions` - create a new terminal session.
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_host_id: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    // Check host exists in DB
    let host_exists: Option<(String,)> =
        sqlx::query_as("SELECT id FROM hosts WHERE id = ?")
            .bind(&host_id)
            .fetch_optional(&state.db)
            .await?;

    if host_exists.is_none() {
        return Err(AppError::NotFound(format!("host {host_id} not found")));
    }

    // Check agent is online
    let sender = state
        .connections
        .get_sender(&parsed_host_id)
        .await
        .ok_or_else(|| {
            AppError::Conflict("host is offline, cannot create session".to_string())
        })?;

    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();

    // Resolve project_id from working_dir
    let project_id: Option<String> = if let Some(ref wd) = body.working_dir {
        sqlx::query_scalar(
            "SELECT id FROM projects WHERE host_id = ? AND (? = path OR ? LIKE path || '/%') LIMIT 1",
        )
        .bind(&host_id)
        .bind(wd)
        .bind(wd)
        .fetch_optional(&state.db)
        .await?
    } else {
        None
    };

    // Insert into DB with status "creating"
    sqlx::query(
        "INSERT INTO sessions (id, host_id, name, status, working_dir, project_id) VALUES (?, ?, ?, 'creating', ?, ?)",
    )
    .bind(&session_id_str)
    .bind(&host_id)
    .bind(&body.name)
    .bind(&body.working_dir)
    .bind(&project_id)
    .execute(&state.db)
    .await?;

    // Create in-memory session state
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, parsed_host_id));
    }

    // Send SessionCreate to agent
    let msg = ServerMessage::SessionCreate {
        session_id,
        shell: body.shell,
        cols: body.cols,
        rows: body.rows,
        working_dir: body.working_dir,
    };

    if sender.send(msg).await.is_err() {
        // Agent disconnected between check and send
        return Err(AppError::Conflict(
            "host went offline, cannot create session".to_string(),
        ));
    }

    let response = serde_json::json!({
        "id": session_id_str,
        "status": "creating",
    });

    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/hosts/:host_id/sessions` - list sessions for a host.
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Path(host_id): Path<String>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let _parsed: Uuid = host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {host_id}")))?;

    let sessions: Vec<SessionResponse> = sqlx::query_as(
        "SELECT id, host_id, name, shell, status, working_dir, project_id, pid, exit_code, created_at, closed_at \
         FROM sessions WHERE host_id = ? ORDER BY created_at DESC",
    )
    .bind(&host_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(sessions))
}

/// `GET /api/sessions/:session_id` - get session detail.
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionResponse>, AppError> {
    let _parsed: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let session: SessionResponse = sqlx::query_as(
        "SELECT id, host_id, name, shell, status, working_dir, project_id, pid, exit_code, created_at, closed_at \
         FROM sessions WHERE id = ?",
    )
    .bind(&session_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("session {session_id} not found")))?;

    Ok(Json(session))
}

#[derive(Debug, Deserialize)]
pub struct UpdateSessionRequest {
    pub name: Option<String>,
}

/// `PATCH /api/sessions/:session_id` - update session metadata.
pub async fn update_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<UpdateSessionRequest>,
) -> Result<Json<SessionResponse>, AppError> {
    let _parsed: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    sqlx::query("UPDATE sessions SET name = ? WHERE id = ?")
        .bind(&body.name)
        .bind(&session_id)
        .execute(&state.db)
        .await?;

    let session: SessionResponse = sqlx::query_as(
        "SELECT id, host_id, name, shell, status, working_dir, project_id, pid, exit_code, created_at, closed_at \
         FROM sessions WHERE id = ?",
    )
    .bind(&session_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("session {session_id} not found")))?;

    Ok(Json(session))
}

/// `DELETE /api/sessions/:session_id` - close a session.
pub async fn close_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    // Look up session to find host_id
    let session: Option<(String, String)> =
        sqlx::query_as("SELECT id, host_id FROM sessions WHERE id = ? AND status != 'closed'")
            .bind(&session_id)
            .fetch_optional(&state.db)
            .await?;

    let (_id, host_id_str) = session
        .ok_or_else(|| AppError::NotFound(format!("session {session_id} not found or already closed")))?;

    let host_id: Uuid = host_id_str
        .parse()
        .map_err(|_| AppError::Internal("invalid host_id in database".to_string()))?;

    // Send SessionClose to agent if connected
    if let Some(sender) = state.connections.get_sender(&host_id).await {
        let msg = ServerMessage::SessionClose {
            session_id: parsed_session_id,
        };
        let _ = sender.send(msg).await;
    }

    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /api/sessions/:session_id/purge` - permanently delete a closed session.
pub async fn purge_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let _parsed: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    // Only allow purging closed sessions
    let status: Option<(String,)> =
        sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(&session_id)
            .fetch_optional(&state.db)
            .await?;

    match status {
        None => return Err(AppError::NotFound(format!("session {session_id} not found"))),
        Some((s,)) if s != "closed" => {
            return Err(AppError::Conflict(format!(
                "session {session_id} is not closed (status: {s}), cannot purge"
            )));
        }
        _ => {}
    }

    // Nullify session_id on agentic_loops (preserve loop data)
    sqlx::query("UPDATE agentic_loops SET session_id = NULL WHERE session_id = ?")
        .bind(&session_id)
        .execute(&state.db)
        .await?;

    // Delete session stats
    sqlx::query("DELETE FROM session_stats WHERE session_id = ?")
        .bind(&session_id)
        .execute(&state.db)
        .await?;

    // Delete the session row
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(&session_id)
        .execute(&state.db)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}
