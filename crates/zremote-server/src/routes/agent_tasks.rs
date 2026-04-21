//! `POST /api/agent-tasks` - generic profile-driven launch (server mode).
//!
//! The profile-aware equivalent of `POST /api/claude-tasks`. The server:
//! 1. Resolves the profile by id from its SQLite database,
//! 2. Validates that the target host exists and is online,
//! 3. Inserts a `sessions` row so the WS terminal stream has something to
//!    attach to,
//! 4. Dispatches [`AgentServerMessage::StartAgent`] over the host's WS
//!    connection — the agent does the PTY spawn and calls the launcher.
//!
//! All kind-specific behavior (claude channel auto-approve, command
//! construction, settings_json validation) happens on the agent side inside
//! [`zremote_agent::agents::ClaudeLauncher`]. Keeping the server ignorant of
//! launcher internals is what makes "add a new agent kind without touching
//! the server" possible — the server only knows the generic protocol.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;
use zremote_core::queries::agent_profiles as q;
use zremote_core::queries::claude_sessions as cq;
use zremote_core::queries::sessions as sq;
use zremote_protocol::ServerMessage;
use zremote_protocol::agents::{AgentProfileData, AgentServerMessage};

use crate::error::{AppError, AppJson};
use crate::state::{AppState, SessionState};

#[derive(Debug, Deserialize)]
pub struct CreateAgentTaskRequest {
    pub host_id: String,
    pub profile_id: String,
    pub project_path: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CreateAgentTaskResponse {
    pub session_id: String,
    pub task_id: String,
    pub agent_kind: String,
    pub profile_id: String,
    pub host_id: String,
    pub project_path: String,
}

/// `POST /api/agent-tasks` - dispatch a profile-driven launch to a host.
///
/// Error mapping:
/// - 400 if `host_id` is not a valid UUID
/// - 404 if the host is unknown or the profile does not exist
/// - 409 if the host is registered but currently offline
/// - 400 if the profile's `agent_kind` is unsupported (stale row that
///   predates the current `SUPPORTED_KINDS` list)
///
/// The agent replies asynchronously with
/// [`AgentLifecycleMessage`](zremote_protocol::agents::AgentLifecycleMessage);
/// the REST response only confirms that the `StartAgent` was queued for
/// delivery. Clients that need per-session lifecycle feedback subscribe to
/// the events WebSocket.
pub async fn create_agent_task(
    State(state): State<Arc<AppState>>,
    AppJson(body): AppJson<CreateAgentTaskRequest>,
) -> Result<impl IntoResponse, AppError> {
    let parsed_host_id: Uuid = body
        .host_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid host ID: {}", body.host_id)))?;

    // Reject path traversal before hitting the DB or dispatching over WS.
    zremote_core::validation::validate_path_no_traversal(&body.project_path)?;

    if !sq::host_exists(&state.db, &body.host_id).await? {
        return Err(AppError::NotFound(format!(
            "host {} not found",
            body.host_id
        )));
    }

    // Load profile
    let profile = q::get_profile(&state.db, &body.profile_id)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!("agent profile {} not found", body.profile_id))
        })?;

    // Kind sanity check — the profile may have been saved under an older
    // `SUPPORTED_KINDS` list. Validate against the current list so we fail
    // fast with 400 rather than sending a dispatch the agent will reject.
    let supported = zremote_protocol::agents::supported_kinds();
    if !supported.contains(&profile.agent_kind.as_str()) {
        return Err(AppError::BadRequest(format!(
            "unsupported agent kind: {}",
            profile.agent_kind
        )));
    }

    // Look up WS sender for the target host
    let sender = state
        .connections
        .get_sender(&parsed_host_id)
        .await
        .ok_or_else(|| {
            AppError::Conflict("host is offline, cannot start agent task".to_string())
        })?;

    let session_id = Uuid::new_v4();
    let session_id_str = session_id.to_string();

    // Resolve project id lazily — reuse `claude_sessions` helper since the
    // DB schema is the same.
    let project_id = match body.project_id.as_ref() {
        Some(id) => Some(id.clone()),
        None => {
            cq::resolve_project_id_by_path(&state.db, &body.host_id, &body.project_path).await?
        }
    };

    // Insert session row
    cq::insert_session_for_task(
        &state.db,
        &session_id_str,
        &body.host_id,
        &body.project_path,
        project_id.as_deref(),
    )
    .await?;

    // Register in-memory session state (terminal WS relay needs this)
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(session_id, SessionState::new(session_id, parsed_host_id));
    }

    let agent_kind = profile.agent_kind.clone();
    let profile_id = profile.id.clone();
    let profile_data: AgentProfileData = profile.into();

    // Mint a per-launch task_id for correlating the Started/StartFailed reply.
    let task_id = Uuid::new_v4().to_string();

    let msg = ServerMessage::AgentAction(AgentServerMessage::StartAgent {
        session_id: session_id_str.clone(),
        task_id: task_id.clone(),
        host_id: body.host_id.clone(),
        project_path: body.project_path.clone(),
        profile: profile_data,
    });

    if sender.send(msg).await.is_err() {
        return Err(AppError::Conflict(
            "host went offline, cannot start agent task".to_string(),
        ));
    }

    Ok((
        StatusCode::CREATED,
        Json(CreateAgentTaskResponse {
            session_id: session_id_str,
            task_id,
            agent_kind,
            profile_id,
            host_id: body.host_id,
            project_path: body.project_path,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ConnectionManager;
    use axum::body::Body;
    use axum::http::{Request, StatusCode as HttpStatus};
    use dashmap::DashMap;
    use std::collections::BTreeMap;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use zremote_core::state::AgenticLoopStore;

    async fn test_state() -> Arc<AppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let (events, _) = broadcast::channel(16);
        Arc::new(AppState {
            db: pool,
            connections: Arc::new(ConnectionManager::new()),
            sessions: zremote_core::state::SessionStore::default(),
            agentic_loops: AgenticLoopStore::default(),
            agent_token_hash: String::new(),
            shutdown: CancellationToken::new(),
            events,
            knowledge_requests: Arc::new(DashMap::new()),
            claude_discover_requests: Arc::new(DashMap::new()),
            directory_requests: Arc::new(DashMap::new()),
            settings_get_requests: Arc::new(DashMap::new()),
            settings_save_requests: Arc::new(DashMap::new()),
            action_inputs_requests: Arc::new(DashMap::new()),
            ticket_store: crate::auth::TicketStore::new(),
        })
    }

    fn router(state: Arc<AppState>) -> axum::Router {
        axum::Router::new()
            .route("/api/agent-tasks", axum::routing::post(create_agent_task))
            .with_state(state)
    }

    async fn insert_host(state: &AppState, host_id: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES (?, ?, ?, ?, 'online')",
        )
        .bind(host_id)
        .bind("test-host")
        .bind("test-host")
        .bind("hash")
        .execute(&state.db)
        .await
        .unwrap();
    }

    async fn insert_profile(state: &AppState, kind: &str, name: &str) -> String {
        let id = Uuid::new_v4().to_string();
        let profile = q::AgentProfile {
            id: id.clone(),
            name: name.to_string(),
            description: None,
            agent_kind: kind.to_string(),
            is_default: false,
            sort_order: 0,
            model: Some("opus".to_string()),
            initial_prompt: None,
            skip_permissions: false,
            allowed_tools: vec![],
            extra_args: vec![],
            env_vars: BTreeMap::new(),
            settings: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        };
        q::insert_profile(&state.db, &profile).await.unwrap();
        id
    }

    #[tokio::test]
    async fn rejects_invalid_host_uuid() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "host_id": "not-a-uuid",
            "profile_id": "anything",
            "project_path": "/tmp",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rejects_unknown_host() {
        let state = test_state().await;
        let app = router(state);

        let body = serde_json::json!({
            "host_id": Uuid::new_v4().to_string(),
            "profile_id": "anything",
            "project_path": "/tmp",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::NOT_FOUND);
    }

    #[tokio::test]
    async fn rejects_unknown_profile() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let app = router(state);
        let body = serde_json::json!({
            "host_id": host_id,
            "profile_id": "does-not-exist",
            "project_path": "/tmp",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::NOT_FOUND);
    }

    #[tokio::test]
    async fn rejects_unsupported_kind_from_stale_row() {
        // Insert a profile with a kind not in SUPPORTED_KINDS via raw SQL.
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;

        let profile_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO agent_profiles (id, name, description, agent_kind, is_default, sort_order, \
             model, initial_prompt, skip_permissions, allowed_tools, extra_args, env_vars, \
             settings_json) \
             VALUES (?, ?, NULL, 'gemini', 0, 0, NULL, NULL, 0, '[]', '[]', '{}', '{}')",
        )
        .bind(&profile_id)
        .bind("Stale gemini")
        .execute(&state.db)
        .await
        .unwrap();

        let app = router(state);
        let body = serde_json::json!({
            "host_id": host_id,
            "profile_id": profile_id,
            "project_path": "/tmp",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rejects_offline_host() {
        let state = test_state().await;
        let host_id = Uuid::new_v4().to_string();
        insert_host(&state, &host_id).await;
        let profile_id = insert_profile(&state, "claude", "P").await;

        let app = router(state);
        let body = serde_json::json!({
            "host_id": host_id,
            "profile_id": profile_id,
            "project_path": "/tmp",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::CONFLICT);
    }

    #[tokio::test]
    async fn dispatches_start_agent_to_online_host() {
        let state = test_state().await;
        let host_uuid = Uuid::new_v4();
        let host_id = host_uuid.to_string();
        insert_host(&state, &host_id).await;
        let profile_id = insert_profile(&state, "claude", "P").await;

        // Register a WS sender for the host so `get_sender` succeeds.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ServerMessage>(4);
        state
            .connections
            .register(host_uuid, "test-host".to_string(), tx, false)
            .await;

        let app = router(state);
        let body = serde_json::json!({
            "host_id": host_id,
            "profile_id": profile_id,
            "project_path": "/home/user/project",
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/agent-tasks")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), HttpStatus::CREATED);

        // Verify the dispatched message
        let msg = rx.recv().await.expect("should receive StartAgent");
        match msg {
            ServerMessage::AgentAction(AgentServerMessage::StartAgent {
                task_id,
                profile,
                project_path,
                host_id: dispatched_host,
                ..
            }) => {
                assert_eq!(dispatched_host, host_id);
                assert_eq!(project_path, "/home/user/project");
                assert_eq!(profile.agent_kind, "claude");
                assert_eq!(profile.id, profile_id);
                // task_id should be a valid UUID string
                assert!(Uuid::parse_str(&task_id).is_ok());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    // Note: the `From<AgentProfile> for AgentProfileData` conversion this
    // route uses is tested in `zremote-core::queries::agent_profiles`
    // alongside the impl itself — no need to duplicate the coverage here.
}
