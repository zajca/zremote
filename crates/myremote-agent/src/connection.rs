use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use myremote_protocol::{AgentMessage, HostId, ServerMessage};
use tokio::time::{interval, timeout};
use tokio_tungstenite::tungstenite::Message;

use crate::config::AgentConfig;

/// Errors that can occur during a WebSocket connection lifecycle.
#[derive(Debug)]
pub enum ConnectionError {
    /// WebSocket connection failed.
    Connect(tokio_tungstenite::tungstenite::Error),
    /// Failed to serialize a message.
    Serialize(serde_json::Error),
    /// Failed to deserialize a message from the server.
    Deserialize(serde_json::Error),
    /// Failed to send a WebSocket message.
    Send(tokio_tungstenite::tungstenite::Error),
    /// Failed to receive a WebSocket message.
    Receive(tokio_tungstenite::tungstenite::Error),
    /// Registration timed out waiting for server acknowledgement.
    RegisterTimeout,
    /// Unexpected message received during registration.
    UnexpectedRegisterResponse(String),
    /// Failed to resolve the system hostname.
    Hostname(std::io::Error),
    /// Server sent an error message.
    ServerError(String),
    /// WebSocket connection was closed.
    ConnectionClosed,
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "WebSocket connection failed: {e}"),
            Self::Serialize(e) => write!(f, "failed to serialize message: {e}"),
            Self::Deserialize(e) => write!(f, "failed to deserialize server message: {e}"),
            Self::Send(e) => write!(f, "failed to send WebSocket message: {e}"),
            Self::Receive(e) => write!(f, "failed to receive WebSocket message: {e}"),
            Self::RegisterTimeout => {
                write!(f, "registration timed out (no RegisterAck within 10s)")
            }
            Self::UnexpectedRegisterResponse(msg) => {
                write!(f, "unexpected response during registration: {msg}")
            }
            Self::Hostname(e) => write!(f, "failed to get hostname: {e}"),
            Self::ServerError(msg) => write!(f, "server error: {msg}"),
            Self::ConnectionClosed => write!(f, "WebSocket connection closed"),
        }
    }
}

impl std::error::Error for ConnectionError {}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Establish a WebSocket connection to the server.
async fn connect(config: &AgentConfig) -> Result<WsStream, ConnectionError> {
    let (ws_stream, _response) = tokio_tungstenite::connect_async(config.server_url.as_str())
        .await
        .map_err(ConnectionError::Connect)?;
    Ok(ws_stream)
}

/// Send a JSON-encoded agent message over the WebSocket.
async fn send_message(ws: &mut WsStream, msg: &AgentMessage) -> Result<(), ConnectionError> {
    let json = serde_json::to_string(msg).map_err(ConnectionError::Serialize)?;
    ws.send(Message::Text(json.into()))
        .await
        .map_err(ConnectionError::Send)
}

/// Register with the server and return the assigned host ID.
async fn register(ws: &mut WsStream, config: &AgentConfig) -> Result<HostId, ConnectionError> {
    let hostname = hostname::get()
        .map_err(ConnectionError::Hostname)?
        .to_string_lossy()
        .into_owned();

    let register_msg = AgentMessage::Register {
        hostname,
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        token: config.token.clone(),
    };

    send_message(ws, &register_msg).await?;
    tracing::debug!("sent Register message, waiting for RegisterAck");

    // Wait for RegisterAck with timeout
    let ack = timeout(REGISTER_TIMEOUT, async {
        while let Some(msg_result) = ws.next().await {
            let msg = msg_result.map_err(ConnectionError::Receive)?;
            match msg {
                Message::Text(text) => {
                    let server_msg: ServerMessage =
                        serde_json::from_str(&text).map_err(ConnectionError::Deserialize)?;
                    return Ok(server_msg);
                }
                Message::Close(_) => return Err(ConnectionError::ConnectionClosed),
                // Skip ping/pong/binary during registration
                _ => {},
            }
        }
        Err(ConnectionError::ConnectionClosed)
    })
    .await
    .map_err(|_| ConnectionError::RegisterTimeout)??;

    match ack {
        ServerMessage::RegisterAck { host_id } => {
            tracing::info!(host_id = %host_id, "registered with server");
            Ok(host_id)
        }
        ServerMessage::Error { message } => Err(ConnectionError::ServerError(message)),
        other => Err(ConnectionError::UnexpectedRegisterResponse(format!(
            "{other:?}"
        ))),
    }
}

/// Handle a server message. Returns `false` if the connection should be closed.
fn handle_server_message(msg: &ServerMessage, host_id: &HostId) -> bool {
    match msg {
        ServerMessage::HeartbeatAck { timestamp } => {
            tracing::debug!(host_id = %host_id, timestamp = %timestamp, "heartbeat acknowledged");
        }
        ServerMessage::SessionCreate { session_id, .. } => {
            tracing::warn!(
                host_id = %host_id,
                session_id = %session_id,
                "received SessionCreate but PTY sessions are not yet implemented"
            );
        }
        ServerMessage::SessionClose { session_id } => {
            tracing::warn!(
                host_id = %host_id,
                session_id = %session_id,
                "received SessionClose but PTY sessions are not yet implemented"
            );
        }
        ServerMessage::TerminalInput { session_id, .. } => {
            tracing::warn!(
                host_id = %host_id,
                session_id = %session_id,
                "received TerminalInput but PTY sessions are not yet implemented"
            );
        }
        ServerMessage::TerminalResize { session_id, .. } => {
            tracing::warn!(
                host_id = %host_id,
                session_id = %session_id,
                "received TerminalResize but PTY sessions are not yet implemented"
            );
        }
        ServerMessage::Error { message } => {
            tracing::error!(host_id = %host_id, error = %message, "server error");
        }
        ServerMessage::RegisterAck { .. } => {
            tracing::warn!(host_id = %host_id, "received unexpected RegisterAck after registration");
        }
    }
    true
}

/// Run a single connection lifecycle: connect, register, then process messages.
///
/// Returns `Ok(())` on clean disconnect, `Err` on failure.
/// The caller is responsible for reconnection logic.
pub async fn run_connection(
    config: &AgentConfig,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), ConnectionError> {
    tracing::info!(server_url = %config.server_url, "connecting to server");

    let mut ws = connect(config).await?;
    tracing::info!("WebSocket connection established");

    let host_id = register(&mut ws, config).await?;

    // Split the WebSocket for concurrent read/write
    let (mut ws_sink, mut ws_stream) = ws.split();

    // Spawn heartbeat task
    let heartbeat_shutdown = shutdown.clone();
    let heartbeat_handle = tokio::spawn(async move {
        let mut heartbeat_interval = interval(HEARTBEAT_INTERVAL);
        // Skip the first immediate tick
        heartbeat_interval.tick().await;

        loop {
            tokio::select! {
                _ = heartbeat_interval.tick() => {
                    let msg = AgentMessage::Heartbeat {
                        timestamp: Utc::now(),
                    };
                    let json = match serde_json::to_string(&msg) {
                        Ok(j) => j,
                        Err(e) => {
                            tracing::error!(error = %e, "failed to serialize heartbeat");
                            continue;
                        }
                    };
                    if let Err(e) = ws_sink.send(Message::Text(json.into())).await {
                        tracing::error!(error = %e, "failed to send heartbeat");
                        return ws_sink;
                    }
                    tracing::debug!("heartbeat sent");
                }
                () = wait_for_shutdown(heartbeat_shutdown.clone()) => {
                    tracing::debug!("heartbeat task shutting down");
                    return ws_sink;
                }
            }
        }
    });

    // Main message loop
    let result = loop {
        tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(server_msg) => {
                                if !handle_server_message(&server_msg, &host_id) {
                                    break Ok(());
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "failed to parse server message, ignoring");
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        tracing::info!(host_id = %host_id, "server closed connection");
                        break Ok(());
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_))) => {
                        // tokio-tungstenite handles ping/pong automatically
                    }
                    Some(Err(e)) => {
                        tracing::error!(error = %e, "WebSocket error");
                        break Err(ConnectionError::Receive(e));
                    }
                    None => {
                        tracing::info!(host_id = %host_id, "WebSocket stream ended");
                        break Err(ConnectionError::ConnectionClosed);
                    }
                }
            }
            () = wait_for_shutdown(shutdown.clone()) => {
                tracing::info!(host_id = %host_id, "shutdown signal received, closing connection");
                break Ok(());
            }
        }
    };

    // Wait for heartbeat task to finish and close the WebSocket cleanly
    match heartbeat_handle.await {
        Ok(mut sink) => {
            let _ = sink.send(Message::Close(None)).await;
            let _ = sink.close().await;
        }
        Err(e) => {
            tracing::error!(error = %e, "heartbeat task panicked");
        }
    }

    result
}

/// Wait until the shutdown signal is received.
async fn wait_for_shutdown(mut rx: tokio::sync::watch::Receiver<bool>) {
    // If already shut down, return immediately
    if *rx.borrow() {
        return;
    }
    // Wait for the value to change to true
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use myremote_protocol::ServerMessage;
    use uuid::Uuid;

    #[test]
    fn connection_error_display_connect() {
        let err = ConnectionError::RegisterTimeout;
        assert!(err.to_string().contains("registration timed out"));
    }

    #[test]
    fn connection_error_display_receive() {
        let inner = tokio_tungstenite::tungstenite::Error::ConnectionClosed;
        let err = ConnectionError::Receive(inner);
        assert!(err.to_string().contains("receive"));
    }

    #[test]
    fn connection_error_display_hostname() {
        let err = ConnectionError::Hostname(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no hostname",
        ));
        assert!(err.to_string().contains("hostname"));
    }

    #[test]
    fn connection_error_display_server_error() {
        let err = ConnectionError::ServerError("bad token".to_string());
        assert!(err.to_string().contains("bad token"));
    }

    #[test]
    fn connection_error_display_unexpected_response() {
        let err = ConnectionError::UnexpectedRegisterResponse("HeartbeatAck".to_string());
        assert!(err.to_string().contains("HeartbeatAck"));
    }

    #[test]
    fn connection_error_display_closed() {
        let err = ConnectionError::ConnectionClosed;
        assert!(err.to_string().contains("closed"));
    }

    #[test]
    fn handle_server_message_heartbeat_ack_returns_true() {
        let host_id = Uuid::new_v4();
        let msg = ServerMessage::HeartbeatAck {
            timestamp: Utc::now(),
        };
        assert!(handle_server_message(&msg, &host_id));
    }

    #[test]
    fn handle_server_message_session_create_returns_true() {
        let host_id = Uuid::new_v4();
        let msg = ServerMessage::SessionCreate {
            session_id: Uuid::new_v4(),
            shell: Some("/bin/bash".to_string()),
            cols: 80,
            rows: 24,
            working_dir: None,
        };
        assert!(handle_server_message(&msg, &host_id));
    }

    #[test]
    fn handle_server_message_session_close_returns_true() {
        let host_id = Uuid::new_v4();
        let msg = ServerMessage::SessionClose {
            session_id: Uuid::new_v4(),
        };
        assert!(handle_server_message(&msg, &host_id));
    }

    #[test]
    fn handle_server_message_terminal_input_returns_true() {
        let host_id = Uuid::new_v4();
        let msg = ServerMessage::TerminalInput {
            session_id: Uuid::new_v4(),
            data: vec![0x41],
        };
        assert!(handle_server_message(&msg, &host_id));
    }

    #[test]
    fn handle_server_message_terminal_resize_returns_true() {
        let host_id = Uuid::new_v4();
        let msg = ServerMessage::TerminalResize {
            session_id: Uuid::new_v4(),
            cols: 120,
            rows: 40,
        };
        assert!(handle_server_message(&msg, &host_id));
    }

    #[test]
    fn handle_server_message_error_returns_true() {
        let host_id = Uuid::new_v4();
        let msg = ServerMessage::Error {
            message: "test error".to_string(),
        };
        assert!(handle_server_message(&msg, &host_id));
    }

    #[test]
    fn handle_server_message_unexpected_register_ack_returns_true() {
        let host_id = Uuid::new_v4();
        let msg = ServerMessage::RegisterAck {
            host_id: Uuid::new_v4(),
        };
        assert!(handle_server_message(&msg, &host_id));
    }

    #[tokio::test]
    async fn wait_for_shutdown_returns_immediately_if_already_true() {
        let (tx, rx) = tokio::sync::watch::channel(true);
        // Should return immediately without blocking
        tokio::time::timeout(Duration::from_millis(100), wait_for_shutdown(rx))
            .await
            .expect("should complete immediately when already shut down");
        drop(tx);
    }

    #[tokio::test]
    async fn wait_for_shutdown_waits_for_signal() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            wait_for_shutdown(rx).await;
        });

        // Signal shutdown
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_millis(100), handle)
            .await
            .expect("should complete after signal")
            .expect("task should not panic");
    }

    #[tokio::test]
    async fn connect_to_invalid_url_returns_error() {
        let config = AgentConfig {
            server_url: url::Url::parse("ws://127.0.0.1:1").unwrap(),
            token: "test".to_string(),
        };
        let result = connect(&config).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConnectionError::Connect(_)));
    }
}
