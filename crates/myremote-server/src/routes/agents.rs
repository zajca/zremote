use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use chrono::Utc;
use myremote_protocol::{AgentMessage, HostId, ServerMessage};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::auth;
use crate::state::AppState;

/// Timeout for the first message (Register) after WebSocket upgrade.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(5);

/// Buffer size for the outbound message channel.
const OUTBOUND_CHANNEL_SIZE: usize = 256;

/// Heartbeat monitor interval.
const HEARTBEAT_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum time since last heartbeat before marking an agent as stale.
const HEARTBEAT_MAX_AGE: Duration = Duration::from_secs(90);

// TODO(phase-7): Add rate limiting on WebSocket connections
/// WebSocket upgrade handler for agent connections.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_agent_connection(socket, state))
}

/// Result of a successful agent registration handshake.
struct RegisteredAgent {
    host_id: HostId,
    generation: u64,
    rx: mpsc::Receiver<ServerMessage>,
}

/// Perform the registration handshake: wait for Register message, validate
/// token, upsert host, register connection, and send `RegisterAck`.
/// Returns `None` if any step fails (errors are sent to the agent).
async fn register_agent(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
) -> Option<RegisteredAgent> {
    // 1. Wait for Register message with timeout
    let register_msg = match tokio::time::timeout(REGISTER_TIMEOUT, recv_agent_message(socket))
        .await
    {
        Ok(Some(msg)) => msg,
        Ok(None) => {
            tracing::warn!("agent disconnected before sending Register");
            return None;
        }
        Err(_) => {
            tracing::warn!("agent did not send Register within timeout");
            let _ = send_server_message(
                socket,
                &ServerMessage::Error {
                    message: "registration timeout".to_string(),
                },
            )
            .await;
            return None;
        }
    };

    // 2. Validate that first message is Register
    let AgentMessage::Register {
        hostname,
        agent_version,
        os,
        arch,
        token,
    } = register_msg
    else {
        tracing::warn!("agent sent non-Register message as first message");
        let _ = send_server_message(
            socket,
            &ServerMessage::Error {
                message: "expected Register as first message".to_string(),
            },
        )
        .await;
        return None;
    };

    // 3. Validate token
    if !auth::verify_token(&token, &state.agent_token_hash) {
        tracing::warn!(hostname = %hostname, "agent authentication failed");
        let _ = send_server_message(
            socket,
            &ServerMessage::Error {
                message: "invalid authentication token".to_string(),
            },
        )
        .await;
        return None;
    }

    // 4. Upsert host in DB
    let host_id = match upsert_host(state, &hostname, &agent_version, &os, &arch, &token).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "failed to upsert host in database");
            let _ = send_server_message(
                socket,
                &ServerMessage::Error {
                    message: "internal server error".to_string(),
                },
            )
            .await;
            return None;
        }
    };

    tracing::info!(host_id = %host_id, hostname = %hostname, "agent registered");

    // 5. Create outbound channel and register connection
    let (tx, rx) = mpsc::channel::<ServerMessage>(OUTBOUND_CHANNEL_SIZE);

    let (old_sender, generation) = state.connections.register(host_id, hostname.clone(), tx).await;
    if let Some(old_sender) = old_sender {
        drop(old_sender);
        tracing::info!(host_id = %host_id, "replaced existing agent connection");
    }

    // 6. Send RegisterAck
    if send_server_message(socket, &ServerMessage::RegisterAck { host_id })
        .await
        .is_err()
    {
        tracing::error!(host_id = %host_id, "failed to send RegisterAck");
        state.connections.unregister(&host_id).await;
        return None;
    }

    Some(RegisteredAgent { host_id, generation, rx })
}

/// Main agent connection handler. Runs the full lifecycle:
/// register -> message loop -> cleanup.
async fn handle_agent_connection(mut socket: WebSocket, state: Arc<AppState>) {
    let Some(RegisteredAgent {
        host_id,
        generation,
        mut rx,
    }) = register_agent(&mut socket, &state).await
    else {
        return;
    };

    // Bidirectional message loop
    loop {
        tokio::select! {
            // Inbound from agent WebSocket
            msg = recv_agent_message(&mut socket) => {
                if let Some(agent_msg) = msg {
                    if let Err(e) = handle_agent_message(&state, host_id, agent_msg, &mut socket).await {
                        tracing::error!(host_id = %host_id, error = %e, "error handling agent message");
                        break;
                    }
                } else {
                    tracing::info!(host_id = %host_id, "agent disconnected");
                    break;
                }
            }
            // Outbound from server to agent
            server_msg = rx.recv() => {
                if let Some(msg) = server_msg {
                    if send_server_message(&mut socket, &msg).await.is_err() {
                        tracing::error!(host_id = %host_id, "failed to send message to agent");
                        break;
                    }
                } else {
                    // Channel closed, server initiated disconnect
                    tracing::info!(host_id = %host_id, "server closed agent channel");
                    break;
                }
            }
        }
    }

    // Cleanup on disconnect
    cleanup_agent(&state, &host_id, generation).await;
}

/// Receive and deserialize an agent message from the WebSocket.
async fn recv_agent_message(socket: &mut WebSocket) -> Option<AgentMessage> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<AgentMessage>(&text) {
                    Ok(msg) => return Some(msg),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to deserialize agent message");
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Binary(_))) => {
                tracing::warn!("received unexpected binary message from agent");
            }
            Some(Err(e)) => {
                tracing::warn!(error = %e, "WebSocket receive error");
                return None;
            }
        }
    }
}

/// Serialize and send a server message over the WebSocket.
async fn send_server_message(
    socket: &mut WebSocket,
    msg: &ServerMessage,
) -> Result<(), axum::Error> {
    let text = serde_json::to_string(msg).map_err(|e| {
        tracing::error!(error = %e, "failed to serialize server message");
        axum::Error::new(e)
    })?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to send WebSocket message");
            axum::Error::new(e)
        })
}

/// Handle a single agent message.
async fn handle_agent_message(
    state: &AppState,
    host_id: HostId,
    msg: AgentMessage,
    socket: &mut WebSocket,
) -> Result<(), String> {
    match msg {
        AgentMessage::Heartbeat { timestamp: _ } => {
            state.connections.update_heartbeat(&host_id).await;

            // Update last_seen_at in database
            let now = Utc::now().to_rfc3339();
            let host_id_str = host_id.to_string();
            if let Err(e) = sqlx::query("UPDATE hosts SET last_seen_at = ?, status = 'online' WHERE id = ?")
                .bind(&now)
                .bind(&host_id_str)
                .execute(&state.db)
                .await
            {
                tracing::warn!(host_id = %host_id, error = %e, "failed to update last_seen_at in database");
            }

            let ack = ServerMessage::HeartbeatAck {
                timestamp: Utc::now(),
            };
            send_server_message(socket, &ack)
                .await
                .map_err(|e| format!("failed to send HeartbeatAck: {e}"))?;
        }
        AgentMessage::TerminalOutput { session_id, .. } => {
            // Phase 2 stub: will relay to browser sessions
            tracing::debug!(host_id = %host_id, session_id = %session_id, "received terminal output (stub)");
        }
        AgentMessage::SessionCreated {
            session_id,
            shell,
            pid,
        } => {
            // Phase 2 stub: will update session in DB
            tracing::debug!(
                host_id = %host_id,
                session_id = %session_id,
                shell = %shell,
                pid = pid,
                "session created (stub)"
            );
        }
        AgentMessage::SessionClosed {
            session_id,
            exit_code,
        } => {
            // Phase 2 stub: will update session in DB
            tracing::debug!(
                host_id = %host_id,
                session_id = %session_id,
                exit_code = ?exit_code,
                "session closed (stub)"
            );
        }
        AgentMessage::Error {
            session_id,
            message,
        } => {
            tracing::warn!(
                host_id = %host_id,
                session_id = ?session_id,
                error_message = %message,
                "agent reported error"
            );
        }
        AgentMessage::Register { .. } => {
            tracing::warn!(host_id = %host_id, "agent sent duplicate Register message");
        }
    }
    Ok(())
}

/// Upsert a host record in the database. Look up by hostname only.
/// If found, update the existing record. Otherwise, create a new host.
async fn upsert_host(
    state: &AppState,
    hostname: &str,
    agent_version: &str,
    os: &str,
    arch: &str,
    token: &str,
) -> Result<HostId, String> {
    let token_hash = auth::hash_token(token);
    let now = Utc::now().to_rfc3339();

    // Look up existing host by hostname only
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT id FROM hosts WHERE hostname = ?")
            .bind(hostname)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| format!("database query failed: {e}"))?;

    let host_id = if let Some((id_str,)) = existing {
        let host_id: HostId = id_str
            .parse()
            .map_err(|e| format!("invalid host ID in database: {e}"))?;

        // Update existing host
        sqlx::query(
            "UPDATE hosts SET auth_token_hash = ?, agent_version = ?, os = ?, arch = ?, \
             status = 'online', last_seen_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(&token_hash)
        .bind(agent_version)
        .bind(os)
        .bind(arch)
        .bind(&now)
        .bind(&now)
        .bind(&id_str)
        .execute(&state.db)
        .await
        .map_err(|e| format!("failed to update host: {e}"))?;

        host_id
    } else {
        // Create new host
        let host_id = Uuid::new_v4();
        let id_str = host_id.to_string();

        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, agent_version, os, arch, \
             status, last_seen_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'online', ?, ?, ?)",
        )
        .bind(&id_str)
        .bind(hostname) // default name = hostname
        .bind(hostname)
        .bind(&token_hash)
        .bind(agent_version)
        .bind(os)
        .bind(arch)
        .bind(&now)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| format!("failed to insert host: {e}"))?;

        host_id
    };

    Ok(host_id)
}

/// Clean up after an agent disconnects: remove from connection manager
/// (only if generation matches) and mark as offline in the database.
async fn cleanup_agent(state: &AppState, host_id: &HostId, generation: u64) {
    state.connections.unregister_if_generation(host_id, generation).await;

    let now = Utc::now().to_rfc3339();
    let host_id_str = host_id.to_string();
    let result =
        sqlx::query("UPDATE hosts SET status = 'offline', updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(&host_id_str)
            .execute(&state.db)
            .await;

    if let Err(e) = result {
        tracing::error!(host_id = %host_id, error = %e, "failed to mark host offline in database");
    }

    tracing::info!(host_id = %host_id, "agent connection cleaned up");
}

/// Spawn a background task that periodically checks for stale agent connections
/// and marks them as offline. Stops when the cancellation token is cancelled.
pub fn spawn_heartbeat_monitor(state: Arc<AppState>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(HEARTBEAT_CHECK_INTERVAL);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let stale_hosts = state.connections.check_stale(HEARTBEAT_MAX_AGE).await;
                    for (host_id, generation) in stale_hosts {
                        tracing::warn!(host_id = %host_id, "agent heartbeat timeout, marking offline");
                        cleanup_agent(&state, &host_id, generation).await;
                    }
                }
                () = cancel.cancelled() => {
                    tracing::info!("heartbeat monitor shutting down");
                    break;
                }
            }
        }
    });
}
