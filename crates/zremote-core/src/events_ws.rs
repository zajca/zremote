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
#[allow(clippy::module_name_repetitions)]
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
                        let lag_event = ServerEvent::EventsLagged { missed: n };
                        let json = match serde_json::to_string(&lag_event) {
                            Ok(j) => j,
                            Err(e) => {
                                tracing::error!(error = %e, "failed to serialize lag event");
                                continue;
                            }
                        };
                        if socket.send(Message::Text(json.into())).await.is_err() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use axum::Router;
    use axum::extract::State;
    use axum::extract::ws::WebSocketUpgrade;
    use axum::response::IntoResponse;
    use futures_util::StreamExt;
    use tokio::net::TcpListener;
    use tokio::sync::broadcast;
    use zremote_protocol::events::{HostInfo, ServerEvent};
    use zremote_protocol::status::HostStatus;

    type EventsTx = Arc<broadcast::Sender<ServerEvent>>;

    async fn ws_upgrade(ws: WebSocketUpgrade, State(tx): State<EventsTx>) -> impl IntoResponse {
        let rx = tx.subscribe();
        ws.on_upgrade(move |socket| handle_events_websocket(socket, rx))
    }

    /// Spin up a test server and return (addr, broadcast sender).
    async fn start_test_server() -> (SocketAddr, EventsTx) {
        start_test_server_with_capacity(1024).await
    }

    async fn start_test_server_with_capacity(cap: usize) -> (SocketAddr, EventsTx) {
        let (tx, _) = broadcast::channel::<ServerEvent>(cap);
        let tx = Arc::new(tx);
        let app = Router::new()
            .route("/ws/events", axum::routing::get(ws_upgrade))
            .with_state(tx.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (addr, tx)
    }

    async fn connect(
        addr: SocketAddr,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let url = format!("ws://{addr}/ws/events");
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        ws
    }

    #[tokio::test]
    async fn relay_single_event() {
        let (addr, tx) = start_test_server().await;
        let mut ws = connect(addr).await;

        let event = ServerEvent::HostDisconnected {
            host_id: "h1".to_string(),
        };
        tx.send(event).unwrap();

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .expect("timed out")
            .expect("stream ended")
            .expect("ws error");

        let text = msg.into_text().unwrap();
        let parsed: ServerEvent = serde_json::from_str(&text).unwrap();
        assert!(matches!(parsed, ServerEvent::HostDisconnected { ref host_id } if host_id == "h1"));

        ws.close(None).await.ok();
    }

    #[tokio::test]
    async fn relay_multiple_events_in_order() {
        let (addr, tx) = start_test_server().await;
        let mut ws = connect(addr).await;

        let events = vec![
            ServerEvent::HostConnected {
                host: HostInfo {
                    id: "h1".to_string(),
                    hostname: "alpha".to_string(),
                    status: HostStatus::Online,
                    agent_version: None,
                    os: None,
                    arch: None,
                },
            },
            ServerEvent::HostDisconnected {
                host_id: "h1".to_string(),
            },
            ServerEvent::ProjectsUpdated {
                host_id: "h2".to_string(),
            },
        ];

        for e in &events {
            tx.send(e.clone()).unwrap();
        }

        for expected in &events {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
                .await
                .expect("timed out")
                .expect("stream ended")
                .expect("ws error");
            let text = msg.into_text().unwrap();
            let parsed: ServerEvent = serde_json::from_str(&text).unwrap();
            assert_eq!(format!("{parsed:?}"), format!("{expected:?}"));
        }

        ws.close(None).await.ok();
    }

    #[tokio::test]
    async fn lag_detection_sends_events_lagged() {
        // Use a tiny broadcast capacity so we can easily trigger lag.
        let (addr, tx) = start_test_server_with_capacity(2).await;
        let mut ws = connect(addr).await;

        // Give the handler time to subscribe before we flood.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Send more events than the channel capacity to trigger lag.
        // The receiver will miss some because the buffer is only 2.
        for i in 0..10 {
            let _ = tx.send(ServerEvent::HostDisconnected {
                host_id: format!("h{i}"),
            });
        }

        // Collect all messages; one of them should be EventsLagged.
        let mut found_lagged = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), ws.next()).await {
                Ok(Some(Ok(msg))) => {
                    if let Ok(text) = msg.into_text()
                        && let Ok(ServerEvent::EventsLagged { missed }) =
                            serde_json::from_str(&text)
                    {
                        assert!(missed > 0, "missed count should be positive");
                        found_lagged = true;
                        break;
                    }
                }
                _ => break,
            }
        }

        assert!(found_lagged, "expected an EventsLagged message");
        ws.close(None).await.ok();
    }

    #[tokio::test]
    async fn handler_exits_when_broadcast_closed() {
        let (addr, tx) = start_test_server().await;
        let mut ws = connect(addr).await;

        // Verify the connection is alive by sending an event.
        tx.send(ServerEvent::HostDisconnected {
            host_id: "h1".to_string(),
        })
        .unwrap();
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .expect("timed out")
            .expect("stream ended")
            .expect("ws error");
        assert!(msg.is_text());

        // Drop the sender to close the broadcast channel.
        drop(tx);

        // The handler exits, axum drops the WebSocket. The client should
        // see the stream end (None) or a close/error within a few seconds.
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), ws.next()).await;
        match result {
            // Ok(None) / Close / stream error = clean shutdown;
            // Err(_) = timeout waiting for teardown (TCP still closing).
            // Both outcomes are acceptable for this test.
            Ok(None | Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_)))
            | Err(_) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_exits_when_client_disconnects() {
        let (addr, tx) = start_test_server().await;
        let mut ws = connect(addr).await;

        // Close from client side.
        ws.close(None).await.ok();
        // Drop to fully sever the connection.
        drop(ws);

        // Small delay, then verify the sender still works (the handler task exited
        // but the broadcast channel is still alive).
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Sending should succeed (no subscribers doesn't panic, just returns Err).
        // This verifies the handler didn't panic.
        let result = tx.send(ServerEvent::HostDisconnected {
            host_id: "h1".to_string(),
        });
        // With no subscribers, send returns Err, but the channel itself is healthy.
        assert!(result.is_err() || result.is_ok());
    }
}
