use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use myremote_core::queries::sessions as q;
use myremote_protocol::ServerMessage;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::{AppState, SessionState};

// Re-export the core row type so other modules (projects.rs) can use it.
pub type SessionResponse = q::SessionRow;

/// Request body for creating a new session.
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub shell: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub working_dir: Option<String>,
    pub name: Option<String>,
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

    if !q::host_exists(&state.db, &host_id).await? {
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
        q::resolve_project_id(&state.db, &host_id, wd).await?
    } else {
        None
    };

    q::insert_session(
        &state.db,
        &session_id_str,
        &host_id,
        body.name.as_deref(),
        body.working_dir.as_deref(),
        project_id.as_deref(),
    )
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

    let sessions = q::list_sessions(&state.db, &host_id).await?;
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

    let session = q::get_session(&state.db, &session_id).await?;
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

    q::update_session_name(&state.db, &session_id, body.name.as_deref()).await?;
    let session = q::get_session(&state.db, &session_id).await?;
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

    let (_id, host_id_str) = q::find_session_for_close(&state.db, &session_id)
        .await?
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
    match q::get_session_status(&state.db, &session_id).await? {
        None => return Err(AppError::NotFound(format!("session {session_id} not found"))),
        Some(ref s) if s != "closed" => {
            return Err(AppError::Conflict(format!(
                "session {session_id} is not closed (status: {s}), cannot purge"
            )));
        }
        _ => {}
    }

    q::purge_session(&state.db, &session_id).await?;

    Ok(StatusCode::NO_CONTENT)
}
