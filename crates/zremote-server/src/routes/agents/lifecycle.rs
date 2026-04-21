//! Agent registration, cleanup on disconnect, session recovery.
//!
//! **Protocol dispatch (Phase 3):**
//! The WebSocket upgrade handler peeks at the first message to determine which
//! auth path to take:
//!
//! - `AgentAuthMessage::Hello { version: 2, … }` → ed25519 challenge-response
//!   via `auth::agent_auth::authenticate_agent`. V2 path (RFC §3 amendment).
//! - `AgentMessage::Register { token, … }` → legacy single-token check.
//!   Kept alive for one release cycle per RFC §9 backward-compat window.
//! - Anything else → reject with a malformed-message error.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use chrono::Utc;
use serde_json;
use tokio::sync::mpsc;
use uuid::Uuid;
use zremote_protocol::auth::{AgentAuthMessage, ServerAuthMessage};
use zremote_protocol::claude::ClaudeTaskStatus;
use zremote_protocol::status::SessionStatus;
use zremote_protocol::{AgentMessage, AgenticLoopId, HostId, ServerMessage};

use crate::auth;
use crate::auth::agent_auth::{self, AgentAuthError};
use crate::state::{AppState, ServerEvent};

use super::send_server_message;

/// Timeout for the first message (Register / Hello) after WebSocket upgrade.
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

/// Dispatch entry point: peek at the first frame to determine which auth path
/// to take. Returns a `RegisteredAgent` on success or `None` on failure.
///
/// V2 path (`AgentAuthMessage::Hello`): ed25519 challenge-response.
/// V1 path (`AgentMessage::Register`): legacy single-token check (one-release compat).
pub(super) async fn register_agent_dispatch(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    peer_ip: Option<&str>,
) -> Option<RegisteredAgent> {
    // Peek at the first frame to decide which path to take.
    let first_frame = match tokio::time::timeout(REGISTER_TIMEOUT, recv_first_frame(socket)).await {
        Ok(Some(text)) => text,
        Ok(None) => {
            tracing::warn!("agent disconnected before first message");
            return None;
        }
        Err(_) => {
            tracing::warn!("agent did not send first message within timeout");
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

    // Try deserializing as v2 AgentAuthMessage first.
    if let Ok(auth_msg) = serde_json::from_str::<AgentAuthMessage>(&first_frame)
        && matches!(auth_msg, AgentAuthMessage::Hello { .. })
    {
        // Feed back the message by creating a single-frame replay socket is
        // not straightforward. Instead, handle the Hello directly here.
        let started = Instant::now();
        return register_agent_v2(socket, state, auth_msg, peer_ip, started).await;
    }

    // Try deserializing as v1 AgentMessage (legacy Register).
    if let Ok(legacy_msg) = serde_json::from_str::<AgentMessage>(&first_frame)
        && matches!(legacy_msg, AgentMessage::Register { .. })
    {
        tracing::warn!(peer_ip = ?peer_ip, "legacy Register auth, upgrade agent");
        return register_agent_legacy(socket, state, legacy_msg).await;
    }

    tracing::warn!(peer_ip = ?peer_ip, "unrecognized first message from agent");
    let _ = send_server_message(
        socket,
        &ServerMessage::Error {
            message: "expected Hello or Register as first message".to_string(),
        },
    )
    .await;
    None
}

/// Receive the first text frame raw (before deserialization) so the dispatch
/// shim can try both v1 and v2 deserialization paths.
async fn recv_first_frame(socket: &mut WebSocket) -> Option<String> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => return Some(text.to_string()),
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Binary(_))) => {
                tracing::warn!("received unexpected binary message from agent");
                return None;
            }
            Some(Err(e)) => {
                tracing::warn!(error = %e, "WebSocket receive error");
                return None;
            }
        }
    }
}

/// V2 (ed25519) registration path. The `hello_msg` has already been parsed by
/// the dispatch shim.
async fn register_agent_v2(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    hello_msg: AgentAuthMessage,
    peer_ip: Option<&str>,
    started: Instant,
) -> Option<RegisteredAgent> {
    // Send the challenge and receive AuthResponse. We have the Hello already,
    // so we need to complete the handshake from step 2 onward.
    // Rather than duplicating the logic, re-use authenticate_agent_from_hello.
    let authenticated =
        match authenticate_agent_from_hello(socket, &state.db, hello_msg, peer_ip).await {
            Ok(auth) => auth,
            Err(err) => {
                agent_auth::reject_after(socket, &state.db, err, None, peer_ip, started).await;
                return None;
            }
        };

    tracing::info!(agent_id = %authenticated.agent_id, "agent authenticated (v2 ed25519)");

    let host_id: HostId = match authenticated.host_id.parse() {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "invalid host_id from agent auth");
            return None;
        }
    };

    // Fetch hostname from DB for connection registration.
    let hostname: String = match sqlx::query_as::<_, (String,)>(
        "SELECT hostname FROM hosts WHERE id = ?",
    )
    .bind(&authenticated.host_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some((h,))) => h,
        Ok(None) => {
            tracing::error!(host_id = %authenticated.host_id, "host not found after agent auth");
            return None;
        }
        Err(e) => {
            tracing::error!(error = %e, "DB error fetching hostname");
            return None;
        }
    };

    let (tx, rx) = mpsc::channel::<ServerMessage>(OUTBOUND_CHANNEL_SIZE);
    let (old_sender, generation) = state
        .connections
        .register(host_id, hostname.clone(), tx, true)
        .await;
    if let Some(old_sender) = old_sender {
        drop(old_sender);
        tracing::info!(host_id = %host_id, "replaced existing agent connection (v2)");
    }

    // Send AuthSuccess.
    let auth_success = ServerAuthMessage::AuthSuccess {
        session_id: authenticated.agent_id.clone(),
        reconnect_token: authenticated.session_token,
    };
    if let Ok(json) = serde_json::to_string(&auth_success)
        && socket.send(Message::Text(json.into())).await.is_err()
    {
        tracing::error!(host_id = %host_id, "failed to send AuthSuccess");
        state.connections.unregister(&host_id).await;
        return None;
    }

    Some(RegisteredAgent {
        host_id,
        generation,
        rx,
        hostname,
        agent_version: String::new(),
        os: String::new(),
        arch: String::new(),
        supports_persistent_sessions: true,
    })
}

/// Handle the ed25519 challenge-response starting from a Hello message that
/// was already parsed by the dispatch shim.
async fn authenticate_agent_from_hello(
    socket: &mut WebSocket,
    pool: &sqlx::SqlitePool,
    hello_msg: AgentAuthMessage,
    peer_ip: Option<&str>,
) -> Result<agent_auth::AuthenticatedAgent, AgentAuthError> {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::{Verifier, VerifyingKey};
    use rand::TryRngCore;
    use rand::rngs::OsRng;
    use zremote_core::queries::agents;
    use zremote_core::queries::audit::{self, AuditEvent, Outcome};
    use zremote_protocol::auth::{AGENT_PROTOCOL_VERSION, build_auth_payload};

    let AgentAuthMessage::Hello {
        version,
        agent_id,
        nonce_agent: nonce_agent_b64,
    } = hello_msg
    else {
        return Err(AgentAuthError::MalformedMessage);
    };

    if version != AGENT_PROTOCOL_VERSION {
        return Err(AgentAuthError::VersionMismatch);
    }

    let agent = agents::find_by_id(pool, &agent_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "DB error looking up agent");
            AgentAuthError::Internal
        })?
        .ok_or(AgentAuthError::UnknownAgent)?;

    if agent.revoked_at.is_some() {
        return Err(AgentAuthError::UnknownAgent);
    }

    let pk_bytes = URL_SAFE_NO_PAD
        .decode(&agent.public_key)
        .map_err(|_| AgentAuthError::InvalidPublicKey)?;
    let pk_arr: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| AgentAuthError::InvalidPublicKey)?;
    let verifying_key =
        VerifyingKey::from_bytes(&pk_arr).map_err(|_| AgentAuthError::InvalidPublicKey)?;

    let mut nonce_server = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut nonce_server)
        .expect("OS CSPRNG unavailable");
    let nonce_server_b64 = URL_SAFE_NO_PAD.encode(nonce_server);

    let challenge_json = serde_json::to_string(&ServerAuthMessage::Challenge {
        nonce_server: nonce_server_b64,
    })
    .map_err(|_| AgentAuthError::Internal)?;

    if socket
        .send(Message::Text(challenge_json.into()))
        .await
        .is_err()
    {
        return Err(AgentAuthError::Internal);
    }

    // Receive AuthResponse.
    let response = tokio::time::timeout(
        agent_auth::AUTH_FAIL_MIN_LATENCY * 100, // 10 s
        recv_auth_response(socket),
    )
    .await
    .map_err(|_| AgentAuthError::Timeout)?
    .ok_or(AgentAuthError::MalformedMessage)?;

    let AgentAuthMessage::AuthResponse {
        signature: signature_b64,
    } = response
    else {
        return Err(AgentAuthError::MalformedMessage);
    };

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(&signature_b64)
        .map_err(|_| AgentAuthError::MalformedMessage)?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| AgentAuthError::MalformedMessage)?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_arr);

    let nonce_agent_bytes = URL_SAFE_NO_PAD
        .decode(&nonce_agent_b64)
        .map_err(|_| AgentAuthError::MalformedMessage)?;
    let nonce_agent_32: [u8; 32] = nonce_agent_bytes
        .try_into()
        .map_err(|_| AgentAuthError::MalformedMessage)?;

    let payload = build_auth_payload(&agent_id, &nonce_server, &nonce_agent_32)
        .ok_or(AgentAuthError::MalformedMessage)?;

    verifying_key
        .verify(&payload, &signature)
        .map_err(|_| AgentAuthError::InvalidSignature)?;

    let session_token = agents::mint_agent_session(pool, &agent_id, 365 * 24 * 3600)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to mint agent_session");
            AgentAuthError::Internal
        })?;

    let _ = agents::set_last_seen(pool, &agent_id, Utc::now()).await;

    let _ = audit::log_event(
        pool,
        AuditEvent {
            ts: Utc::now(),
            actor: format!("agent:{agent_id}"),
            ip: peer_ip.map(str::to_string),
            event: "agent_auth_ok".to_string(),
            target: Some(agent_id.clone()),
            outcome: Outcome::Ok,
            details: None,
        },
    )
    .await;

    Ok(agent_auth::AuthenticatedAgent {
        agent_id,
        host_id: agent.host_id,
        session_token,
    })
}

async fn recv_auth_response(socket: &mut WebSocket) -> Option<AgentAuthMessage> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str::<AgentAuthMessage>(&text).ok();
            }
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Binary(_))) => return None,
            Some(Err(_)) => return None,
        }
    }
}

/// V1 (legacy) registration path. Kept for one release cycle per RFC §9.
async fn register_agent_legacy(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    msg: AgentMessage,
) -> Option<RegisteredAgent> {
    // Delegate to the existing register_agent function by re-using its logic.
    // The msg has already been parsed; wrap it in a pre-parsed call.
    register_agent_from_parsed(socket, state, msg).await
}

/// Process a pre-parsed AgentMessage::Register (legacy v1 path).
async fn register_agent_from_parsed(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    register_msg: AgentMessage,
) -> Option<RegisteredAgent> {
    let AgentMessage::Register {
        hostname,
        agent_version,
        os,
        arch,
        token,
        supports_persistent_sessions,
    } = register_msg
    else {
        tracing::warn!("expected Register message in legacy path");
        return None;
    };

    if !auth::verify_token(&token, &state.agent_token_hash) {
        tracing::warn!(hostname = %hostname, "legacy agent authentication failed");
        let _ = send_server_message(
            socket,
            &ServerMessage::Error {
                message: "invalid authentication token".to_string(),
            },
        )
        .await;
        return None;
    }

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

    tracing::info!(host_id = %host_id, hostname = %hostname, "agent registered (legacy v1)");

    let (tx, rx) = mpsc::channel::<ServerMessage>(OUTBOUND_CHANNEL_SIZE);
    let (old_sender, generation) = state
        .connections
        .register(host_id, hostname.clone(), tx, supports_persistent_sessions)
        .await;
    if let Some(old_sender) = old_sender {
        drop(old_sender);
        tracing::info!(host_id = %host_id, "replaced existing agent connection");
    }

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
