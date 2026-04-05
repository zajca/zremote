use zremote_protocol::SessionId;
use zremote_protocol::channel::ChannelMessage;

/// Event from HTTP server to MCP stdio loop.
#[derive(Debug)]
pub enum StdioEvent {
    /// Push a channel message to CC.
    ChannelNotify(ChannelMessage),
    /// Push a permission response to CC.
    PermissionResponse {
        request_id: String,
        allowed: bool,
        reason: Option<String>,
    },
}

/// Shared state for the channel server.
#[derive(Clone)]
pub struct ChannelState {
    pub session_id: SessionId,
    pub agent_callback: String,
    pub stdio_tx: tokio::sync::mpsc::Sender<StdioEvent>,
    pub http_client: reqwest::Client,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn stdio_event_debug_channel_notify() {
        let evt = StdioEvent::ChannelNotify(ChannelMessage::Signal {
            action: zremote_protocol::channel::SignalAction::Continue,
            reason: None,
        });
        let debug = format!("{evt:?}");
        assert!(debug.contains("ChannelNotify"));
    }

    #[test]
    fn stdio_event_debug_permission_response() {
        let evt = StdioEvent::PermissionResponse {
            request_id: "req-1".to_string(),
            allowed: true,
            reason: None,
        };
        let debug = format!("{evt:?}");
        assert!(debug.contains("PermissionResponse"));
    }

    #[test]
    fn channel_state_is_clone() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let state = ChannelState {
            session_id: Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:9999".to_string(),
            stdio_tx: tx,
            http_client: reqwest::Client::new(),
        };
        let cloned = state.clone();
        assert_eq!(cloned.session_id, state.session_id);
        assert_eq!(cloned.agent_callback, state.agent_callback);
    }
}
