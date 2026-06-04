use std::sync::Arc;

use axum::Json;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use tokio::sync::mpsc;
use uuid::Uuid;
use zremote_core::error::AppError;
use zremote_core::state::{BrowserMessage, ServerEvent, SessionState};
use zremote_core::terminal_ws::{
    BROWSER_CHANNEL_SIZE, RegistrationResult, SessionError, TerminalBackend,
    handle_terminal_websocket,
};
use zremote_protocol::status::SessionStatus;

use crate::local::state::LocalAppState;
use crate::pty::shell_integration::ShellIntegrationConfig;

/// Local-mode terminal backend that writes directly to PTY.
struct LocalTerminalBackend {
    state: Arc<LocalAppState>,
}

impl TerminalBackend for LocalTerminalBackend {
    async fn register_browser(
        &self,
        session_id: &uuid::Uuid,
    ) -> Result<RegistrationResult, SessionError> {
        // Phase 1: Check existence with read lock
        let session_exists = {
            let sessions = self.state.sessions.read().await;
            sessions.contains_key(session_id)
        };

        if !session_exists {
            let db_status =
                sqlx::query_as::<_, (String,)>("SELECT status FROM sessions WHERE id = ?")
                    .bind(session_id.to_string())
                    .fetch_optional(&self.state.db)
                    .await;

            // RFC-013: a `resumable` session has no live backend yet. With
            // auto-resume on, relaunch the agent now (so the happy path below
            // finds it active); with it off, return a typed resumable signal so
            // the GUI offers an explicit "Continue" instead of a dead terminal.
            if matches!(&db_status, Ok(Some((s,))) if s == "resumable") {
                if crate::config::resume_agents_on_restart() {
                    self.resume_resumable_session(session_id).await?;
                    // Fall through: the session is now in memory and active.
                } else {
                    return Err(SessionError::resumable(
                        "session is resumable — click to continue".to_string(),
                    ));
                }
            } else {
                let error_message = match db_status {
                    Ok(Some((status,))) if status == "active" || status == "creating" => {
                        "session is stale (server restarted)".to_string()
                    }
                    Ok(Some((status,))) => format!("session is {status}"),
                    Ok(None) => "session not found".to_string(),
                    Err(_) => "session not found or not active".to_string(),
                };
                return Err(SessionError::new(error_message));
            }
        }

        // Phase 2: Take write lock for the happy path
        let (tx, rx) = mpsc::channel::<BrowserMessage>(BROWSER_CHANNEL_SIZE);

        let scrollback_data;
        let status;
        {
            let mut sessions = self.state.sessions.write().await;
            let Some(session) = sessions.get_mut(session_id) else {
                return Err(SessionError::new("session was closed while connecting"));
            };

            if session.status != SessionStatus::Active
                && session.status != SessionStatus::Creating
                && session.status != SessionStatus::Suspended
            {
                return Err(SessionError::new(format!("session is {}", session.status)));
            }

            scrollback_data = session.scrollback.iter().cloned().collect::<Vec<_>>();
            status = session.status;
            session.browser_senders.push(tx);
        }

        // Read terminal size
        let (cols, rows) = {
            let sessions = self.state.sessions.read().await;
            sessions
                .get(session_id)
                .map_or((0, 0), |s| (s.last_cols, s.last_rows))
        };

        Ok(RegistrationResult {
            rx,
            scrollback: scrollback_data,
            cols,
            rows,
            status,
        })
    }

    async fn send_input(&self, session_id: &uuid::Uuid, data: Vec<u8>) {
        let mut mgr = self.state.session_manager.lock().await;
        if let Err(e) = mgr.write_to(session_id, &data) {
            tracing::warn!(error = %e, "failed to write to PTY");
        }
    }

    async fn send_resize(&self, session_id: &uuid::Uuid, cols: u16, rows: u16) {
        let mgr = self.state.session_manager.lock().await;
        if let Err(e) = mgr.resize(session_id, cols, rows) {
            tracing::warn!(error = %e, "failed to resize PTY");
        }
        // Track terminal size for scrollback replay
        {
            let mut sessions = self.state.sessions.write().await;
            if let Some(session) = sessions.get_mut(session_id) {
                session.last_cols = cols;
                session.last_rows = rows;
            }
        }
    }

    async fn send_image_paste(&self, session_id: &uuid::Uuid, data: String) {
        if let Err(e) = set_clipboard_image_and_paste(&data, &self.state, session_id).await {
            tracing::warn!(error = %e, "image paste failed");
        }
    }
}

impl LocalTerminalBackend {
    /// Resume-on-attach (RFC-013): re-spawn the agent for a `resumable` session
    /// during `register_browser`, mapping engine errors to a `SessionError` so
    /// the WS handler reports a clean failure (and the row stays `resumable`).
    async fn resume_resumable_session(&self, session_id: &Uuid) -> Result<(), SessionError> {
        resume_session_by_id(&self.state, session_id)
            .await
            .map_err(|e| SessionError::new(format!("failed to resume session: {e}")))?;
        Ok(())
    }
}

/// Shared resume engine for the local agent (RFC-013), used by both
/// resume-on-attach (`register_browser`) and the explicit
/// `POST /api/hosts/:host_id/sessions/:id/resume` endpoint.
///
/// Re-spawns the agent for an existing `resumable` session using the argv-direct
/// engine (`SessionManager::resume_session`), reusing the SAME `sessions.id`,
/// then transitions the row `resumable` -> `active` and re-creates the in-memory
/// `SessionState`. Returns an error (leaving the row `resumable`) if the session
/// has no resumable agent ref or the agent CLI cannot be spawned.
pub async fn resume_session_by_id(
    state: &Arc<LocalAppState>,
    session_id: &Uuid,
) -> Result<(), AppError> {
    let session_id_str = session_id.to_string();

    // Load status + shell + working_dir in one read.
    let row: Option<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT status, shell, working_dir FROM sessions WHERE id = ?")
            .bind(&session_id_str)
            .fetch_optional(&state.db)
            .await?;
    let (status, shell_opt, working_dir) =
        row.ok_or_else(|| AppError::NotFound(format!("session {session_id_str} not found")))?;

    // Guard: only a `resumable` session may be resumed. Without this an
    // explicit resume (REST) on an active/closed session would corrupt
    // timestamps or reactivate a closed row.
    if status != "resumable" {
        return Err(AppError::Conflict(format!(
            "session {session_id_str} is not resumable (status: {status})"
        )));
    }

    // Build the resume argv from the persisted agent identity (RFC-012).
    let Some(resume_argv) =
        crate::session::build_resume_argv_for_session(&state.db, &session_id_str).await?
    else {
        return Err(AppError::BadRequest(format!(
            "session {session_id_str} is not a resumable agent session"
        )));
    };

    let shell = crate::shell::resolve_shell(shell_opt.as_deref());
    let ai_config = ShellIntegrationConfig::for_ai_session();

    // Spawn the agent as the session's process (argv-direct, no shell race).
    let pid = {
        let mut mgr = state.session_manager.lock().await;
        mgr.resume_session(
            *session_id,
            &shell,
            120,
            40,
            working_dir.as_deref(),
            None,
            Some(&ai_config),
            &resume_argv,
        )
        .await
        .map_err(|e| AppError::Internal(format!("failed to spawn resume PTY: {e}")))?
    };

    // resumable -> active (reuses the same id; clears suspended/closed stamps).
    // If the DB update fails AFTER the spawn, tear the spawned backend down so we
    // don't leave an orphan PTY with the row still `resumable`.
    if let Err(e) = mark_session_active_and_pid(state, &session_id_str, &shell, pid).await {
        let mut mgr = state.session_manager.lock().await;
        let _ = mgr.close(session_id);
        return Err(e);
    }

    // Re-create in-memory state only after spawn AND DB update both succeed, so
    // a failed resume never leaves a dangling SessionState without a backend.
    {
        let mut sessions = state.sessions.write().await;
        let mut session_state = SessionState::new(*session_id, state.host_id);
        session_state.status = SessionStatus::Active;
        sessions.insert(*session_id, session_state);
    }

    let _ = state.events.send(ServerEvent::SessionUpdated {
        session_id: session_id_str,
    });

    Ok(())
}

/// Transition a resumed session row to `active` and record the new shell/pid.
/// Split out so the caller can roll back the spawned PTY if this fails.
async fn mark_session_active_and_pid(
    state: &Arc<LocalAppState>,
    session_id_str: &str,
    shell: &str,
    pid: u32,
) -> Result<(), AppError> {
    zremote_core::queries::sessions::mark_session_active(&state.db, session_id_str).await?;
    sqlx::query("UPDATE sessions SET shell = ?, pid = ? WHERE id = ?")
        .bind(shell)
        .bind(i64::from(pid))
        .bind(session_id_str)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Request body for terminal input.
#[derive(Debug, Deserialize)]
pub struct TerminalInputBody {
    /// Base64-encoded bytes to send to PTY stdin.
    pub data: String,
}

/// Maximum allowed PTY input size per request (64 KB).
const MAX_PTY_INPUT_BYTES: usize = 65_536;

/// `POST /api/sessions/{id}/terminal/input`
pub async fn terminal_input(
    State(state): State<Arc<LocalAppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<TerminalInputBody>,
) -> Result<impl IntoResponse, AppError> {
    use base64::Engine;

    let parsed_session_id: Uuid = session_id
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid session ID: {session_id}")))?;

    let data = base64::engine::general_purpose::STANDARD
        .decode(&body.data)
        .map_err(|e| AppError::BadRequest(format!("invalid base64 data: {e}")))?;

    if data.is_empty() {
        return Err(AppError::BadRequest("data must not be empty".to_string()));
    }

    if data.len() > MAX_PTY_INPUT_BYTES {
        return Err(AppError::BadRequest(format!(
            "data too large: {} bytes (max {MAX_PTY_INPUT_BYTES})",
            data.len()
        )));
    }

    let mut mgr = state.session_manager.lock().await;
    mgr.write_to(&parsed_session_id, &data).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") {
            AppError::NotFound(msg)
        } else {
            AppError::Internal(format!("write failed: {msg}"))
        }
    })?;

    Ok(StatusCode::ACCEPTED)
}

/// WebSocket upgrade handler for browser terminal connections.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<Arc<LocalAppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_terminal_connection(socket, session_id, state))
}

async fn handle_terminal_connection(
    socket: axum::extract::ws::WebSocket,
    session_id_str: String,
    state: Arc<LocalAppState>,
) {
    // A UUID is 36 chars; cap the echoed path segment before any allocation
    // work (format! + JSON) to bound memory on malformed/malicious requests.
    const MAX_SESSION_ID_LEN: usize = 64;
    let (parsed, echoed) = if session_id_str.len() > MAX_SESSION_ID_LEN {
        (Err(()), "<too long>".to_string())
    } else {
        (
            session_id_str.parse::<uuid::Uuid>().map_err(|_| ()),
            session_id_str.clone(),
        )
    };
    let Ok(session_id) = parsed else {
        let mut socket = socket;
        let err = BrowserMessage::Error {
            message: format!("invalid session id '{echoed}': expected UUID"),
        };
        if let Ok(json) = serde_json::to_string(&err) {
            let _ = socket
                .send(axum::extract::ws::Message::Text(json.into()))
                .await;
        }
        let _ = socket.send(axum::extract::ws::Message::Close(None)).await;
        return;
    };

    let backend = LocalTerminalBackend { state };
    handle_terminal_websocket(socket, session_id, &backend).await;
}

/// Decode a base64-encoded PNG, set it on the system clipboard, and send Ctrl+V
/// to the PTY so Claude Code detects the paste and reads the clipboard image.
async fn set_clipboard_image_and_paste(
    b64_png: &str,
    state: &LocalAppState,
    session_id: &uuid::Uuid,
) -> Result<(), String> {
    use base64::Engine;

    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(b64_png)
        .map_err(|e| format!("base64 decode: {e}"))?;

    // Decode PNG to RGBA for arboard
    let decoder = png::Decoder::new(png_bytes.as_slice());
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png decode: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png frame: {e}"))?;
    buf.truncate(info.buffer_size());

    let img_data = arboard::ImageData {
        width: info.width as usize,
        height: info.height as usize,
        bytes: std::borrow::Cow::Owned(buf),
    };

    // Set clipboard in a blocking task (arboard may interact with display server)
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| format!("clipboard init: {e}"))?;
        clipboard
            .set_image(img_data)
            .map_err(|e| format!("clipboard set: {e}"))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))??;

    // Send Ctrl+V (0x16) to PTY so Claude Code reads the clipboard
    let mut mgr = state.session_manager.lock().await;
    mgr.write_to(session_id, &[0x16])
        .map_err(|e| format!("PTY write: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use base64::Engine;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use crate::local::upsert_local_host;

    async fn test_state() -> Arc<LocalAppState> {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"test-host");
        upsert_local_host(&pool, &host_id, "test-host")
            .await
            .unwrap();
        LocalAppState::new(
            pool,
            "test-host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            Uuid::new_v4(),
        )
    }

    fn build_test_router(state: Arc<LocalAppState>) -> Router {
        Router::new()
            .route(
                "/api/sessions/{session_id}/terminal/input",
                post(terminal_input),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn terminal_input_invalid_session_id() {
        let state = test_state().await;
        let app = build_test_router(state);
        let data = base64::engine::general_purpose::STANDARD.encode(b"hello");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sessions/not-a-uuid/terminal/input")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"data":"{data}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn terminal_input_invalid_base64() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let app = build_test_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session_id}/terminal/input"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"data":"not-valid-base64!!!"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn terminal_input_empty_data() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let data = base64::engine::general_purpose::STANDARD.encode(b"");
        let app = build_test_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session_id}/terminal/input"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"data":"{data}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn terminal_input_session_not_found() {
        let state = test_state().await;
        let session_id = Uuid::new_v4().to_string();
        let data = base64::engine::general_purpose::STANDARD.encode(b"hello");
        let app = build_test_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session_id}/terminal/input"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"data":"{data}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- RFC-013 resume-on-attach + REST resume ---

    // tokio Mutex so the guard can be held across `.await` (env mutation must be
    // serialized for the whole async test body, including DB setup).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn insert_resumable_agent_session(
        state: &Arc<LocalAppState>,
        id: &str,
        agent_kind: Option<&str>,
        agent_ref: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO sessions (id, host_id, status, shell, working_dir, agent_kind, agent_session_ref) \
             VALUES (?, ?, 'resumable', '/bin/sh', '/tmp', ?, ?)",
        )
        .bind(id)
        .bind(state.host_id.to_string())
        .bind(agent_kind)
        .bind(agent_ref)
        .execute(&state.db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn resume_session_by_id_rejects_non_resumable_session() {
        // A session with no agent_session_ref cannot be resumed (BadRequest).
        let state = test_state().await;
        let id = Uuid::new_v4();
        insert_resumable_agent_session(&state, &id.to_string(), None, None).await;

        let err = resume_session_by_id(&state, &id).await.unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)), "got {err:?}");
        // Row stays resumable (not mutated to active on failure).
        let (status,): (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(status, "resumable");
    }

    #[tokio::test]
    async fn resume_session_by_id_rejects_non_resumable_status() {
        // HIGH #1: resume must be rejected (Conflict) for active/closed sessions
        // so it can't corrupt timestamps or reactivate a closed row.
        for status in ["active", "closed"] {
            let state = test_state().await;
            let id = Uuid::new_v4();
            // Insert with an agent ref but the wrong status.
            sqlx::query(
                "INSERT INTO sessions (id, host_id, status, agent_kind, agent_session_ref) \
                 VALUES (?, ?, ?, 'claude', 'cc-xyz')",
            )
            .bind(id.to_string())
            .bind(state.host_id.to_string())
            .bind(status)
            .execute(&state.db)
            .await
            .unwrap();

            let err = resume_session_by_id(&state, &id).await.unwrap_err();
            assert!(
                matches!(err, AppError::Conflict(_)),
                "status {status} should be rejected with Conflict, got {err:?}"
            );
            // Status unchanged (no mark_session_active side effect).
            let (got,): (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
                .bind(id.to_string())
                .fetch_one(&state.db)
                .await
                .unwrap();
            assert_eq!(got, status, "status must be untouched on rejected resume");
        }
    }

    #[tokio::test]
    async fn resume_session_by_id_transitions_resumable_to_active() {
        // Claude agent session -> resume_argv = ["claude","--resume",..]. The
        // `claude` binary likely isn't on PATH in CI, so the spawn may fail; in
        // that case the row must STAY resumable (no false 'active'). When the
        // spawn does succeed, the row must be 'active' and tracked in memory.
        let state = test_state().await;
        let id = Uuid::new_v4();
        insert_resumable_agent_session(&state, &id.to_string(), Some("claude"), Some("cc-xyz"))
            .await;

        let result = resume_session_by_id(&state, &id).await;
        let (status,): (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        if result.is_ok() {
            assert_eq!(status, "active");
            assert!(state.sessions.read().await.contains_key(&id));
        } else {
            // Spawn failed (no `claude` on PATH): row must remain resumable.
            assert_eq!(status, "resumable");
        }
    }

    #[tokio::test]
    async fn register_browser_returns_resumable_when_autoresume_off() {
        let _guard = ENV_LOCK.lock().await;
        // SAFETY: serialized by ENV_LOCK; restored at end.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("ZREMOTE_RESUME_AGENTS_ON_RESTART", "false");
        }

        let state = test_state().await;
        let id = Uuid::new_v4();
        insert_resumable_agent_session(&state, &id.to_string(), Some("claude"), Some("cc-xyz"))
            .await;

        let backend = LocalTerminalBackend {
            state: state.clone(),
        };
        let Err(err) = backend.register_browser(&id).await else {
            panic!("expected a resumable error, got Ok");
        };
        assert!(
            err.resumable,
            "expected typed resumable signal, got: {}",
            err.message
        );
        // Row untouched (no resume attempted when the flag is off).
        let (status,): (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(status, "resumable");

        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("ZREMOTE_RESUME_AGENTS_ON_RESTART");
        }
    }

    #[tokio::test]
    async fn register_browser_hard_error_for_closed_session() {
        // A non-resumable, non-live session still yields a hard (non-resumable)
        // error so the GUI shows the stale/closed message.
        let state = test_state().await;
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, 'closed')")
            .bind(id.to_string())
            .bind(state.host_id.to_string())
            .execute(&state.db)
            .await
            .unwrap();

        let backend = LocalTerminalBackend {
            state: state.clone(),
        };
        let Err(err) = backend.register_browser(&id).await else {
            panic!("expected a hard error, got Ok");
        };
        assert!(!err.resumable);
        assert!(err.message.contains("closed"), "got: {}", err.message);
    }
}
