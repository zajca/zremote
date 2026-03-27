use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;
use zremote_protocol::ServerMessage;

use crate::state::{AppState, BrowserMessage, encode_binary_output};

/// Buffer size for the browser message channel.
const BROWSER_CHANNEL_SIZE: usize = 256;

/// Messages sent from browser to server via WebSocket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum BrowserInput {
    #[serde(rename = "input")]
    Input { data: String },
    #[serde(rename = "resize")]
    Resize { cols: u16, rows: u16 },
    #[serde(rename = "image_paste")]
    ImagePaste { data: String },
}

/// WebSocket upgrade handler for browser terminal connections.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_terminal_connection(socket, session_id, state))
}

#[allow(clippy::too_many_lines)]
async fn handle_terminal_connection(
    mut socket: WebSocket,
    session_id_str: String,
    state: Arc<AppState>,
) {
    let Ok(session_id) = session_id_str.parse() else {
        let _ = socket.send(Message::Close(None)).await;
        return;
    };

    // Validate session exists and is active, send scrollback
    let (tx, mut rx) = mpsc::channel::<BrowserMessage>(BROWSER_CHANNEL_SIZE);
    let host_id;

    // Phase 1: Check existence with read lock
    let session_exists = {
        let sessions = state.sessions.read().await;
        sessions.contains_key(&session_id)
    };

    if !session_exists {
        // Query DB for diagnostics before returning error
        let error_message =
            match sqlx::query_as::<_, (String,)>("SELECT status FROM sessions WHERE id = ?")
                .bind(session_id.to_string())
                .fetch_optional(&state.db)
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

        let error_msg = serde_json::json!({
            "type": "error",
            "message": error_message
        });
        let _ = socket
            .send(Message::Text(error_msg.to_string().into()))
            .await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    }

    // Phase 2: Take write lock for the happy path
    let scrollback_data;
    {
        let mut sessions = state.sessions.write().await;
        let Some(session) = sessions.get_mut(&session_id) else {
            // Session was removed between read and write lock
            let error_msg = serde_json::json!({
                "type": "error",
                "message": "session was closed while connecting"
            });
            let _ = socket
                .send(Message::Text(error_msg.to_string().into()))
                .await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        };

        if session.status != "active"
            && session.status != "creating"
            && session.status != "suspended"
        {
            let error_msg = serde_json::json!({
                "type": "error",
                "message": format!("session is {}", session.status)
            });
            let _ = socket
                .send(Message::Text(error_msg.to_string().into()))
                .await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }

        host_id = session.host_id;
        scrollback_data = session.scrollback.iter().cloned().collect::<Vec<_>>();

        // Register browser sender
        session.browser_senders.push(tx);
    }

    // Read terminal size for scrollback framing
    let (scrollback_cols, scrollback_rows) = {
        let sessions = state.sessions.read().await;
        sessions
            .get(&session_id)
            .map_or((0, 0), |s| (s.last_cols, s.last_rows))
    };

    // Send merged scrollback buffer with framing messages
    if !scrollback_data.is_empty() {
        // Send scrollback_start so the browser can reset terminal state
        let start_msg = BrowserMessage::ScrollbackStart {
            cols: scrollback_cols,
            rows: scrollback_rows,
        };
        if let Ok(json) = serde_json::to_string(&start_msg)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            return;
        }

        // Send each scrollback chunk as an individual binary frame (no merge allocation).
        // The client buffers between ScrollbackStart/End and feeds alacritty once.
        for chunk in &scrollback_data {
            let frame = encode_binary_output(None, chunk);
            if socket.send(Message::Binary(frame.into())).await.is_err() {
                return;
            }
        }

        // Send scrollback_end marker
        let end_msg = BrowserMessage::ScrollbackEnd;
        if let Ok(json) = serde_json::to_string(&end_msg)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            return;
        }
    }

    // If session is currently suspended, notify the browser immediately
    {
        let sessions = state.sessions.read().await;
        if let Some(session) = sessions.get(&session_id)
            && session.status == "suspended"
        {
            let suspended_msg = BrowserMessage::SessionSuspended;
            if let Ok(json) = serde_json::to_string(&suspended_msg)
                && socket.send(Message::Text(json.into())).await.is_err()
            {
                return;
            }
        }
    }

    // Bidirectional relay loop with output coalescing.
    // Instead of forwarding every PTY chunk individually, we collect output
    // over a short window (~16ms, one frame) and send as a single combined
    // message. This reduces WebSocket message count and browser JSON parsing.
    const COALESCE_WINDOW: Duration = Duration::from_millis(16);
    let mut output_buf: Vec<u8> = Vec::new();
    let mut coalesce_deadline: Option<Instant> = None;

    loop {
        // If we have buffered output, use a timeout so we flush promptly.
        let flush_sleep = async {
            match coalesce_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            // Browser -> server
            ws_msg = socket.recv() => {
                match ws_msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<BrowserInput>(&text) {
                            Ok(BrowserInput::Input { mut data }) => {
                                const MAX_INPUT_BYTES: usize = 1_048_576;
                                if data.len() > MAX_INPUT_BYTES {
                                    tracing::warn!(len = data.len(), "browser terminal input exceeds 1 MB, truncating");
                                    data.truncate(MAX_INPUT_BYTES);
                                }
                                // Flush any buffered output before forwarding input
                                // to keep output/input ordering correct.
                                if !output_buf.is_empty() {
                                    let frame = encode_binary_output(None, &output_buf);
                                    output_buf.clear();
                                    coalesce_deadline = None;
                                    if socket.send(Message::Binary(frame.into())).await.is_err() {
                                        break;
                                    }
                                }
                                if let Some(sender) = state.connections.get_sender(&host_id).await {
                                    let msg = ServerMessage::TerminalInput {
                                        session_id,
                                        data: data.into_bytes(),
                                    };
                                    let _ = sender.send(msg).await;
                                }
                            }
                            Ok(BrowserInput::Resize { cols, rows }) => {
                                // Track terminal size for scrollback replay
                                {
                                    let mut sessions = state.sessions.write().await;
                                    if let Some(session) = sessions.get_mut(&session_id) {
                                        session.last_cols = cols;
                                        session.last_rows = rows;
                                    }
                                }
                                if let Some(sender) = state.connections.get_sender(&host_id).await {
                                    let msg = ServerMessage::TerminalResize {
                                        session_id,
                                        cols,
                                        rows,
                                    };
                                    let _ = sender.send(msg).await;
                                }
                            }
                            Ok(BrowserInput::ImagePaste { data }) => {
                                use base64::Engine;
                                if let Ok(png_bytes) = base64::engine::general_purpose::STANDARD.decode(&data)
                                    && let Some(sender) = state.connections.get_sender(&host_id).await
                                {
                                    let msg = ServerMessage::TerminalImagePaste {
                                        session_id,
                                        data: png_bytes,
                                    };
                                    let _ = sender.send(msg).await;
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
            // Server -> browser: receive and buffer output chunks
            browser_msg = rx.recv() => {
                match browser_msg {
                    Some(BrowserMessage::Output { data, .. }) => {
                        output_buf.extend_from_slice(&data);
                        if coalesce_deadline.is_none() {
                            coalesce_deadline = Some(Instant::now() + COALESCE_WINDOW);
                        }
                    }
                    Some(msg) => {
                        // Non-output messages (session_closed, error) flush buffer first, then send immediately.
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
    use crate::state::{
        BINARY_TAG_OUTPUT, BINARY_TAG_PANE_OUTPUT, BrowserMessage, decode_binary_output,
        encode_binary_output,
    };

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
        // data is base64 encoded
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
