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
use zremote_protocol::ServerMessage;
use zremote_protocol::status::SessionStatus;

use crate::state::AppState;

/// Server-mode terminal backend that forwards input to the remote agent.
struct ServerTerminalBackend {
    state: Arc<AppState>,
    host_id: uuid::Uuid,
}

impl TerminalBackend for ServerTerminalBackend {
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
                        "session is stale (agent disconnected or server restarted)".to_string()
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
        if let Some(sender) = self.state.connections.get_sender(&self.host_id).await {
            let msg = ServerMessage::TerminalInput {
                session_id: *session_id,
                data,
            };
            let _ = sender.send(msg).await;
        }
    }

    async fn send_resize(&self, session_id: &uuid::Uuid, cols: u16, rows: u16) {
        // Track terminal size for scrollback replay
        {
            let mut sessions = self.state.sessions.write().await;
            if let Some(session) = sessions.get_mut(session_id) {
                session.last_cols = cols;
                session.last_rows = rows;
            }
        }
        if let Some(sender) = self.state.connections.get_sender(&self.host_id).await {
            let msg = ServerMessage::TerminalResize {
                session_id: *session_id,
                cols,
                rows,
            };
            let _ = sender.send(msg).await;
        }
    }

    async fn send_image_paste(&self, session_id: &uuid::Uuid, data: String) {
        use base64::Engine;
        if let Ok(png_bytes) = base64::engine::general_purpose::STANDARD.decode(&data)
            && let Some(sender) = self.state.connections.get_sender(&self.host_id).await
        {
            let msg = ServerMessage::TerminalImagePaste {
                session_id: *session_id,
                data: png_bytes,
            };
            let _ = sender.send(msg).await;
        }
    }
}

/// WebSocket upgrade handler for browser terminal connections.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_terminal_connection(socket, session_id, state))
}

async fn handle_terminal_connection(
    socket: axum::extract::ws::WebSocket,
    session_id_str: String,
    state: Arc<AppState>,
) {
    let Ok(session_id) = session_id_str.parse() else {
        let mut socket = socket;
        let _ = socket.send(axum::extract::ws::Message::Close(None)).await;
        return;
    };

    // Resolve host_id before entering the shared handler
    let host_id = {
        let sessions = state.sessions.read().await;
        sessions.get(&session_id).map(|s| s.host_id)
    };

    let Some(host_id) = host_id else {
        // Session not in memory -- let register_browser handle the error with DB lookup
        let backend = ServerTerminalBackend {
            state: state.clone(),
            host_id: uuid::Uuid::nil(),
        };
        handle_terminal_websocket(socket, session_id, &backend).await;
        return;
    };

    let backend = ServerTerminalBackend { state, host_id };
    handle_terminal_websocket(socket, session_id, &backend).await;
}

#[cfg(test)]
mod tests {
    use crate::state::{
        BINARY_TAG_OUTPUT, BINARY_TAG_PANE_OUTPUT, BrowserMessage, decode_binary_output,
        encode_binary_output,
    };

    // --- BrowserMessage serialization ---

    #[test]
    fn serialize_scrollback_start() {
        let msg = BrowserMessage::ScrollbackStart { cols: 80, rows: 24 };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "scrollback_start");
        assert_eq!(json["cols"], 80);
        assert_eq!(json["rows"], 24);
    }

    #[test]
    fn serialize_scrollback_end() {
        let msg = BrowserMessage::ScrollbackEnd;
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "scrollback_end");
    }

    #[test]
    fn serialize_session_closed() {
        let msg = BrowserMessage::SessionClosed { exit_code: Some(0) };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert_eq!(json["exit_code"], 0);
    }

    #[test]
    fn serialize_session_closed_no_exit_code() {
        let msg = BrowserMessage::SessionClosed { exit_code: None };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert!(json["exit_code"].is_null());
    }

    #[test]
    fn serialize_session_suspended() {
        let msg = BrowserMessage::SessionSuspended;
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_suspended");
    }

    #[test]
    fn serialize_session_resumed() {
        let msg = BrowserMessage::SessionResumed;
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_resumed");
    }

    #[test]
    fn serialize_output_main_pane() {
        let msg = BrowserMessage::Output {
            pane_id: None,
            data: b"hello".to_vec(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "output");
        assert!(json.get("pane_id").is_none());
        assert!(json["data"].is_string());
    }

    #[test]
    fn serialize_output_specific_pane() {
        let msg = BrowserMessage::Output {
            pane_id: Some("pane-1".to_string()),
            data: b"world".to_vec(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "output");
        assert_eq!(json["pane_id"], "pane-1");
    }

    #[test]
    fn serialize_error() {
        let msg = BrowserMessage::Error {
            message: "something went wrong".to_string(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["message"], "something went wrong");
    }

    // --- encode_binary_output / decode_binary_output ---

    #[test]
    fn encode_main_pane_output() {
        let data = b"terminal output";
        let frame = encode_binary_output(None, data);
        assert_eq!(frame[0], BINARY_TAG_OUTPUT);
        assert_eq!(&frame[1..], data);
    }

    #[test]
    fn encode_specific_pane_output() {
        let data = b"pane data";
        let pane_id = "my-pane";
        let frame = encode_binary_output(Some(pane_id), data);
        assert_eq!(frame[0], BINARY_TAG_PANE_OUTPUT);
        assert_eq!(frame[1], pane_id.len() as u8);
        assert_eq!(&frame[2..2 + pane_id.len()], pane_id.as_bytes());
        assert_eq!(&frame[2 + pane_id.len()..], data);
    }

    #[test]
    fn encode_decode_roundtrip_main_pane() {
        let data = b"roundtrip test";
        let frame = encode_binary_output(None, data);
        let (pane_id, decoded) = decode_binary_output(&frame).unwrap();
        assert!(pane_id.is_none());
        assert_eq!(decoded, data);
    }

    #[test]
    fn encode_decode_roundtrip_specific_pane() {
        let data = b"pane roundtrip";
        let frame = encode_binary_output(Some("p1"), data);
        let (pane_id, decoded) = decode_binary_output(&frame).unwrap();
        assert_eq!(pane_id.as_deref(), Some("p1"));
        assert_eq!(decoded, data);
    }

    #[test]
    fn encode_empty_data() {
        let frame = encode_binary_output(None, b"");
        assert_eq!(frame, vec![BINARY_TAG_OUTPUT]);
        let (pane_id, decoded) = decode_binary_output(&frame).unwrap();
        assert!(pane_id.is_none());
        assert!(decoded.is_empty());
    }

    #[test]
    fn decode_empty_frame_returns_none() {
        assert!(decode_binary_output(&[]).is_none());
    }

    #[test]
    fn decode_unknown_tag_returns_none() {
        assert!(decode_binary_output(&[0xFF, 0x01, 0x02]).is_none());
    }
}
