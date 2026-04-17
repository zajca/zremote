use std::sync::Mutex;

use zremote_client::{ApiClient, ClientEvent};

use crate::persistence::Persistence;

/// Shared application state accessible from all GPUI views.
pub struct AppState {
    /// HTTP client for REST API calls.
    pub api: ApiClient,
    /// Handle to the tokio runtime running on background threads.
    pub tokio_handle: tokio::runtime::Handle,
    /// Receiver for real-time server events and connection status (from /ws/events WebSocket).
    pub event_rx: flume::Receiver<ClientEvent>,
    /// Keep the event stream alive (dropping it cancels the background task).
    pub _event_stream: zremote_client::EventStream,
    /// Server mode: "server" or "local".
    pub mode: String,
    /// Server/agent version (from /api/mode response).
    pub server_version: Option<String>,
    /// Persistent GUI state (window size, selected session, etc.).
    pub persistence: Mutex<Persistence>,
    /// Currently selected project (or worktree) in the sidebar. When set,
    /// terminal panel filters sessions to this project and breadcrumb
    /// reflects the active context. `None` means "no project selected".
    ///
    /// Held behind a `Mutex` so views can read from `&self` (GPUI render
    /// borrows are immutable) and mutate from click handlers. Mutation must
    /// be followed by `cx.notify()` on views that depend on it (sidebar,
    /// terminal_panel, main_view breadcrumb).
    pub selected_project_id: Mutex<Option<String>>,
}
