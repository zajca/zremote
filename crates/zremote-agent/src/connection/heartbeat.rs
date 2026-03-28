use std::time::Duration;

use futures_util::SinkExt;
use futures_util::stream::SplitSink;
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_tungstenite::tungstenite::Message;
use zremote_protocol::{AgentMessage, AgenticAgentMessage};

use super::{ConnectionError, WsStream, serialize_agent_message, serialize_agentic_message};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

type WsSink = SplitSink<WsStream, Message>;

/// Run the sender task: drains outbound + agentic channels and sends heartbeats to the WS sink.
///
/// Returns the sink when shutting down so the caller can close it cleanly.
pub(super) async fn run_sender(
    mut ws_sink: WsSink,
    shutdown: tokio::sync::watch::Receiver<bool>,
    mut outbound_rx: mpsc::Receiver<AgentMessage>,
    mut agentic_rx: mpsc::Receiver<AgenticAgentMessage>,
) -> WsSink {
    let mut heartbeat_interval = interval(HEARTBEAT_INTERVAL);
    // Skip the first immediate tick
    heartbeat_interval.tick().await;

    loop {
        // biased: shutdown + heartbeat checked first so they aren't
        // starved when outbound_rx is saturated with PTY output.
        tokio::select! {
            biased;

            () = super::wait_for_shutdown(shutdown.clone()) => {
                tracing::debug!("sender task shutting down");
                return ws_sink;
            }
            _ = heartbeat_interval.tick() => {
                let msg = AgentMessage::Heartbeat {
                    timestamp: chrono::Utc::now(),
                };
                match serialize_agent_message(&msg) {
                    Ok(ws_msg) => {
                        if let Err(e) = ws_sink.send(ws_msg).await {
                            tracing::error!(error = %e, "failed to send heartbeat");
                            return ws_sink;
                        }
                        tracing::debug!("heartbeat sent");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to serialize heartbeat");
                    }
                }
            }
            Some(msg) = outbound_rx.recv() => {
                match serialize_agent_message(&msg) {
                    Ok(ws_msg) => {
                        if let Err(e) = ws_sink.send(ws_msg).await {
                            tracing::error!(error = %e, "failed to send outbound message");
                            return ws_sink;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to serialize outbound message");
                    }
                }
            }
            Some(msg) = agentic_rx.recv() => {
                match serialize_agentic_message(&msg) {
                    Ok(ws_msg) => {
                        if let Err(e) = ws_sink.send(ws_msg).await {
                            tracing::error!(error = %e, "failed to send agentic message");
                            return ws_sink;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to serialize agentic message");
                    }
                }
            }
        }
    }
}
