use crate::api::ApiClient;
use crate::types::ServerEvent;

/// Shared application state accessible from all GPUI views.
pub struct AppState {
    /// HTTP client for REST API calls.
    pub api: ApiClient,
    /// Handle to the tokio runtime running on background threads.
    pub tokio_handle: tokio::runtime::Handle,
    /// Receiver for real-time server events (from /ws/events WebSocket).
    pub event_rx: flume::Receiver<ServerEvent>,
    /// Server mode: "server" or "local".
    pub mode: String,
}
