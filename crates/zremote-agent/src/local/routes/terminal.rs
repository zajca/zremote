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
}
