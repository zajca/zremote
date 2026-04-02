use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use tokio::sync::mpsc;
use zremote_core::state::BrowserMessage;
use zremote_core::terminal_ws::{
    BROWSER_CHANNEL_SIZE, RegistrationResult, SessionError, TerminalBackend,
    handle_terminal_websocket,
};
use zremote_protocol::status::SessionStatus;

use crate::local::state::LocalAppState;

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
            let error_message =
                match sqlx::query_as::<_, (String,)>("SELECT status FROM sessions WHERE id = ?")
                    .bind(session_id.to_string())
                    .fetch_optional(&self.state.db)
                    .await
                {
                    Ok(Some((status,))) if status == "active" || status == "creating" => {
                        "session is stale (server restarted)".to_string()
                    }
                    Ok(Some((status,))) => {
                        format!("session is {status}")
                    }
                    Ok(None) => "session not found".to_string(),
                    Err(_) => "session not found or not active".to_string(),
                };
            return Err(SessionError {
                message: error_message,
            });
        }

        // Phase 2: Take write lock for the happy path
        let (tx, rx) = mpsc::channel::<BrowserMessage>(BROWSER_CHANNEL_SIZE);

        let scrollback_data;
        let status;
        {
            let mut sessions = self.state.sessions.write().await;
            let Some(session) = sessions.get_mut(session_id) else {
                return Err(SessionError {
                    message: "session was closed while connecting".to_string(),
                });
            };

            if session.status != SessionStatus::Active
                && session.status != SessionStatus::Creating
                && session.status != SessionStatus::Suspended
            {
                return Err(SessionError {
                    message: format!("session is {}", session.status),
                });
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
    let Ok(session_id) = session_id_str.parse() else {
        let mut socket = socket;
        let _ = socket.send(axum::extract::ws::Message::Close(None)).await;
        return;
    };

    let backend = LocalTerminalBackend { state };
    handle_terminal_websocket(socket, session_id, &backend).await;
}

/// Decode a base64-encoded PNG, set it on the system clipboard, and send Ctrl+V
/// to the PTY so Claude Code detects the paste and reads the clipboard image.
///
/// On clipboard failure, sends `BrowserMessage::ImagePasteError` directly to all
/// browser senders and returns `Ok(())`. The caller does not need to handle errors.
async fn set_clipboard_image_and_paste(
    b64_png: &str,
    state: &LocalAppState,
    session_id: &uuid::Uuid,
) -> Result<(), String> {
    use crate::clipboard::{ImagePasteOutcome, try_clipboard_paste};
    use base64::Engine;

    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(b64_png)
        .map_err(|e| format!("base64 decode: {e}"))?;

    let sid = *session_id;
    // Run clipboard operations in a blocking task (arboard may interact with display server)
    let outcome = tokio::task::spawn_blocking(move || try_clipboard_paste(&png_bytes, sid))
        .await
        .map_err(|e| format!("spawn_blocking: {e}"))?;

    match outcome {
        ImagePasteOutcome::Success => {
            // Send Ctrl+V (0x16) to PTY so Claude Code reads the clipboard
            let mut mgr = state.session_manager.lock().await;
            mgr.write_to(session_id, &[0x16])
                .map_err(|e| format!("PTY write: {e}"))?;
        }
        ImagePasteOutcome::Fallback { path, error } => {
            tracing::warn!(session_id = %session_id, error = %error, "image paste fell back to temp file");
            // Send error with fallback path back to browser
            let sessions = state.sessions.read().await;
            if let Some(session) = sessions.get(session_id) {
                let msg = zremote_core::state::BrowserMessage::ImagePasteError {
                    message: error,
                    fallback_path: if path.is_empty() { None } else { Some(path) },
                };
                for sender in &session.browser_senders {
                    if sender.try_send(msg.clone()).is_err() {
                        tracing::warn!(session_id = %session_id, "failed to send image paste error to browser (channel full/closed)");
                    }
                }
            }
        }
    }
    Ok(())
}
