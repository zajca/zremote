use std::sync::Arc;

use axum::extract::{Path, State};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use myremote_protocol::ServerMessage;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::state::{AppState, BrowserMessage};

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

    // Clone scrollback and register sender under the lock, then drop the lock before sending
    let scrollback_data;
    {
        let mut sessions = state.sessions.write().await;
        let Some(session) = sessions.get_mut(&session_id) else {
            let error_msg = serde_json::json!({
                "type": "error",
                "message": "session not found or not active"
            });
            let _ = socket
                .send(Message::Text(error_msg.to_string().into()))
                .await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        };

        if session.status != "active" && session.status != "creating" {
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

    // Send scrollback buffer without holding the write lock
    for chunk in &scrollback_data {
        let msg = BrowserMessage::Output {
            data: chunk.clone(),
        };
        if let Ok(json) = serde_json::to_string(&msg)
            && socket
                .send(Message::Text(json.into()))
                .await
                .is_err()
        {
            return;
        }
    }

    // Bidirectional relay loop
    loop {
        tokio::select! {
            // Browser -> server
            ws_msg = socket.recv() => {
                match ws_msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<BrowserInput>(&text) {
                            Ok(BrowserInput::Input { data }) => {
                                if data.len() > 4096 {
                                    tracing::warn!("browser terminal input exceeds 4096 bytes, closing connection");
                                    break;
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
                                if let Some(sender) = state.connections.get_sender(&host_id).await {
                                    let msg = ServerMessage::TerminalResize {
                                        session_id,
                                        cols,
                                        rows,
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
            // Server -> browser
            browser_msg = rx.recv() => {
                match browser_msg {
                    Some(msg) => {
                        if let Ok(json) = serde_json::to_string(&msg)
                            && socket.send(Message::Text(json.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    // Cleanup: sender drops automatically when this function returns,
    // try_send in the relay loop will detect it and remove it.
}
