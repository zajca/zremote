//! Shared events WebSocket handler logic.
//!
//! Both the server and local agent broadcast `ServerEvent`s to browser
//! clients over WebSocket. This module contains the shared relay loop.

use axum::extract::ws::{Message, WebSocket};
use tokio::sync::broadcast;

use crate::state::ServerEvent;

/// Relay events from a broadcast receiver to a WebSocket client.
///
/// The caller is responsible for WebSocket upgrade and subscribing to
/// the broadcast channel. This function handles the bidirectional loop:
/// forwarding events to the client and detecting client disconnect.
pub async fn handle_events_websocket(
    mut socket: WebSocket,
    mut rx: broadcast::Receiver<ServerEvent>,
) {
    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        let json = match serde_json::to_string(&event) {
                            Ok(j) => j,
                            Err(e) => {
                                tracing::error!(error = %e, "failed to serialize server event");
                                continue;
                            }
                        };
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "events client lagged, client should re-fetch state");
                        let hint = serde_json::json!({"type": "lagged", "skipped": n});
                        if socket.send(Message::Text(hint.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    _ => {}
                }
            }
        }
    }
}
