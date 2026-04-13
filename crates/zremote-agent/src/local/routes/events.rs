use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::IntoResponse;

use crate::local::state::LocalAppState;

/// WebSocket upgrade handler for browser event stream.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<LocalAppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        let rx = state.events.subscribe();
        zremote_core::events_ws::handle_events_websocket(socket, rx)
    })
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
            std::path::PathBuf::from("/tmp/zremote-test"),
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
