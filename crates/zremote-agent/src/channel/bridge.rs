use std::collections::HashMap;

use zremote_protocol::SessionId;
use zremote_protocol::channel::ChannelMessage;

use super::port;

/// Manages per-session channel connections.
/// Discovers channel servers via port files, sends messages via HTTP.
pub struct ChannelBridge {
    channels: HashMap<SessionId, ChannelConnection>,
    http_client: reqwest::Client,
}

struct ChannelConnection {
    port: u16,
    base_url: String,
}

/// Errors that can occur when interacting with a channel server.
#[derive(Debug)]
pub enum ChannelBridgeError {
    /// No channel connection found for the given session.
    NotConnected,
    /// HTTP request to the channel server failed.
    Http(reqwest::Error),
    /// Channel server returned a non-success status.
    ServerError(String),
}

impl std::fmt::Display for ChannelBridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => write!(f, "no channel connection for session"),
            Self::Http(e) => write!(f, "channel HTTP error: {e}"),
            Self::ServerError(msg) => write!(f, "channel server error: {msg}"),
        }
    }
}

impl std::error::Error for ChannelBridgeError {}

impl ChannelBridge {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
            http_client: reqwest::Client::new(),
        }
    }

    /// Discover a channel server for a session by reading its port file.
    /// Returns `true` if discovered, `false` if port file not found.
    pub async fn discover(&mut self, session_id: SessionId) -> Result<bool, std::io::Error> {
        match port::read_port_file(&session_id).await {
            Ok(port) => {
                self.register(session_id, port);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Register a channel server for a session with a known port.
    /// Use this when the port was discovered outside the lock.
    pub fn register(&mut self, session_id: SessionId, port: u16) {
        let base_url = format!("http://127.0.0.1:{port}");
        tracing::info!(session = %session_id, port, "registered channel server");
        self.channels
            .insert(session_id, ChannelConnection { port, base_url });
    }

    /// Send a `ChannelMessage` to a session's channel server.
    pub async fn send(
        &self,
        session_id: &SessionId,
        msg: &ChannelMessage,
    ) -> Result<(), ChannelBridgeError> {
        let conn = self
            .channels
            .get(session_id)
            .ok_or(ChannelBridgeError::NotConnected)?;

        let resp = self
            .http_client
            .post(format!("{}/notify", conn.base_url))
            .json(msg)
            .send()
            .await
            .map_err(ChannelBridgeError::Http)?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(ChannelBridgeError::ServerError(format!("{status}: {body}")))
        }
    }

    /// Send a permission response to a session's channel server.
    pub async fn respond_permission(
        &self,
        session_id: &SessionId,
        request_id: &str,
        allowed: bool,
        reason: Option<&str>,
    ) -> Result<(), ChannelBridgeError> {
        let conn = self
            .channels
            .get(session_id)
            .ok_or(ChannelBridgeError::NotConnected)?;

        let body = serde_json::json!({
            "request_id": request_id,
            "allowed": allowed,
            "reason": reason,
        });

        let resp = self
            .http_client
            .post(format!("{}/permission-response", conn.base_url))
            .json(&body)
            .send()
            .await
            .map_err(ChannelBridgeError::Http)?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(ChannelBridgeError::ServerError(format!("{status}: {body}")))
        }
    }

    /// Check if a channel is available for a session.
    pub fn is_available(&self, session_id: &SessionId) -> bool {
        self.channels.contains_key(session_id)
    }

    /// Remove a channel connection (on session close).
    pub fn remove(&mut self, session_id: &SessionId) {
        self.channels.remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn new_bridge_has_no_channels() {
        let bridge = ChannelBridge::new();
        let id = Uuid::new_v4();
        assert!(!bridge.is_available(&id));
    }

    #[test]
    fn remove_nonexistent_is_noop() {
        let mut bridge = ChannelBridge::new();
        bridge.remove(&Uuid::new_v4());
    }

    #[tokio::test]
    async fn discover_without_port_file() {
        let mut bridge = ChannelBridge::new();
        let found = bridge.discover(Uuid::new_v4()).await.unwrap();
        assert!(!found);
    }

    #[tokio::test]
    async fn send_not_connected() {
        let bridge = ChannelBridge::new();
        let result = bridge
            .send(
                &Uuid::new_v4(),
                &ChannelMessage::Signal {
                    action: zremote_protocol::channel::SignalAction::Continue,
                    reason: None,
                },
            )
            .await;
        assert!(matches!(result, Err(ChannelBridgeError::NotConnected)));
    }

    #[tokio::test]
    async fn respond_permission_not_connected() {
        let bridge = ChannelBridge::new();
        let result = bridge
            .respond_permission(&Uuid::new_v4(), "req-1", true, None)
            .await;
        assert!(matches!(result, Err(ChannelBridgeError::NotConnected)));
    }

    #[test]
    fn manual_insert_and_remove() {
        let mut bridge = ChannelBridge::new();
        let session_id = Uuid::new_v4();

        bridge.channels.insert(
            session_id,
            ChannelConnection {
                port: 12345,
                base_url: "http://127.0.0.1:12345".to_string(),
            },
        );
        assert!(bridge.is_available(&session_id));

        bridge.remove(&session_id);
        assert!(!bridge.is_available(&session_id));
    }

    #[test]
    fn error_display() {
        let err = ChannelBridgeError::NotConnected;
        assert!(err.to_string().contains("no channel connection"));

        let err = ChannelBridgeError::ServerError("500: oops".to_string());
        assert!(err.to_string().contains("500: oops"));
    }

    #[tokio::test]
    async fn send_to_real_channel_server() {
        use super::super::http;
        use super::super::types::ChannelState;

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let state = ChannelState {
            session_id: Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:0".to_string(),
            stdio_tx: tx,
            http_client: reqwest::Client::new(),
        };
        let app = http::router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let mut bridge = ChannelBridge::new();
        let session_id = state.session_id;
        bridge.channels.insert(
            session_id,
            ChannelConnection {
                port: addr.port(),
                base_url: format!("http://127.0.0.1:{}", addr.port()),
            },
        );

        let msg = ChannelMessage::Signal {
            action: zremote_protocol::channel::SignalAction::Abort,
            reason: Some("test abort".to_string()),
        };
        bridge.send(&session_id, &msg).await.unwrap();

        let evt = rx.try_recv().unwrap();
        assert!(matches!(
            evt,
            super::super::types::StdioEvent::ChannelNotify(_)
        ));
    }

    #[tokio::test]
    async fn respond_permission_to_real_channel_server() {
        use super::super::http;
        use super::super::types::ChannelState;

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let state = ChannelState {
            session_id: Uuid::new_v4(),
            agent_callback: "http://127.0.0.1:0".to_string(),
            stdio_tx: tx,
            http_client: reqwest::Client::new(),
        };
        let app = http::router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let mut bridge = ChannelBridge::new();
        let session_id = state.session_id;
        bridge.channels.insert(
            session_id,
            ChannelConnection {
                port: addr.port(),
                base_url: format!("http://127.0.0.1:{}", addr.port()),
            },
        );

        bridge
            .respond_permission(&session_id, "perm-test", true, Some("approved"))
            .await
            .unwrap();

        let evt = rx.try_recv().unwrap();
        match evt {
            super::super::types::StdioEvent::PermissionResponse {
                request_id,
                allowed,
                reason,
            } => {
                assert_eq!(request_id, "perm-test");
                assert!(allowed);
                assert_eq!(reason.unwrap(), "approved");
            }
            _ => panic!("expected PermissionResponse"),
        }
    }
}
