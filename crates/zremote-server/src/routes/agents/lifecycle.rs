//! Agent registration, cleanup on disconnect, session recovery.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use chrono::Utc;
use tokio::sync::mpsc;
use uuid::Uuid;
use zremote_protocol::claude::ClaudeTaskStatus;
use zremote_protocol::status::SessionStatus;
use zremote_protocol::{AgentMessage, AgenticLoopId, HostId, ServerMessage};

use crate::auth;
use crate::state::{AppState, ServerEvent};

use super::send_server_message;

/// Timeout for the first message (Register) after WebSocket upgrade.
pub(super) const REGISTER_TIMEOUT: Duration = Duration::from_secs(5);

/// Buffer size for the outbound message channel.
pub(super) const OUTBOUND_CHANNEL_SIZE: usize = 256;

/// Result of a successful agent registration handshake.
pub(super) struct RegisteredAgent {
    pub(super) host_id: HostId,
    pub(super) generation: u64,
    pub(super) rx: mpsc::Receiver<ServerMessage>,
    pub(super) hostname: String,
    pub(super) agent_version: String,
    pub(super) os: String,
    pub(super) arch: String,
    pub(super) supports_persistent_sessions: bool,
}

/// Receive a raw `AgentMessage` during registration (before the main loop).
async fn recv_terminal_message(socket: &mut WebSocket) -> Option<AgentMessage> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<AgentMessage>(&text) {
                Ok(msg) => return Some(msg),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to deserialize register message");
                }
            },
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

/// Perform the registration handshake: wait for Register message, validate
/// token, upsert host, register connection, and send `RegisterAck`.
/// Returns `None` if any step fails (errors are sent to the agent).
pub(super) async fn register_agent(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
) -> Option<RegisteredAgent> {
    // 1. Wait for Register message with timeout
    let register_msg =
        match tokio::time::timeout(REGISTER_TIMEOUT, recv_terminal_message(socket)).await {
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
        supports_persistent_sessions,
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

    let (old_sender, generation) = state
        .connections
        .register(host_id, hostname.clone(), tx, supports_persistent_sessions)
        .await;
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

    Some(RegisteredAgent {
        host_id,
        generation,
        rx,
        hostname,
        agent_version,
        os,
        arch,
        supports_persistent_sessions,
    })
}

/// Upsert a host record in the database. Look up by hostname only.
/// If found, update the existing record. Otherwise, create a new host.
// TODO(phase-3): Validate token matches stored hash before allowing upsert to prevent hostname hijack
pub(super) async fn upsert_host(
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
    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM hosts WHERE hostname = ?")
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

/// Clean up after an agent disconnects: close sessions, clean agentic loops,
/// remove from connection manager (only if generation matches), and mark as offline.
pub(super) async fn cleanup_agent(state: &AppState, host_id: &HostId, generation: u64) {
    // Read the persistent flag BEFORE unregistering (removal clears it from the map).
    let supports_persistent = state
        .connections
        .supports_persistent_sessions(host_id)
        .await;
    let removed = state
        .connections
        .unregister_if_generation(host_id, generation)
        .await;

    if !removed {
        // A newer connection has already replaced this one. Skip cleanup
        // to avoid race conditions where we re-suspend sessions that the
        // new connection has already recovered.
        tracing::debug!(
            host_id = %host_id,
            generation,
            "skipping stale cleanup (newer connection active)"
        );
        return;
    }

    let now = Utc::now().to_rfc3339();
    let host_id_str = host_id.to_string();

    if supports_persistent {
        // Suspend sessions (they may be recovered when the agent reconnects)
        let suspended_session_ids: Vec<_> = {
            let mut sessions = state.sessions.write().await;
            let session_ids: Vec<_> = sessions
                .iter()
                .filter(|(_, s)| {
                    s.host_id == *host_id
                        && s.status != SessionStatus::Closed
                        && s.status != SessionStatus::Suspended
                })
                .map(|(id, _)| *id)
                .collect();

            for &sid in &session_ids {
                if let Some(session) = sessions.get_mut(&sid) {
                    session.status = SessionStatus::Suspended;
                    // Notify connected browsers about suspension
                    let browser_msg = crate::state::BrowserMessage::SessionSuspended;
                    session.browser_senders.retain(|sender| {
                        match sender.try_send(browser_msg.clone()) {
                            Ok(()) => true,
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                        }
                    });
                }
            }

            session_ids
        };

        // Update DB: mark as suspended
        if let Err(e) = sqlx::query(
            "UPDATE sessions SET status = 'suspended', suspended_at = ? WHERE host_id = ? AND status NOT IN ('closed', 'suspended')",
        )
        .bind(&now)
        .bind(&host_id_str)
        .execute(&state.db)
        .await
        {
            tracing::error!(host_id = %host_id, error = %e, "failed to suspend sessions in database");
        }

        // Emit SessionSuspended events (not SessionClosed)
        for sid in &suspended_session_ids {
            let _ = state.events.send(ServerEvent::SessionSuspended {
                session_id: sid.to_string(),
            });
        }

        if !suspended_session_ids.is_empty() {
            tracing::info!(host_id = %host_id, count = suspended_session_ids.len(), "suspended sessions for disconnected agent (persistent)");
        }

        // Suspend claude tasks (may recover on reconnect)
        if let Err(e) = sqlx::query(
            "UPDATE claude_sessions SET status = 'suspended', disconnect_reason = 'agent_disconnected' \
             WHERE host_id = ? AND status IN ('starting', 'active')",
        )
        .bind(&host_id_str)
        .execute(&state.db)
        .await
        {
            tracing::error!(host_id = %host_id, error = %e, "failed to suspend claude sessions on disconnect");
        }

        // Emit ClaudeTaskEnded events for suspended tasks
        if let Ok(rows) = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            "SELECT id, project_path, task_name FROM claude_sessions WHERE host_id = ? AND status = 'suspended'",
        )
        .bind(&host_id_str)
        .fetch_all(&state.db)
        .await
        {
            for (task_id, project_path, task_name) in rows {
                let _ = state.events.send(ServerEvent::ClaudeTaskEnded {
                    task_id,
                    status: ClaudeTaskStatus::Suspended,
                    summary: Some("agent disconnected".to_string()),
                    session_id: None,
                    host_id: Some(host_id_str.clone()),
                    project_path,
                    task_name,
                });
            }
        }

        // Do NOT clean agentic loops or close sessions -- agent may reconnect
    } else {
        // Standard behavior: close all sessions
        let closed_session_ids: Vec<_> = {
            let mut sessions = state.sessions.write().await;
            let session_ids: Vec<_> = sessions
                .iter()
                .filter(|(_, s)| s.host_id == *host_id)
                .map(|(id, _)| *id)
                .collect();

            for &sid in &session_ids {
                if let Some(session) = sessions.remove(&sid) {
                    let browser_msg =
                        crate::state::BrowserMessage::SessionClosed { exit_code: None };
                    for sender in &session.browser_senders {
                        let _ = sender.try_send(browser_msg.clone());
                    }
                }
            }

            session_ids
        };

        // Batch update sessions in DB
        if let Err(e) = sqlx::query(
            "UPDATE sessions SET status = 'closed', closed_at = ? WHERE host_id = ? AND status != 'closed'",
        )
        .bind(&now)
        .bind(&host_id_str)
        .execute(&state.db)
        .await
        {
            tracing::error!(host_id = %host_id, error = %e, "failed to close sessions in database");
        }

        // Clean orphaned agentic loops from DashMap
        let closed_set: HashSet<_> = closed_session_ids.iter().copied().collect();
        let orphaned_loop_ids: Vec<AgenticLoopId> = state
            .agentic_loops
            .iter()
            .filter(|entry| closed_set.contains(&entry.value().session_id))
            .map(|entry| *entry.key())
            .collect();

        for loop_id in &orphaned_loop_ids {
            state.agentic_loops.remove(loop_id);
        }

        if let Err(e) = sqlx::query(
            "UPDATE agentic_loops SET status = 'completed', ended_at = ?, end_reason = 'agent_disconnected' \
             WHERE session_id IN (SELECT id FROM sessions WHERE host_id = ?) \
             AND status != 'completed' AND ended_at IS NULL",
        )
        .bind(&now)
        .bind(&host_id_str)
        .execute(&state.db)
        .await
        {
            tracing::error!(host_id = %host_id, error = %e, "failed to complete orphaned agentic loops in database");
        }

        // Emit SessionClosed events
        for sid in &closed_session_ids {
            let _ = state.events.send(ServerEvent::SessionClosed {
                session_id: sid.to_string(),
                exit_code: None,
            });
        }

        if !closed_session_ids.is_empty() {
            tracing::info!(host_id = %host_id, count = closed_session_ids.len(), "closed sessions for disconnected agent");
        }

        // Mark starting/active claude_sessions for this host as error
        if let Err(e) = sqlx::query(
            "UPDATE claude_sessions SET status = 'error', ended_at = ?, error_message = 'agent disconnected while task was running', \
             disconnect_reason = 'agent_disconnected' \
             WHERE host_id = ? AND status IN ('starting', 'active')",
        )
        .bind(&now)
        .bind(&host_id_str)
        .execute(&state.db)
        .await
        {
            tracing::error!(host_id = %host_id, error = %e, "failed to mark claude sessions as error on disconnect");
        }
    }

    // Mark host offline in DB
    let result = sqlx::query("UPDATE hosts SET status = 'offline', updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(&host_id_str)
        .execute(&state.db)
        .await;

    if let Err(e) = result {
        tracing::error!(host_id = %host_id, error = %e, "failed to mark host offline in database");
    }

    if removed {
        let _ = state.events.send(ServerEvent::HostDisconnected {
            host_id: host_id_str,
        });
    }

    tracing::info!(host_id = %host_id, "agent connection cleaned up");
}
