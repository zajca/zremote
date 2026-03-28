//! Shared terminal WebSocket handler logic.
//!
//! Both the server and local agent use nearly identical WebSocket handlers
//! for terminal connections. This module extracts the common logic behind
//! a [`TerminalBackend`] trait that abstracts the mode-specific operations.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;
use zremote_protocol::SessionId;
use zremote_protocol::status::SessionStatus;

use crate::state::{BrowserMessage, encode_binary_output};

/// Buffer size for the browser message channel.
pub const BROWSER_CHANNEL_SIZE: usize = 256;

/// Maximum input bytes accepted from a browser terminal message (1 MB).
const MAX_INPUT_BYTES: usize = 1_048_576;

/// Output coalescing window (~one frame at 60 fps).
const COALESCE_WINDOW: Duration = Duration::from_millis(16);

/// Messages sent from browser to server via WebSocket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserInput {
    #[serde(rename = "input")]
    Input { data: String },
    #[serde(rename = "resize")]
    Resize { cols: u16, rows: u16 },
    #[serde(rename = "image_paste")]
    ImagePaste { data: String },
}

/// Result of registering a browser sender with the backend.
pub struct RegistrationResult {
    /// Receiver for messages from the backend (PTY output, session events).
    pub rx: mpsc::Receiver<BrowserMessage>,
    /// Scrollback chunks to replay.
    pub scrollback: Vec<Vec<u8>>,
    /// Terminal dimensions at time of connection.
    pub cols: u16,
    pub rows: u16,
    /// Current session status (used to send suspended notification).
    pub status: SessionStatus,
}

/// Error message returned when a session cannot be connected.
pub struct SessionError {
    pub message: String,
}

/// Abstracts the differences between server and local terminal backends.
///
/// Server mode: forwards input/resize/paste to the remote agent via `ServerMessage`.
/// Local mode: writes directly to the PTY and system clipboard.
// Suppressed: both implementors (ServerTerminalBackend, LocalTerminalBackend) are
// concrete types used directly, never as trait objects, so the lack of Send bound
// on the returned futures is not a concern.
#[allow(async_fn_in_trait)]
pub trait TerminalBackend: Send + Sync + 'static {
    /// Try to register a browser sender for the given session.
    ///
    /// Returns `Ok(RegistrationResult)` with scrollback data and a receiver,
    /// or `Err(SessionError)` with a user-facing error message.
    async fn register_browser(
        &self,
        session_id: &SessionId,
    ) -> Result<RegistrationResult, SessionError>;

    /// Send terminal input data to the PTY.
    async fn send_input(&self, session_id: &SessionId, data: Vec<u8>);

    /// Send a terminal resize event.
    async fn send_resize(&self, session_id: &SessionId, cols: u16, rows: u16);

    /// Handle an image paste event (base64-encoded PNG).
    async fn send_image_paste(&self, session_id: &SessionId, data: String);
}

/// Shared terminal WebSocket message loop.
///
/// The caller is responsible for WebSocket upgrade and session ID parsing.
/// This function handles:
/// - Scrollback replay
/// - Bidirectional relay with output coalescing
/// - Cleanup on disconnect
#[allow(clippy::too_many_lines)]
pub async fn handle_terminal_websocket<B: TerminalBackend>(
    mut socket: WebSocket,
    session_id: SessionId,
    backend: &B,
) {
    // Register browser and get scrollback
    let registration = match backend.register_browser(&session_id).await {
        Ok(r) => r,
        Err(e) => {
            let error_msg = serde_json::json!({
                "type": "error",
                "message": e.message
            });
            let _ = socket
                .send(Message::Text(error_msg.to_string().into()))
                .await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };

    let RegistrationResult {
        mut rx,
        scrollback,
        cols,
        rows,
        status,
    } = registration;

    // Send scrollback buffer with framing messages
    if !scrollback.is_empty() {
        let start_msg = BrowserMessage::ScrollbackStart { cols, rows };
        if let Ok(json) = serde_json::to_string(&start_msg)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            return;
        }

        for chunk in &scrollback {
            let frame = encode_binary_output(None, chunk);
            if socket.send(Message::Binary(frame.into())).await.is_err() {
                return;
            }
        }

        let end_msg = BrowserMessage::ScrollbackEnd;
        if let Ok(json) = serde_json::to_string(&end_msg)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            return;
        }
    }

    // If session is currently suspended, notify immediately
    if status == SessionStatus::Suspended {
        let suspended_msg = BrowserMessage::SessionSuspended;
        if let Ok(json) = serde_json::to_string(&suspended_msg)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            return;
        }
    }

    // Bidirectional relay loop with output coalescing
    let mut output_buf: Vec<u8> = Vec::new();
    let mut coalesce_deadline: Option<Instant> = None;

    loop {
        let flush_sleep = async {
            match coalesce_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            // Browser -> backend
            ws_msg = socket.recv() => {
                match ws_msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<BrowserInput>(&text) {
                            Ok(BrowserInput::Input { mut data }) => {
                                if data.len() > MAX_INPUT_BYTES {
                                    tracing::warn!(len = data.len(), "browser terminal input exceeds 1 MB, truncating");
                                    data.truncate(MAX_INPUT_BYTES);
                                }
                                // Flush buffered output before forwarding input
                                // to keep output/input ordering correct.
                                if !output_buf.is_empty() {
                                    let frame = encode_binary_output(None, &output_buf);
                                    output_buf.clear();
                                    coalesce_deadline = None;
                                    if socket.send(Message::Binary(frame.into())).await.is_err() {
                                        break;
                                    }
                                }
                                backend.send_input(&session_id, data.into_bytes()).await;
                            }
                            Ok(BrowserInput::Resize { cols, rows }) => {
                                backend.send_resize(&session_id, cols, rows).await;
                            }
                            Ok(BrowserInput::ImagePaste { data }) => {
                                if data.len() > MAX_INPUT_BYTES {
                                    tracing::warn!(len = data.len(), "image_paste data exceeds limit, rejecting");
                                } else {
                                    backend.send_image_paste(&session_id, data).await;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "invalid browser terminal message");
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_))) => {
                        tracing::warn!("unexpected binary message from browser terminal");
                    }
                    Some(Err(e)) => {
                        tracing::warn!(error = %e, "browser terminal WebSocket error");
                        break;
                    }
                }
            }
            // Backend -> browser: receive and buffer output chunks
            browser_msg = rx.recv() => {
                match browser_msg {
                    Some(BrowserMessage::Output { data, .. }) => {
                        output_buf.extend_from_slice(&data);
                        if coalesce_deadline.is_none() {
                            coalesce_deadline = Some(Instant::now() + COALESCE_WINDOW);
                        }
                    }
                    Some(msg) => {
                        // Non-output messages flush buffer first, then send immediately.
                        if !output_buf.is_empty() {
                            let frame = encode_binary_output(None, &output_buf);
                            output_buf.clear();
                            coalesce_deadline = None;
                            if socket.send(Message::Binary(frame.into())).await.is_err() {
                                break;
                            }
                        }
                        if let Ok(json) = serde_json::to_string(&msg)
                            && socket.send(Message::Text(json.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    None => break,
                }
            }
            // Flush coalesced output when the window expires
            () = flush_sleep => {
                if !output_buf.is_empty() {
                    let frame = encode_binary_output(None, &output_buf);
                    output_buf.clear();
                    coalesce_deadline = None;
                    if socket.send(Message::Binary(frame.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    // Cleanup: sender drops automatically when this function returns,
    // try_send in the relay loop will detect it and remove it.
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- BrowserInput deserialization ---

    #[test]
    fn deserialize_input_variant() {
        let json = r#"{"type": "input", "data": "hello"}"#;
        let msg: BrowserInput = serde_json::from_str(json).unwrap();
        match msg {
            BrowserInput::Input { data } => assert_eq!(data, "hello"),
            other => panic!("expected Input, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_resize_variant() {
        let json = r#"{"type": "resize", "cols": 120, "rows": 40}"#;
        let msg: BrowserInput = serde_json::from_str(json).unwrap();
        match msg {
            BrowserInput::Resize { cols, rows } => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
            }
            other => panic!("expected Resize, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_image_paste_variant() {
        let json = r#"{"type": "image_paste", "data": "base64data=="}"#;
        let msg: BrowserInput = serde_json::from_str(json).unwrap();
        match msg {
            BrowserInput::ImagePaste { data } => assert_eq!(data, "base64data=="),
            other => panic!("expected ImagePaste, got {other:?}"),
        }
    }

    #[test]
    fn deserialize_unknown_type_returns_error() {
        let json = r#"{"type": "unknown_variant", "data": "x"}"#;
        let result = serde_json::from_str::<BrowserInput>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_missing_type_field_returns_error() {
        let json = r#"{"data": "hello"}"#;
        let result = serde_json::from_str::<BrowserInput>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_input_missing_data_returns_error() {
        let json = r#"{"type": "input"}"#;
        let result = serde_json::from_str::<BrowserInput>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_resize_missing_cols_returns_error() {
        let json = r#"{"type": "resize", "rows": 40}"#;
        let result = serde_json::from_str::<BrowserInput>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_resize_missing_rows_returns_error() {
        let json = r#"{"type": "resize", "cols": 120}"#;
        let result = serde_json::from_str::<BrowserInput>(json);
        assert!(result.is_err());
    }
}
