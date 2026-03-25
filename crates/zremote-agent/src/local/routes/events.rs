use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;

use crate::local::state::LocalAppState;

/// WebSocket upgrade handler for browser event stream.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<LocalAppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_events_connection(socket, state))
}

async fn handle_events_connection(mut socket: WebSocket, state: Arc<LocalAppState>) {
    let mut rx = state.events.subscribe();

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
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "events client lagged, client should re-fetch state");
                        let hint = serde_json::json!({"type": "lagged", "skipped": n});
                        if socket.send(Message::Text(hint.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
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
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;
    use zremote_core::state::ServerEvent;

    use crate::local::state::LocalAppState;

    #[tokio::test]
    async fn events_broadcast_channel_works() {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let host_id = Uuid::new_v4();
        let state = LocalAppState::new(
            pool,
            "host".to_string(),
            host_id,
            shutdown,
            crate::config::PersistenceBackend::None,
        );

        let mut rx = state.events.subscribe();

        let event = ServerEvent::SessionClosed {
            session_id: "test-session".to_string(),
            exit_code: Some(0),
        };
        state.events.send(event).unwrap();

        let received = rx.recv().await.unwrap();
        match received {
            ServerEvent::SessionClosed {
                session_id,
                exit_code,
            } => {
                assert_eq!(session_id, "test-session");
                assert_eq!(exit_code, Some(0));
            }
            _ => panic!("expected SessionClosed event"),
        }
    }
}
