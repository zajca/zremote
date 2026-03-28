use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::IntoResponse;

use crate::state::AppState;

/// WebSocket upgrade handler for browser event stream.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        let rx = state.events.subscribe();
        zremote_core::events_ws::handle_events_websocket(socket, rx)
    })
}
