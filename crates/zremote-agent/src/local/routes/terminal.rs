use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;
use zremote_core::state::{BrowserMessage, encode_binary_output};

use crate::local::state::LocalAppState;

/// Buffer size for the browser message channel.
const BROWSER_CHANNEL_SIZE: usize = 256;

/// Messages sent from browser to server via WebSocket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum BrowserInput {
    #[serde(rename = "input")]
    Input {
        #[serde(default)]
        pane_id: Option<String>,
        data: String,
    },
    #[serde(rename = "resize")]
    Resize {
        #[serde(default)]
        pane_id: Option<String>,
        cols: u16,
        rows: u16,
    },
    #[serde(rename = "image_paste")]
    ImagePaste { data: String },
}

/// WebSocket upgrade handler for browser terminal connections.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<Arc<LocalAppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_terminal_connection(socket, session_id, state))
}

#[allow(clippy::too_many_lines)]
async fn handle_terminal_connection(
    mut socket: WebSocket,
    session_id_str: String,
    state: Arc<LocalAppState>,
) {
    let Ok(session_id) = session_id_str.parse() else {
        let _ = socket.send(Message::Close(None)).await;
        return;
    };

    // Validate session exists and is active, send scrollback
    let (tx, mut rx) = mpsc::channel::<BrowserMessage>(BROWSER_CHANNEL_SIZE);

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
                    "session is stale (server restarted)".to_string()
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

        let end_msg = BrowserMessage::ScrollbackEnd;
        if let Ok(json) = serde_json::to_string(&end_msg)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            return;
        }
    }

    // Send extra pane info and scrollback
    {
        let sessions = state.sessions.read().await;
        if let Some(session) = sessions.get(&session_id) {
            for (pane_id, (chunks, _size)) in &session.pane_scrollbacks {
                // Notify about existing pane
                let pane_added = BrowserMessage::PaneAdded {
                    pane_id: pane_id.clone(),
                    index: 0, // index not stored in scrollback, will be refreshed by sync
                };
                if let Ok(json) = serde_json::to_string(&pane_added)
                    && socket.send(Message::Text(json.into())).await.is_err()
                {
                    return;
                }

                // Send pane scrollback as individual binary frames
                for chunk in chunks {
                    let frame = encode_binary_output(Some(pane_id), chunk);
                    if socket.send(Message::Binary(frame.into())).await.is_err() {
                        return;
                    }
                }
            }
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
    #[allow(clippy::items_after_statements)]
    const COALESCE_WINDOW: Duration = Duration::from_millis(16);
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
            // Browser -> PTY
            ws_msg = socket.recv() => {
                match ws_msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<BrowserInput>(&text) {
                            Ok(BrowserInput::Input { pane_id: target_pane, mut data }) => {
                                const MAX_INPUT_BYTES: usize = 1_048_576;
                                if data.len() > MAX_INPUT_BYTES {
                                    tracing::warn!(len = data.len(), "browser terminal input exceeds 1 MB, truncating");
                                    data.truncate(MAX_INPUT_BYTES);
                                }
                                // Flush any buffered output before forwarding input
                                if !output_buf.is_empty() {
                                    let frame = encode_binary_output(None, &output_buf);
                                    output_buf.clear();
                                    coalesce_deadline = None;
                                    if socket.send(Message::Binary(frame.into())).await.is_err() {
                                        break;
                                    }
                                }
                                // Write to specific pane or main pane
                                let mut mgr = state.session_manager.lock().await;
                                let result = if let Some(ref pid) = target_pane {
                                    mgr.write_to_pane(&session_id, pid, data.as_bytes())
                                } else {
                                    mgr.write_to(&session_id, data.as_bytes())
                                };
                                if let Err(e) = result {
                                    tracing::warn!(error = %e, "failed to write to PTY");
                                    break;
                                }
                            }
                            Ok(BrowserInput::Resize { pane_id: target_pane, cols, rows }) => {
                                let mgr = state.session_manager.lock().await;
                                let result = if let Some(ref pid) = target_pane {
                                    mgr.resize_pane(&session_id, pid, cols, rows)
                                } else {
                                    mgr.resize(&session_id, cols, rows)
                                };
                                if let Err(e) = result {
                                    tracing::warn!(error = %e, "failed to resize PTY");
                                }
                                // Track terminal size for scrollback replay (main pane only)
                                if target_pane.is_none() {
                                    let mut sessions = state.sessions.write().await;
                                    if let Some(session) = sessions.get_mut(&session_id) {
                                        session.last_cols = cols;
                                        session.last_rows = rows;
                                    }
                                }
                            }
                            Ok(BrowserInput::ImagePaste { data }) => {
                                if let Err(e) = set_clipboard_image_and_paste(
                                    &data,
                                    &state,
                                    &session_id,
                                ).await {
                                    tracing::warn!(error = %e, "image paste failed");
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
            // PTY -> browser: receive and buffer output chunks
            browser_msg = rx.recv() => {
                match browser_msg {
                    Some(BrowserMessage::Output { data, .. }) => {
                        output_buf.extend_from_slice(&data);
                        if coalesce_deadline.is_none() {
                            coalesce_deadline = Some(Instant::now() + COALESCE_WINDOW);
                        }
                    }
                    Some(msg) => {
                        // Non-output messages (session_closed, error) flush buffer first
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
    // try_send in the output loop will detect it and remove it.
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
