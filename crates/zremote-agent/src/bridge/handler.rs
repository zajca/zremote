use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use tokio::sync::mpsc;
use zremote_core::state::{BrowserMessage, encode_binary_output};
use zremote_protocol::SessionId;

use super::BridgeState;

/// Buffer size for the per-connection output channel.
const OUTPUT_CHANNEL_SIZE: usize = 256;

/// Commands sent from the bridge WS handler to the agent connection loop.
#[derive(Debug)]
pub enum BridgeCommand {
    Write {
        session_id: SessionId,
        pane_id: Option<String>,
        data: Vec<u8>,
    },
    Resize {
        session_id: SessionId,
        pane_id: Option<String>,
        cols: u16,
        rows: u16,
    },
}

/// Messages sent from the GUI to the bridge via WebSocket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum BridgeInput {
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
}

/// WebSocket upgrade handler for direct bridge terminal connections.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<BridgeState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_bridge_connection(socket, session_id, state))
}

async fn handle_bridge_connection(
    mut socket: WebSocket,
    session_id_str: String,
    state: BridgeState,
) {
    let Ok(session_id) = session_id_str.parse::<SessionId>() else {
        let _ = socket.send(Message::Close(None)).await;
        return;
    };

    tracing::info!(session_id = %session_id, "direct bridge GUI connected");

    // Snapshot scrollback while holding the read lock, then drop it before
    // any async I/O so we don't hold the lock across socket.send() awaits.
    let replay = {
        let guard = state.scrollback.read().await;
        guard
            .get(&session_id)
            .filter(|sb| !sb.chunks.is_empty())
            .map(|sb| (sb.cols, sb.rows, sb.snapshot()))
    };

    // Send scrollback replay BEFORE registering the sender.  This guarantees
    // the client always receives history first.  Output generated during the
    // replay window is not delivered to this connection (acceptable trade-off
    // vs. the alternative of interleaving live data before history).
    if let Some((cols, rows, chunks)) = replay {
        let start = BrowserMessage::ScrollbackStart { cols, rows };
        if let Ok(json) = serde_json::to_string(&start)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            tracing::info!(session_id = %session_id, "direct bridge GUI disconnected");
            return;
        }
        for chunk in &chunks {
            let frame = encode_binary_output(None, chunk);
            if socket.send(Message::Binary(frame.into())).await.is_err() {
                tracing::info!(session_id = %session_id, "direct bridge GUI disconnected");
                return;
            }
        }
        let end = BrowserMessage::ScrollbackEnd;
        if let Ok(json) = serde_json::to_string(&end)
            && socket.send(Message::Text(json.into())).await.is_err()
        {
            tracing::info!(session_id = %session_id, "direct bridge GUI disconnected");
            return;
        }
    }

    // Register output sender AFTER scrollback replay is complete.
    // This guarantees live output always follows history with no gaps.
    let (tx, mut rx) = mpsc::channel::<BrowserMessage>(OUTPUT_CHANNEL_SIZE);
    {
        let mut guard = state.senders.write().await;
        guard.entry(session_id).or_default().push(tx);
    }

    // Bidirectional relay loop.
    // No output coalescing here -- the GUI side coalesces repaints at 16ms.
    // Sending output immediately avoids double-coalescing latency.
    loop {
        tokio::select! {
            // GUI -> Agent (input/resize)
            ws_msg = socket.recv() => {
                match ws_msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<BridgeInput>(&text) {
                            Ok(BridgeInput::Input { mut data, pane_id }) => {
                                const MAX_INPUT_BYTES: usize = 1_048_576;
                                if data.len() > MAX_INPUT_BYTES {
                                    tracing::warn!(len = data.len(), "bridge input exceeds 1 MB, truncating");
                                    let boundary = data.floor_char_boundary(MAX_INPUT_BYTES);
                                    data.truncate(boundary);
                                }
                                if state.command_tx.try_send(BridgeCommand::Write {
                                    session_id,
                                    pane_id,
                                    data: data.into_bytes(),
                                }).is_err() {
                                    tracing::warn!("bridge command channel full, input dropped");
                                }
                            }
                            Ok(BridgeInput::Resize { cols, rows, pane_id }) => {
                                if state.command_tx.try_send(BridgeCommand::Resize {
                                    session_id,
                                    pane_id,
                                    cols,
                                    rows,
                                }).is_err() {
                                    tracing::warn!("bridge command channel full, resize dropped");
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "invalid bridge terminal message");
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Binary(_))) => {
                        tracing::warn!("unexpected binary message from bridge GUI");
                    }
                    Some(Err(e)) => {
                        tracing::warn!(error = %e, "bridge WebSocket error");
                        break;
                    }
                }
            }
            // Agent -> GUI: forward output immediately as binary frames
            browser_msg = rx.recv() => {
                match browser_msg {
                    Some(BrowserMessage::Output { pane_id, data }) => {
                        let frame = encode_binary_output(pane_id.as_deref(), &data);
                        if socket.send(Message::Binary(frame.into())).await.is_err() {
                            break;
                        }
                    }
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

    tracing::info!(session_id = %session_id, "direct bridge GUI disconnected");
    // Sender drops automatically; fan_out will clean it up on next send.
}
