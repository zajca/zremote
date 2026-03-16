use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use myremote_core::state::BrowserMessage;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::local::state::LocalAppState;

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
        let error_message = match sqlx::query_as::<_, (String,)>(
            "SELECT status FROM sessions WHERE id = ?",
        )
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

        if session.status != "active" && session.status != "creating" && session.status != "suspended" {
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

    // Send merged scrollback buffer with framing messages
    if !scrollback_data.is_empty() {
        let start_msg = BrowserMessage::ScrollbackStart;
        if let Ok(json) = serde_json::to_string(&start_msg)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            return;
        }

        // Merge all chunks into a single blob to avoid ANSI escape fragmentation
        let mut merged = Vec::new();
        for chunk in &scrollback_data {
            merged.extend_from_slice(chunk);
        }
        let output_msg = BrowserMessage::Output { data: merged };
        if let Ok(json) = serde_json::to_string(&output_msg)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            return;
        }

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
                            Ok(BrowserInput::Input { data }) => {
                                if data.len() > 4096 {
                                    tracing::warn!("browser terminal input exceeds 4096 bytes, closing connection");
                                    break;
                                }
                                // Flush any buffered output before forwarding input
                                if !output_buf.is_empty() {
                                    let msg = BrowserMessage::Output { data: std::mem::take(&mut output_buf) };
                                    coalesce_deadline = None;
                                    if let Ok(json) = serde_json::to_string(&msg)
                                        && socket.send(Message::Text(json.into())).await.is_err()
                                    {
                                        break;
                                    }
                                }
                                // Write directly to PTY via session manager
                                let mut mgr = state.session_manager.lock().await;
                                if let Err(e) = mgr.write_to(&session_id, data.as_bytes()) {
                                    tracing::warn!(error = %e, "failed to write to PTY");
                                    break;
                                }
                            }
                            Ok(BrowserInput::Resize { cols, rows }) => {
                                let mgr = state.session_manager.lock().await;
                                if let Err(e) = mgr.resize(&session_id, cols, rows) {
                                    tracing::warn!(error = %e, "failed to resize PTY");
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
                    Some(BrowserMessage::Output { data }) => {
                        output_buf.extend_from_slice(&data);
                        if coalesce_deadline.is_none() {
                            coalesce_deadline = Some(Instant::now() + COALESCE_WINDOW);
                        }
                    }
                    Some(msg) => {
                        // Non-output messages (session_closed, error) flush buffer first
                        if !output_buf.is_empty() {
                            let flush = BrowserMessage::Output { data: std::mem::take(&mut output_buf) };
                            coalesce_deadline = None;
                            if let Ok(json) = serde_json::to_string(&flush)
                                && socket.send(Message::Text(json.into())).await.is_err()
                            {
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
                    let msg = BrowserMessage::Output { data: std::mem::take(&mut output_buf) };
                    coalesce_deadline = None;
                    if let Ok(json) = serde_json::to_string(&msg)
                        && socket.send(Message::Text(json.into())).await.is_err()
                    {
                        break;
                    }
                }
            }
        }
    }

    // Cleanup: sender drops automatically when this function returns,
    // try_send in the output loop will detect it and remove it.
}
