use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zremote_protocol::agentic::{AgenticAgentMessage, AgenticStatus};
use zremote_protocol::{AgentMessage, AgenticLoopId, HostId, ServerMessage};

use crate::auth;
use crate::state::{AgenticLoopState, AppState, HostInfo, LoopInfo, ServerEvent, SessionInfo};

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
    hostname: String,
    agent_version: String,
    os: String,
    arch: String,
    supports_persistent_sessions: bool,
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
async fn register_agent(socket: &mut WebSocket, state: &Arc<AppState>) -> Option<RegisteredAgent> {
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

/// Main agent connection handler. Runs the full lifecycle:
/// register -> message loop -> cleanup.
async fn handle_agent_connection(mut socket: WebSocket, state: Arc<AppState>) {
    let Some(RegisteredAgent {
        host_id,
        generation,
        mut rx,
        hostname,
        agent_version,
        os,
        arch,
        supports_persistent_sessions,
    }) = register_agent(&mut socket, &state).await
    else {
        return;
    };

    // Emit HostConnected event
    let _ = state.events.send(ServerEvent::HostConnected {
        host: HostInfo {
            id: host_id.to_string(),
            hostname,
            status: "online".to_string(),
            agent_version: Some(agent_version),
            os: Some(os),
            arch: Some(arch),
        },
    });

    if supports_persistent_sessions {
        tracing::info!(host_id = %host_id, "agent supports persistent sessions");
    }

    // Bidirectional message loop
    loop {
        tokio::select! {
            // Inbound from agent WebSocket
            msg = recv_agent_message(&mut socket) => {
                match msg {
                    Some(InboundMessage::Terminal(agent_msg)) => {
                        if let Err(e) = handle_agent_message(&state, host_id, agent_msg, &mut socket).await {
                            tracing::error!(host_id = %host_id, error = %e, "error handling agent message");
                            break;
                        }
                    }
                    Some(InboundMessage::Agentic(agentic_msg)) => {
                        if let Err(e) = handle_agentic_message(&state, host_id, agentic_msg).await {
                            tracing::error!(host_id = %host_id, error = %e, "error handling agentic message");
                        }
                    }
                    None => {
                        tracing::info!(host_id = %host_id, "agent disconnected");
                        break;
                    }
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

/// Enum representing either a terminal or agentic message from the agent.
enum InboundMessage {
    Terminal(AgentMessage),
    Agentic(AgenticAgentMessage),
}

/// Known `AgentMessage` type tags.
const TERMINAL_MSG_TYPES: &[&str] = &[
    "Register",
    "Heartbeat",
    "TerminalOutput",
    "SessionCreated",
    "SessionClosed",
    "Error",
    "ProjectDiscovered",
    "ProjectList",
    "KnowledgeAction",
    "ClaudeAction",
    "GitStatusUpdate",
    "WorktreeCreated",
    "WorktreeDeleted",
    "WorktreeError",
    "SessionsRecovered",
    "DirectoryListing",
    "ProjectSettingsResult",
    "ProjectSettingsSaved",
    "WorktreeHookResult",
    "ActionInputsResolved",
];

/// Known `AgenticAgentMessage` type tags.
const AGENTIC_MSG_TYPES: &[&str] = &["LoopDetected", "LoopStateUpdate", "LoopEnded"];

/// Receive and deserialize an agent message from the WebSocket.
/// Parses to `serde_json::Value` first, then dispatches based on the "type" tag.
async fn recv_agent_message(socket: &mut WebSocket) -> Option<InboundMessage> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                let value: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to parse agent message as JSON");
                        continue;
                    }
                };

                let msg_type = value
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();

                if TERMINAL_MSG_TYPES.contains(&msg_type.as_str()) {
                    match serde_json::from_value::<AgentMessage>(value) {
                        Ok(msg) => return Some(InboundMessage::Terminal(msg)),
                        Err(e) => {
                            tracing::warn!(msg_type = %msg_type, error = %e, "failed to deserialize terminal message");
                        }
                    }
                } else if AGENTIC_MSG_TYPES.contains(&msg_type.as_str()) {
                    match serde_json::from_value::<AgenticAgentMessage>(value) {
                        Ok(msg) => return Some(InboundMessage::Agentic(msg)),
                        Err(e) => {
                            tracing::warn!(msg_type = %msg_type, error = %e, "failed to deserialize agentic message");
                        }
                    }
                } else {
                    tracing::warn!(msg_type = %msg_type, "unknown agent message type");
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
    socket.send(Message::Text(text.into())).await.map_err(|e| {
        tracing::error!(error = %e, "failed to send WebSocket message");
        axum::Error::new(e)
    })
}

/// Handle a single agent message.
#[allow(clippy::too_many_lines)]
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
            if let Err(e) =
                sqlx::query("UPDATE hosts SET last_seen_at = ?, status = 'online' WHERE id = ?")
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
        AgentMessage::TerminalOutput { session_id, data } => {
            let mut sessions = state.sessions.write().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                let browser_msg = crate::state::BrowserMessage::Output {
                    pane_id: None,
                    data: data.clone(),
                };
                session.append_scrollback(data);
                // Forward to all browser senders, remove dead ones
                session.browser_senders.retain(|sender| {
                    match sender.try_send(browser_msg.clone()) {
                        Ok(()) => true,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                    }
                });
            }
        }
        AgentMessage::SessionCreated {
            session_id,
            shell,
            pid,
            tmux_name,
        } => {
            // Update DB
            let session_id_str = session_id.to_string();
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(e) = sqlx::query(
                "UPDATE sessions SET status = 'active', shell = ?, pid = ?, tmux_name = ?, created_at = ? WHERE id = ?",
            )
            .bind(&shell)
            .bind(i64::from(pid))
            .bind(&tmux_name)
            .bind(&now)
            .bind(&session_id_str)
            .execute(&state.db)
            .await
            {
                tracing::error!(session_id = %session_id, error = %e, "failed to update session in DB");
            }

            // Update in-memory state
            let mut sessions = state.sessions.write().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                session.status = "active".to_string();
            }

            // Emit SessionCreated event
            let _ = state.events.send(ServerEvent::SessionCreated {
                session: SessionInfo {
                    id: session_id.to_string(),
                    host_id: host_id.to_string(),
                    shell: Some(shell.clone()),
                    status: "active".to_string(),
                },
            });

            tracing::info!(
                host_id = %host_id,
                session_id = %session_id,
                shell = %shell,
                pid = pid,
                "session created"
            );
        }
        AgentMessage::SessionClosed {
            session_id,
            exit_code,
        } => {
            // Update DB
            let session_id_str = session_id.to_string();
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(e) = sqlx::query(
                "UPDATE sessions SET status = 'closed', exit_code = ?, closed_at = ? WHERE id = ?",
            )
            .bind(exit_code)
            .bind(&now)
            .bind(&session_id_str)
            .execute(&state.db)
            .await
            {
                tracing::error!(session_id = %session_id, error = %e, "failed to update session closed in DB");
            }

            // Notify browser senders and remove from store
            let mut sessions = state.sessions.write().await;
            if let Some(session) = sessions.remove(&session_id) {
                let browser_msg = crate::state::BrowserMessage::SessionClosed { exit_code };
                for sender in &session.browser_senders {
                    let _ = sender.try_send(browser_msg.clone());
                }
            }

            // Check if session has a linked claude_session in starting/active state
            {
                let now_ct = chrono::Utc::now().to_rfc3339();
                // starting -> error (session closed before Claude started)
                if let Ok(result) = sqlx::query(
                    "UPDATE claude_sessions SET status = 'error', ended_at = ? \
                     WHERE session_id = ? AND status = 'starting'",
                )
                .bind(&now_ct)
                .bind(&session_id_str)
                .execute(&state.db)
                .await
                    && result.rows_affected() > 0
                    && let Ok(Some((task_id,))) = sqlx::query_as::<_, (String,)>(
                        "SELECT id FROM claude_sessions WHERE session_id = ? AND status = 'error'",
                    )
                    .bind(&session_id_str)
                    .fetch_optional(&state.db)
                    .await
                {
                    let _ = state.events.send(ServerEvent::ClaudeTaskEnded {
                        task_id,
                        status: "error".to_string(),
                        summary: Some("session closed before Claude started".to_string()),
                    });
                }
                // active -> completed (normal exit)
                if let Ok(result) = sqlx::query(
                    "UPDATE claude_sessions SET status = 'completed', ended_at = ? \
                     WHERE session_id = ? AND status = 'active'",
                )
                .bind(&now_ct)
                .bind(&session_id_str)
                .execute(&state.db)
                .await
                    && result.rows_affected() > 0
                    && let Ok(Some((task_id,))) = sqlx::query_as::<_, (String,)>(
                        "SELECT id FROM claude_sessions WHERE session_id = ?",
                    )
                    .bind(&session_id_str)
                    .fetch_optional(&state.db)
                    .await
                {
                    let _ = state.events.send(ServerEvent::ClaudeTaskEnded {
                        task_id,
                        status: "completed".to_string(),
                        summary: None,
                    });
                }
            }

            // Emit SessionClosed event
            let _ = state.events.send(ServerEvent::SessionClosed {
                session_id: session_id.to_string(),
                exit_code,
            });

            tracing::info!(
                host_id = %host_id,
                session_id = %session_id,
                exit_code = ?exit_code,
                "session closed"
            );
        }
        AgentMessage::SessionsRecovered { sessions } => {
            tracing::info!(
                host_id = %host_id,
                count = sessions.len(),
                "agent reported recovered sessions"
            );

            let now = chrono::Utc::now().to_rfc3339();

            // Get all non-closed sessions for this host from BOTH in-memory
            // store and DB. This covers suspended sessions (normal reconnect) AND
            // active sessions (e.g. daemon sessions that weren't suspended due to
            // server restart or race conditions).
            let host_session_ids: Vec<uuid::Uuid> = {
                let mut ids: HashSet<uuid::Uuid> = {
                    let sessions_store = state.sessions.read().await;
                    sessions_store
                        .iter()
                        .filter(|(_, s)| s.host_id == host_id && s.status != "closed")
                        .map(|(id, _)| *id)
                        .collect()
                };
                // Also check DB for sessions not yet in memory (server restart case)
                if let Ok(db_rows) = sqlx::query_scalar::<_, String>(
                    "SELECT id FROM sessions WHERE host_id = ? AND status != 'closed'",
                )
                .bind(host_id.to_string())
                .fetch_all(&state.db)
                .await
                {
                    for row in db_rows {
                        if let Ok(id) = row.parse::<uuid::Uuid>() {
                            ids.insert(id);
                        }
                    }
                }
                ids.into_iter().collect()
            };

            let recovered_ids: HashSet<uuid::Uuid> =
                sessions.iter().map(|s| s.session_id).collect();

            // Resume recovered sessions
            for recovered in &sessions {
                // Update DB: suspended -> active
                if let Err(e) = sqlx::query(
                    "UPDATE sessions SET status = 'active', suspended_at = NULL, pid = ?, shell = ? WHERE id = ?",
                )
                .bind(i64::from(recovered.pid))
                .bind(&recovered.shell)
                .bind(recovered.session_id.to_string())
                .execute(&state.db)
                .await
                {
                    tracing::error!(session_id = %recovered.session_id, error = %e, "failed to resume session in DB");
                    continue;
                }

                // Update in-memory state
                let mut sessions_store = state.sessions.write().await;
                if let Some(session) = sessions_store.get_mut(&recovered.session_id) {
                    session.status = "active".to_string();
                    // Notify connected browsers
                    let resume_msg = crate::state::BrowserMessage::SessionResumed;
                    session.browser_senders.retain(|sender| {
                        match sender.try_send(resume_msg.clone()) {
                            Ok(()) => true,
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                        }
                    });
                } else {
                    // Session was not in memory (e.g., server restarted too). Create it.
                    sessions_store.insert(
                        recovered.session_id,
                        crate::state::SessionState::new(recovered.session_id, host_id),
                    );
                    if let Some(session) = sessions_store.get_mut(&recovered.session_id) {
                        session.status = "active".to_string();
                    }
                }

                // Emit SessionResumed event
                let _ = state
                    .events
                    .send(crate::state::ServerEvent::SessionResumed {
                        session_id: recovered.session_id.to_string(),
                    });

                tracing::info!(
                    session_id = %recovered.session_id,
                    pid = recovered.pid,
                    "session resumed after agent reconnection"
                );
            }

            // Close sessions that were NOT recovered by the agent
            for sid in &host_session_ids {
                if !recovered_ids.contains(sid) {
                    let sid_str = sid.to_string();

                    // Update DB
                    if let Err(e) = sqlx::query(
                        "UPDATE sessions SET status = 'closed', closed_at = ? WHERE id = ?",
                    )
                    .bind(&now)
                    .bind(&sid_str)
                    .execute(&state.db)
                    .await
                    {
                        tracing::error!(session_id = %sid, error = %e, "failed to close unrecovered session in DB");
                    }

                    // Remove from memory + notify browsers
                    let mut sessions_store = state.sessions.write().await;
                    if let Some(session) = sessions_store.remove(sid) {
                        let close_msg =
                            crate::state::BrowserMessage::SessionClosed { exit_code: None };
                        for sender in &session.browser_senders {
                            let _ = sender.try_send(close_msg.clone());
                        }
                    }

                    // Emit SessionClosed event
                    let _ = state.events.send(crate::state::ServerEvent::SessionClosed {
                        session_id: sid_str,
                        exit_code: None,
                    });

                    tracing::info!(session_id = %sid, "closed unrecovered session");
                }
            }

            return Ok(());
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

            if let Some(session_id) = session_id {
                // Update session status to error in DB
                let now = Utc::now().to_rfc3339();
                if let Err(e) = sqlx::query(
                    "UPDATE sessions SET status = 'error', closed_at = ? WHERE id = ? AND status != 'closed'",
                )
                .bind(&now)
                .bind(session_id.to_string())
                .execute(&state.db)
                .await
                {
                    tracing::error!(session_id = %session_id, error = %e, "failed to update session error status in DB");
                }

                // Notify browser senders and remove from store
                let mut sessions = state.sessions.write().await;
                if let Some(session) = sessions.remove(&session_id) {
                    let browser_msg =
                        crate::state::BrowserMessage::SessionClosed { exit_code: None };
                    for sender in &session.browser_senders {
                        let _ = sender.try_send(browser_msg.clone());
                    }
                }
                drop(sessions);

                // Emit SessionClosed event
                let _ = state.events.send(ServerEvent::SessionClosed {
                    session_id: session_id.to_string(),
                    exit_code: None,
                });
            }
        }
        AgentMessage::Register { .. } => {
            tracing::warn!(host_id = %host_id, "agent sent duplicate Register message");
        }
        AgentMessage::ProjectDiscovered {
            path,
            name,
            has_claude_config,
            has_zremote_config,
            project_type,
        } => {
            let host_id_str = host_id.to_string();
            let project_id = Uuid::new_v4().to_string();
            if let Err(e) = sqlx::query(
                "INSERT INTO projects (id, host_id, path, name, has_claude_config, has_zremote_config, project_type) \
                 VALUES (?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(host_id, path) DO UPDATE SET \
                 name = excluded.name, has_claude_config = excluded.has_claude_config, \
                 has_zremote_config = excluded.has_zremote_config, \
                 project_type = excluded.project_type",
            )
            .bind(&project_id)
            .bind(&host_id_str)
            .bind(&path)
            .bind(&name)
            .bind(has_claude_config)
            .bind(has_zremote_config)
            .bind(&project_type)
            .execute(&state.db)
            .await
            {
                tracing::warn!(host_id = %host_id, path = %path, error = %e, "failed to upsert project");
            } else {
                tracing::info!(host_id = %host_id, path = %path, name = %name, "project discovered");
                let _ = state.events.send(ServerEvent::ProjectsUpdated {
                    host_id: host_id.to_string(),
                });
            }
        }
        AgentMessage::GitStatusUpdate {
            path,
            git_info,
            worktrees,
        } => {
            let host_id_str = host_id.to_string();
            let now = chrono::Utc::now().to_rfc3339();
            let remotes_json = serde_json::to_string(&git_info.remotes).ok();
            if let Err(e) = sqlx::query(
                "UPDATE projects SET \
                 git_branch = ?, git_commit_hash = ?, git_commit_message = ?, \
                 git_is_dirty = ?, git_ahead = ?, git_behind = ?, \
                 git_remotes = ?, git_updated_at = ? \
                 WHERE host_id = ? AND path = ?",
            )
            .bind(&git_info.branch)
            .bind(&git_info.commit_hash)
            .bind(&git_info.commit_message)
            .bind(git_info.is_dirty)
            .bind(git_info.ahead)
            .bind(git_info.behind)
            .bind(&remotes_json)
            .bind(&now)
            .bind(&host_id_str)
            .bind(&path)
            .execute(&state.db)
            .await
            {
                tracing::warn!(host_id = %host_id, path = %path, error = %e, "failed to update git status");
            }

            // Upsert/delete worktree children
            upsert_worktree_children(state, &host_id_str, &path, &worktrees).await;

            let _ = state.events.send(ServerEvent::ProjectsUpdated {
                host_id: host_id.to_string(),
            });
        }
        AgentMessage::WorktreeCreated {
            project_path,
            worktree,
            hook_result: _,
        } => {
            let host_id_str = host_id.to_string();

            // Find parent project id
            let parent: Option<(String,)> =
                sqlx::query_as("SELECT id FROM projects WHERE host_id = ? AND path = ?")
                    .bind(&host_id_str)
                    .bind(&project_path)
                    .fetch_optional(&state.db)
                    .await
                    .ok()
                    .flatten();

            if let Some((parent_id,)) = parent {
                let wt_id = Uuid::new_v4().to_string();
                let wt_name = std::path::Path::new(&worktree.path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("worktree")
                    .to_string();

                if let Err(e) = sqlx::query(
                    "INSERT INTO projects (id, host_id, path, name, project_type, parent_project_id, \
                     git_branch, git_commit_hash) \
                     VALUES (?, ?, ?, ?, 'worktree', ?, ?, ?) \
                     ON CONFLICT(host_id, path) DO UPDATE SET \
                     git_branch = excluded.git_branch, git_commit_hash = excluded.git_commit_hash",
                )
                .bind(&wt_id)
                .bind(&host_id_str)
                .bind(&worktree.path)
                .bind(&wt_name)
                .bind(&parent_id)
                .bind(&worktree.branch)
                .bind(&worktree.commit_hash)
                .execute(&state.db)
                .await
                {
                    tracing::warn!(host_id = %host_id, path = %worktree.path, error = %e, "failed to insert worktree child");
                }
            }

            let _ = state.events.send(ServerEvent::ProjectsUpdated {
                host_id: host_id.to_string(),
            });
        }
        AgentMessage::WorktreeDeleted {
            project_path: _,
            worktree_path,
        } => {
            let host_id_str = host_id.to_string();
            if let Err(e) = sqlx::query(
                "DELETE FROM projects WHERE host_id = ? AND path = ? AND parent_project_id IS NOT NULL",
            )
            .bind(&host_id_str)
            .bind(&worktree_path)
            .execute(&state.db)
            .await
            {
                tracing::warn!(host_id = %host_id, path = %worktree_path, error = %e, "failed to delete worktree child");
            }

            let _ = state.events.send(ServerEvent::ProjectsUpdated {
                host_id: host_id.to_string(),
            });
        }
        AgentMessage::WorktreeError {
            project_path,
            message,
        } => {
            tracing::warn!(host_id = %host_id, path = %project_path, error = %message, "worktree operation error");
            let _ = state.events.send(ServerEvent::WorktreeError {
                host_id: host_id.to_string(),
                project_path,
                message,
            });
        }
        AgentMessage::WorktreeHookResult {
            project_path,
            worktree_path,
            hook_type,
            success,
            ..
        } => {
            tracing::info!(
                host_id = %host_id,
                path = %project_path,
                worktree = %worktree_path,
                hook = %hook_type,
                success = %success,
                "worktree hook result"
            );
        }
        AgentMessage::KnowledgeAction(knowledge_msg) => {
            if let Err(e) = handle_knowledge_message(state, host_id, knowledge_msg).await {
                tracing::error!(host_id = %host_id, error = %e, "error handling knowledge message");
            }
        }
        AgentMessage::ProjectList { projects } => {
            let host_id_str = host_id.to_string();
            tracing::info!(host_id = %host_id, count = projects.len(), "received project list");
            let now = chrono::Utc::now().to_rfc3339();
            for project in &projects {
                let project_id = Uuid::new_v4().to_string();
                let remotes_json = project
                    .git_info
                    .as_ref()
                    .map(|gi| serde_json::to_string(&gi.remotes).unwrap_or_default());
                let git_updated = project.git_info.as_ref().map(|_| now.clone());
                if let Err(e) = sqlx::query(
                    "INSERT INTO projects (id, host_id, path, name, has_claude_config, has_zremote_config, project_type, \
                     git_branch, git_commit_hash, git_commit_message, git_is_dirty, \
                     git_ahead, git_behind, git_remotes, git_updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(host_id, path) DO UPDATE SET \
                     name = excluded.name, has_claude_config = excluded.has_claude_config, \
                     has_zremote_config = excluded.has_zremote_config, \
                     project_type = excluded.project_type, \
                     git_branch = excluded.git_branch, git_commit_hash = excluded.git_commit_hash, \
                     git_commit_message = excluded.git_commit_message, git_is_dirty = excluded.git_is_dirty, \
                     git_ahead = excluded.git_ahead, git_behind = excluded.git_behind, \
                     git_remotes = excluded.git_remotes, git_updated_at = excluded.git_updated_at",
                )
                .bind(&project_id)
                .bind(&host_id_str)
                .bind(&project.path)
                .bind(&project.name)
                .bind(project.has_claude_config)
                .bind(project.has_zremote_config)
                .bind(&project.project_type)
                .bind(project.git_info.as_ref().and_then(|gi| gi.branch.as_deref()))
                .bind(project.git_info.as_ref().and_then(|gi| gi.commit_hash.as_deref()))
                .bind(project.git_info.as_ref().and_then(|gi| gi.commit_message.as_deref()))
                .bind(project.git_info.as_ref().is_some_and(|gi| gi.is_dirty))
                .bind(project.git_info.as_ref().map_or(0, |gi| gi.ahead))
                .bind(project.git_info.as_ref().map_or(0, |gi| gi.behind))
                .bind(&remotes_json)
                .bind(&git_updated)
                .execute(&state.db)
                .await
                {
                    tracing::warn!(host_id = %host_id, path = %project.path, error = %e, "failed to upsert project");
                }

                // Upsert worktree children
                if !project.worktrees.is_empty() {
                    upsert_worktree_children(
                        state,
                        &host_id_str,
                        &project.path,
                        &project.worktrees,
                    )
                    .await;
                }
            }
            let _ = state.events.send(ServerEvent::ProjectsUpdated {
                host_id: host_id.to_string(),
            });
        }
        AgentMessage::DirectoryListing {
            request_id,
            entries,
            error,
            ..
        } => {
            if let Some((_, sender)) = state.directory_requests.remove(&request_id) {
                let _ = sender.send(crate::state::DirectoryListingResponse { entries, error });
            } else {
                tracing::warn!(request_id = %request_id, "no pending request for directory listing");
            }
        }
        AgentMessage::ProjectSettingsResult {
            request_id,
            settings,
            error,
        } => {
            if let Some((_, sender)) = state.settings_get_requests.remove(&request_id) {
                let _ = sender.send(crate::state::SettingsGetResponse { settings, error });
            } else {
                tracing::warn!(request_id = %request_id, "no pending request for settings get");
            }
        }
        AgentMessage::ProjectSettingsSaved { request_id, error } => {
            if let Some((_, sender)) = state.settings_save_requests.remove(&request_id) {
                let _ = sender.send(crate::state::SettingsSaveResponse { error });
            } else {
                tracing::warn!(request_id = %request_id, "no pending request for settings save");
            }
        }
        AgentMessage::ClaudeAction(claude_msg) => {
            if let Err(e) = handle_claude_message(state, host_id, claude_msg).await {
                tracing::error!(host_id = %host_id, error = %e, "error handling claude message");
            }
        }
        AgentMessage::ActionInputsResolved {
            request_id,
            inputs,
            error,
        } => {
            if let Some((_, sender)) = state.action_inputs_requests.remove(&request_id) {
                let _ = sender.send(crate::state::ActionInputsResolveResponse { inputs, error });
            } else {
                tracing::warn!(
                    request_id = %request_id,
                    "received ActionInputsResolved for unknown request"
                );
            }
        }
    }
    Ok(())
}

/// Upsert worktree children for a project and clean up stale ones.
async fn upsert_worktree_children(
    state: &AppState,
    host_id_str: &str,
    project_path: &str,
    worktrees: &[zremote_protocol::project::WorktreeInfo],
) {
    // Find parent project id
    let parent: Option<(String,)> =
        sqlx::query_as("SELECT id FROM projects WHERE host_id = ? AND path = ?")
            .bind(host_id_str)
            .bind(project_path)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

    let Some((parent_id,)) = parent else {
        return;
    };

    // Upsert each worktree as a child project
    for wt in worktrees {
        let wt_id = Uuid::new_v4().to_string();
        let wt_name = std::path::Path::new(&wt.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("worktree")
            .to_string();

        if let Err(e) = sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, project_type, parent_project_id, \
             git_branch, git_commit_hash) \
             VALUES (?, ?, ?, ?, 'worktree', ?, ?, ?) \
             ON CONFLICT(host_id, path) DO UPDATE SET \
             git_branch = excluded.git_branch, git_commit_hash = excluded.git_commit_hash, \
             parent_project_id = excluded.parent_project_id",
        )
        .bind(&wt_id)
        .bind(host_id_str)
        .bind(&wt.path)
        .bind(&wt_name)
        .bind(&parent_id)
        .bind(&wt.branch)
        .bind(&wt.commit_hash)
        .execute(&state.db)
        .await
        {
            tracing::warn!(path = %wt.path, error = %e, "failed to upsert worktree child");
        }
    }

    // Clean up stale worktree children whose paths are no longer in the list
    let current_paths: Vec<&str> = worktrees.iter().map(|wt| wt.path.as_str()).collect();
    if current_paths.is_empty() {
        // Delete all worktree children for this parent
        if let Err(e) = sqlx::query("DELETE FROM projects WHERE parent_project_id = ?")
            .bind(&parent_id)
            .execute(&state.db)
            .await
        {
            tracing::warn!(parent_id = %parent_id, error = %e, "failed to clean up worktree children");
        }
    } else {
        // Fetch existing child paths and delete those not in current list
        let existing: Vec<(String, String)> =
            sqlx::query_as("SELECT id, path FROM projects WHERE parent_project_id = ?")
                .bind(&parent_id)
                .fetch_all(&state.db)
                .await
                .unwrap_or_default();

        for (child_id, child_path) in &existing {
            if !current_paths.contains(&child_path.as_str())
                && let Err(e) = sqlx::query("DELETE FROM projects WHERE id = ?")
                    .bind(child_id)
                    .execute(&state.db)
                    .await
            {
                tracing::warn!(child_id = %child_id, error = %e, "failed to delete stale worktree child");
            }
        }
    }
}

/// DB row for an agentic loop, matching the `agentic_loops` table columns.
#[derive(sqlx::FromRow)]
struct LoopRow {
    id: String,
    session_id: String,
    project_path: Option<String>,
    tool_name: String,
    status: String,
    started_at: String,
    ended_at: Option<String>,
    end_reason: Option<String>,
    task_name: Option<String>,
}

/// Fetch a `LoopInfo` from the DB.
async fn fetch_loop_info(state: &AppState, loop_id: &str) -> Option<LoopInfo> {
    let row: LoopRow = sqlx::query_as(
        "SELECT id, session_id, project_path, tool_name, status, started_at, \
         ended_at, end_reason, task_name \
         FROM agentic_loops WHERE id = ?",
    )
    .bind(loop_id)
    .fetch_optional(&state.db)
    .await
    .ok()??;

    Some(LoopInfo {
        id: row.id,
        session_id: row.session_id,
        project_path: row.project_path,
        tool_name: row.tool_name,
        status: row.status,
        started_at: row.started_at,
        ended_at: row.ended_at,
        end_reason: row.end_reason,
        task_name: row.task_name,
    })
}

/// Handle an agentic agent message: update DB and in-memory state.
#[allow(clippy::too_many_lines)]
async fn handle_agentic_message(
    state: &AppState,
    host_id: HostId,
    msg: AgenticAgentMessage,
) -> Result<(), String> {
    match msg {
        AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path,
            tool_name,
        } => {
            let loop_id_str = loop_id.to_string();
            let session_id_str = session_id.to_string();

            if let Err(e) = sqlx::query(
                "INSERT INTO agentic_loops (id, session_id, project_path, tool_name) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&loop_id_str)
            .bind(&session_id_str)
            .bind(&project_path)
            .bind(&tool_name)
            .execute(&state.db)
            .await
            {
                tracing::error!(loop_id = %loop_id, error = %e, "failed to insert agentic loop");
                return Err(format!("failed to insert agentic loop: {e}"));
            }

            state.agentic_loops.insert(
                loop_id,
                AgenticLoopState {
                    loop_id,
                    session_id,
                    host_id,
                    status: AgenticStatus::Working,
                    task_name: None,
                    last_updated: Instant::now(),
                },
            );

            tracing::info!(host_id = %host_id, loop_id = %loop_id, tool_name = %tool_name, "agentic loop detected");

            // Link loop to claude_session if one exists, or auto-create one for manually-started sessions
            let link_result = sqlx::query(
                "UPDATE claude_sessions SET loop_id = ?, status = 'active' WHERE session_id = ? AND status = 'starting'",
            )
            .bind(&loop_id_str)
            .bind(&session_id_str)
            .execute(&state.db)
            .await;

            let linked_task_id = match link_result {
                Ok(result) if result.rows_affected() > 0 => {
                    // Successfully linked to an existing dialog-started task
                    let row: Option<(String,)> =
                        sqlx::query_as("SELECT id FROM claude_sessions WHERE loop_id = ?")
                            .bind(&loop_id_str)
                            .fetch_optional(&state.db)
                            .await
                            .ok()
                            .flatten();
                    row.map(|(id,)| id)
                }
                _ => {
                    // No dialog-started task exists -- auto-create one for this manually-started Claude session
                    let auto_task_id = Uuid::new_v4().to_string();
                    let host_id_str = host_id.to_string();

                    // Resolve project_id from project_path
                    let project_id: Option<String> = sqlx::query_scalar(
                        "SELECT id FROM projects WHERE host_id = ? AND path = ? LIMIT 1",
                    )
                    .bind(&host_id_str)
                    .bind(&project_path)
                    .fetch_optional(&state.db)
                    .await
                    .ok()
                    .flatten();

                    if let Err(e) = sqlx::query(
                        "INSERT INTO claude_sessions (id, session_id, host_id, project_path, project_id, status, loop_id) \
                         VALUES (?, ?, ?, ?, ?, 'active', ?) \
                         ON CONFLICT(session_id) DO UPDATE SET loop_id = excluded.loop_id, status = 'active'",
                    )
                    .bind(&auto_task_id)
                    .bind(&session_id_str)
                    .bind(&host_id_str)
                    .bind(&project_path)
                    .bind(&project_id)
                    .bind(&loop_id_str)
                    .execute(&state.db)
                    .await
                    {
                        tracing::warn!(loop_id = %loop_id, error = %e, "failed to auto-create claude session for detected loop");
                        None
                    } else {
                        tracing::info!(loop_id = %loop_id, task_id = %auto_task_id, "auto-created claude task for manually-started session");
                        Some(auto_task_id)
                    }
                }
            };

            // Emit task event if we have a linked/created task
            if let Some(ref task_id) = linked_task_id {
                let _ = state.events.send(ServerEvent::ClaudeTaskStarted {
                    task_id: task_id.clone(),
                    session_id: session_id_str.clone(),
                    host_id: host_id.to_string(),
                    project_path: project_path.clone(),
                });
                let _ = state.events.send(ServerEvent::ClaudeTaskUpdated {
                    task_id: task_id.clone(),
                    status: "active".to_string(),
                    loop_id: Some(loop_id_str.clone()),
                });
            }

            // Broadcast event to browser clients
            let hostname = state
                .connections
                .get_hostname(&host_id)
                .await
                .unwrap_or_default();
            if let Some(loop_info) = fetch_loop_info(state, &loop_id_str).await {
                let _ = state.events.send(ServerEvent::LoopDetected {
                    loop_info,
                    host_id: host_id.to_string(),
                    hostname,
                });
            }
        }
        AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status,
            task_name,
        } => {
            // Update in-memory state
            if let Some(mut entry) = state.agentic_loops.get_mut(&loop_id) {
                entry.status = status;
                if task_name.is_some() {
                    entry.task_name = task_name.clone();
                }
                entry.last_updated = Instant::now();
            }

            // Update DB
            let loop_id_str = loop_id.to_string();
            let status_str = serde_json::to_value(status)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{status:?}").to_lowercase());

            if let Err(e) = sqlx::query(
                "UPDATE agentic_loops SET status = ?, task_name = COALESCE(?, task_name) WHERE id = ?",
            )
            .bind(&status_str)
            .bind(task_name.as_deref())
            .bind(&loop_id_str)
            .execute(&state.db)
            .await
            {
                tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop status in DB");
            }

            // Update task_name on linked claude_session if provided
            if task_name.is_some() {
                let _ = sqlx::query(
                    "UPDATE claude_sessions SET task_name = COALESCE(?, task_name) WHERE loop_id = ?",
                )
                .bind(task_name.as_deref())
                .bind(&loop_id_str)
                .execute(&state.db)
                .await;
            }

            // Broadcast event with full loop info
            let hostname = state
                .connections
                .get_hostname(&host_id)
                .await
                .unwrap_or_default();
            if let Some(loop_info) = fetch_loop_info(state, &loop_id_str).await {
                let _ = state.events.send(ServerEvent::LoopStatusChanged {
                    loop_info,
                    host_id: host_id.to_string(),
                    hostname,
                });
            }
        }
        AgenticAgentMessage::LoopEnded { loop_id, reason } => {
            let loop_id_str = loop_id.to_string();
            let now = chrono::Utc::now().to_rfc3339();

            if let Err(e) = sqlx::query(
                "UPDATE agentic_loops SET status = 'completed', ended_at = ?, \
                 end_reason = ? WHERE id = ?",
            )
            .bind(&now)
            .bind(&reason)
            .bind(&loop_id_str)
            .execute(&state.db)
            .await
            {
                tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop ended in DB");
            }

            // Update linked claude_session if any
            if let Ok(Some((task_id,))) =
                sqlx::query_as::<_, (String,)>("SELECT id FROM claude_sessions WHERE loop_id = ?")
                    .bind(&loop_id_str)
                    .fetch_optional(&state.db)
                    .await
            {
                let now_str = chrono::Utc::now().to_rfc3339();
                let _ = sqlx::query(
                    "UPDATE claude_sessions SET status = 'completed', ended_at = ? WHERE id = ?",
                )
                .bind(&now_str)
                .bind(&task_id)
                .execute(&state.db)
                .await;

                let _ = state.events.send(ServerEvent::ClaudeTaskEnded {
                    task_id,
                    status: "completed".to_string(),
                    summary: None,
                });
            }

            // Fetch full loop info before removing from in-memory state
            let loop_info = fetch_loop_info(state, &loop_id_str).await;

            // Remove from in-memory state
            state.agentic_loops.remove(&loop_id);

            let hostname = state
                .connections
                .get_hostname(&host_id)
                .await
                .unwrap_or_default();
            if let Some(loop_info) = loop_info {
                let _ = state.events.send(ServerEvent::LoopEnded {
                    loop_info,
                    host_id: host_id.to_string(),
                    hostname,
                });
            }

            tracing::info!(host_id = %host_id, loop_id = %loop_id, reason = %reason, "agentic loop ended");
        }
    }
    Ok(())
}

/// Handle a Claude agent message: update DB state and emit events.
async fn handle_claude_message(
    state: &AppState,
    host_id: HostId,
    msg: zremote_protocol::claude::ClaudeAgentMessage,
) -> Result<(), String> {
    use zremote_protocol::claude::ClaudeAgentMessage;

    match msg {
        ClaudeAgentMessage::SessionStarted {
            claude_task_id,
            session_id: _,
        } => {
            let task_id_str = claude_task_id.to_string();
            tracing::info!(host_id = %host_id, task_id = %task_id_str, "claude session started");
            // Status stays 'starting' until LoopDetected links it
        }
        ClaudeAgentMessage::SessionStartFailed {
            claude_task_id,
            session_id: _,
            error,
        } => {
            let task_id_str = claude_task_id.to_string();
            let now = chrono::Utc::now().to_rfc3339();
            tracing::warn!(host_id = %host_id, task_id = %task_id_str, error = %error, "claude session start failed");

            if let Err(e) = sqlx::query(
                "UPDATE claude_sessions SET status = 'error', ended_at = ? WHERE id = ?",
            )
            .bind(&now)
            .bind(&task_id_str)
            .execute(&state.db)
            .await
            {
                tracing::error!(task_id = %task_id_str, error = %e, "failed to update claude task status");
            }

            let _ = state.events.send(ServerEvent::ClaudeTaskEnded {
                task_id: task_id_str,
                status: "error".to_string(),
                summary: Some(error),
            });
        }
        ClaudeAgentMessage::SessionsDiscovered {
            project_path,
            sessions,
        } => {
            tracing::info!(
                host_id = %host_id,
                project_path = %project_path,
                count = sessions.len(),
                "claude sessions discovered"
            );

            // Resolve pending discover request via oneshot channel
            let request_key = format!("{host_id}:{project_path}");
            if let Some((_, tx)) = state.claude_discover_requests.remove(&request_key) {
                let _ = tx.send(sessions);
            }
        }
        ClaudeAgentMessage::SessionIdCaptured {
            claude_task_id,
            cc_session_id,
        } => {
            let task_id_str = claude_task_id.to_string();
            tracing::info!(
                host_id = %host_id,
                task_id = %task_id_str,
                cc_session_id = %cc_session_id,
                "claude session ID captured"
            );

            if let Err(e) =
                sqlx::query("UPDATE claude_sessions SET claude_session_id = ? WHERE id = ?")
                    .bind(&cc_session_id)
                    .bind(&task_id_str)
                    .execute(&state.db)
                    .await
            {
                tracing::error!(
                    task_id = %task_id_str,
                    error = %e,
                    "failed to store claude_session_id"
                );
            }
        }
        ClaudeAgentMessage::MetricsUpdate {
            cc_session_id,
            model,
            cost_usd,
            tokens_in,
            tokens_out,
            context_used_pct,
            context_window_size,
            rate_limit_5h_pct,
            rate_limit_7d_pct,
            lines_added,
            lines_removed,
            cc_version,
        } => {
            tracing::debug!(
                host_id = %host_id,
                cc_session_id = %cc_session_id,
                "claude session metrics update from agent"
            );

            #[allow(clippy::cast_possible_wrap, clippy::cast_precision_loss)]
            match zremote_core::queries::claude_sessions::update_session_metrics(
                &state.db,
                &cc_session_id,
                model.as_deref(),
                cost_usd,
                tokens_in.map(|v| v as i64),
                tokens_out.map(|v| v as i64),
                context_used_pct.map(|v| v as f64),
                context_window_size.map(|v| v as i64),
                rate_limit_5h_pct.map(|v| v as i64),
                rate_limit_7d_pct.map(|v| v as i64),
                lines_added,
                lines_removed,
                cc_version.as_deref(),
            )
            .await
            {
                Ok(true) => {
                    tracing::debug!(cc_session_id, "updated claude session metrics via agent");
                    let _ = state.events.send(ServerEvent::ClaudeSessionMetrics {
                        session_id: cc_session_id,
                        model,
                        context_used_pct: context_used_pct.map(|v| v as f64),
                        context_window_size,
                        cost_usd,
                        tokens_in,
                        tokens_out,
                        lines_added,
                        lines_removed,
                        rate_limit_5h_pct,
                        rate_limit_7d_pct,
                    });
                }
                Ok(false) => {
                    tracing::debug!(
                        cc_session_id,
                        "no matching claude_session for metrics update"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        cc_session_id,
                        error = %e,
                        "failed to update session metrics"
                    );
                }
            }
        }
    }
    Ok(())
}

/// Handle a knowledge agent message: update DB and in-memory state.
#[allow(clippy::too_many_lines)]
async fn handle_knowledge_message(
    state: &AppState,
    host_id: HostId,
    msg: zremote_protocol::knowledge::KnowledgeAgentMessage,
) -> Result<(), String> {
    use zremote_protocol::knowledge::KnowledgeAgentMessage;

    let host_id_str = host_id.to_string();

    match msg {
        KnowledgeAgentMessage::ServiceStatus {
            status,
            version,
            error,
        } => {
            let status_str = serde_json::to_value(status)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{status:?}").to_lowercase());

            let kb_id = Uuid::new_v4().to_string();
            let now = chrono::Utc::now().to_rfc3339();

            // Upsert knowledge_bases
            if let Err(e) = sqlx::query(
                "INSERT INTO knowledge_bases (id, host_id, status, openviking_version, last_error, started_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(host_id) DO UPDATE SET \
                 status = excluded.status, openviking_version = COALESCE(excluded.openviking_version, knowledge_bases.openviking_version), \
                 last_error = excluded.last_error, updated_at = excluded.updated_at",
            )
            .bind(&kb_id)
            .bind(&host_id_str)
            .bind(&status_str)
            .bind(&version)
            .bind(&error)
            .bind(if status_str == "ready" {
                Some(&now)
            } else {
                None
            })
            .bind(&now)
            .execute(&state.db)
            .await
            {
                tracing::warn!(host_id = %host_id, error = %e, "failed to upsert knowledge base status");
            }

            let _ = state
                .events
                .send(crate::state::ServerEvent::KnowledgeStatusChanged {
                    host_id: host_id_str,
                    status: status_str,
                    error,
                });
        }
        KnowledgeAgentMessage::KnowledgeBaseReady {
            project_path,
            total_files,
            total_chunks,
        } => {
            tracing::info!(
                host_id = %host_id,
                project_path,
                total_files,
                total_chunks,
                "knowledge base ready"
            );
        }
        KnowledgeAgentMessage::IndexingProgress {
            project_path,
            status,
            files_processed,
            files_total,
            error,
        } => {
            let status_str = serde_json::to_value(status)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{status:?}").to_lowercase());

            // Find project_id from path + host
            let project: Option<(String,)> =
                sqlx::query_as("SELECT id FROM projects WHERE host_id = ? AND path = ?")
                    .bind(&host_id_str)
                    .bind(&project_path)
                    .fetch_optional(&state.db)
                    .await
                    .unwrap_or(None);

            if let Some((project_id,)) = project {
                let indexing_id = Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();

                // Upsert indexing status
                if let Err(e) = sqlx::query(
                    "INSERT INTO knowledge_indexing (id, project_id, status, files_processed, files_total, started_at, error) \
                     VALUES (?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(id) DO UPDATE SET \
                     status = excluded.status, files_processed = excluded.files_processed, \
                     files_total = excluded.files_total, error = excluded.error, \
                     completed_at = CASE WHEN excluded.status IN ('completed', 'failed') THEN ? ELSE NULL END",
                )
                .bind(&indexing_id)
                .bind(&project_id)
                .bind(&status_str)
                .bind(i64::try_from(files_processed).unwrap_or(0))
                .bind(i64::try_from(files_total).unwrap_or(0))
                .bind(&now)
                .bind(&error)
                .bind(&now)
                .execute(&state.db)
                .await
                {
                    tracing::warn!(error = %e, "failed to upsert indexing status");
                }

                let _ = state
                    .events
                    .send(crate::state::ServerEvent::IndexingProgress {
                        project_id,
                        project_path: project_path.clone(),
                        status: status_str,
                        files_processed,
                        files_total,
                    });
            } else {
                tracing::warn!(
                    host_id = %host_id,
                    project_path,
                    "indexing progress for unknown project"
                );
            }
        }
        KnowledgeAgentMessage::SearchResults {
            project_path: _,
            request_id,
            results,
            duration_ms,
        } => {
            // Route response to waiting HTTP handler via oneshot
            if let Some((_, sender)) = state.knowledge_requests.remove(&request_id) {
                let _ = sender.send(KnowledgeAgentMessage::SearchResults {
                    project_path: String::new(),
                    request_id,
                    results,
                    duration_ms,
                });
            } else {
                tracing::warn!(request_id = %request_id, "no pending request for search results");
            }
        }
        KnowledgeAgentMessage::MemoryExtracted { loop_id, memories } => {
            let loop_id_str = loop_id.to_string();

            // Find project from loop's project_path
            let project_path: Option<(String,)> =
                sqlx::query_as("SELECT project_path FROM agentic_loops WHERE id = ?")
                    .bind(&loop_id_str)
                    .fetch_optional(&state.db)
                    .await
                    .unwrap_or(None);

            if let Some((ref path,)) = project_path {
                let project: Option<(String,)> =
                    sqlx::query_as("SELECT id FROM projects WHERE host_id = ? AND path = ?")
                        .bind(&host_id_str)
                        .bind(path)
                        .fetch_optional(&state.db)
                        .await
                        .unwrap_or(None);

                if let Some((project_id,)) = project {
                    let memory_count = u32::try_from(memories.len()).unwrap_or(u32::MAX);

                    for memory in &memories {
                        let memory_id = Uuid::new_v4().to_string();
                        let category_str = serde_json::to_value(memory.category)
                            .ok()
                            .and_then(|v| v.as_str().map(String::from))
                            .unwrap_or_else(|| "pattern".to_string());

                        // Check for existing memory with same key (dedup)
                        let existing: Option<(String, f64)> = sqlx::query_as(
                            "SELECT id, confidence FROM knowledge_memories WHERE project_id = ? AND key = ?"
                        )
                        .bind(&project_id)
                        .bind(&memory.key)
                        .fetch_optional(&state.db)
                        .await
                        .unwrap_or(None);

                        match existing {
                            Some((existing_id, existing_conf))
                                if memory.confidence > existing_conf =>
                            {
                                // Update existing memory with higher confidence
                                if let Err(e) = sqlx::query(
                                    "UPDATE knowledge_memories SET content = ?, confidence = ?, loop_id = ?, \
                                     category = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?"
                                )
                                .bind(&memory.content)
                                .bind(memory.confidence)
                                .bind(&loop_id_str)
                                .bind(&category_str)
                                .bind(&existing_id)
                                .execute(&state.db)
                                .await {
                                    tracing::warn!(error = %e, "failed to update memory");
                                }
                            }
                            Some(_) => {
                                // Existing memory has equal or higher confidence, skip
                                tracing::debug!(key = %memory.key, "skipping memory with lower confidence");
                            }
                            None => {
                                // Insert new memory
                                if let Err(e) = sqlx::query(
                                    "INSERT INTO knowledge_memories (id, project_id, loop_id, key, content, category, confidence) \
                                     VALUES (?, ?, ?, ?, ?, ?, ?)"
                                )
                                .bind(&memory_id)
                                .bind(&project_id)
                                .bind(&loop_id_str)
                                .bind(&memory.key)
                                .bind(&memory.content)
                                .bind(&category_str)
                                .bind(memory.confidence)
                                .execute(&state.db)
                                .await {
                                    tracing::warn!(error = %e, "failed to insert memory");
                                }
                            }
                        }
                    }

                    let _ = state
                        .events
                        .send(crate::state::ServerEvent::MemoryExtracted {
                            project_id,
                            loop_id: loop_id_str,
                            memory_count,
                        });

                    // Phase 3: Increment memories_since_regen and check auto-regeneration threshold
                    if let Err(e) = sqlx::query(
                        "UPDATE knowledge_bases SET memories_since_regen = memories_since_regen + ? WHERE host_id = ?"
                    )
                    .bind(i64::from(memory_count))
                    .bind(&host_id_str)
                    .execute(&state.db)
                    .await {
                        tracing::warn!(error = %e, "failed to increment memories_since_regen");
                    }

                    // Check if we should auto-regenerate
                    let threshold: i64 = sqlx::query_as::<_, (String,)>(
                        "SELECT value FROM config_global WHERE key = 'openviking.regenerate_threshold'"
                    )
                    .fetch_optional(&state.db)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|(v,)| v.parse().ok())
                    .unwrap_or(5);

                    let current_count: Option<(i64,)> = sqlx::query_as(
                        "SELECT memories_since_regen FROM knowledge_bases WHERE host_id = ?",
                    )
                    .bind(&host_id_str)
                    .fetch_optional(&state.db)
                    .await
                    .unwrap_or(None);

                    if let Some((count,)) = current_count
                        && count >= threshold
                        && let Some(sender) = state.connections.get_sender(&host_id).await
                    {
                        // WriteClaudeMd with empty content triggers the agent to
                        // generate instructions and write them in section mode.
                        let _ = sender.send(zremote_protocol::ServerMessage::KnowledgeAction(
                            zremote_protocol::knowledge::KnowledgeServerMessage::WriteClaudeMd {
                                project_path: path.clone(),
                                content: String::new(),
                                mode: zremote_protocol::knowledge::WriteMdMode::Section,
                            }
                        )).await;

                        // Also trigger skills generation
                        let _ = sender.send(zremote_protocol::ServerMessage::KnowledgeAction(
                            zremote_protocol::knowledge::KnowledgeServerMessage::GenerateSkills {
                                project_path: path.clone(),
                            }
                        )).await;

                        // Reset counter
                        let _ = sqlx::query(
                            "UPDATE knowledge_bases SET memories_since_regen = 0, \
                             last_regenerated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE host_id = ?"
                        )
                        .bind(&host_id_str)
                        .execute(&state.db)
                        .await;

                        tracing::info!(host_id = %host_id, path, count, threshold, "triggered auto-regeneration");
                    }
                }
            }
        }
        KnowledgeAgentMessage::InstructionsGenerated {
            project_path,
            content,
            memories_used,
        } => {
            // Generate the same deterministic request_id to find the waiting handler
            let request_id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("instructions:{host_id_str}:{project_path}").as_bytes(),
            );

            if let Some((_, sender)) = state.knowledge_requests.remove(&request_id) {
                let _ = sender.send(KnowledgeAgentMessage::InstructionsGenerated {
                    project_path,
                    content,
                    memories_used,
                });
            } else {
                tracing::warn!(
                    host_id = %host_id,
                    project_path,
                    "no pending request for generated instructions"
                );
            }
        }
        KnowledgeAgentMessage::ClaudeMdWritten {
            project_path,
            bytes_written,
            error,
        } => {
            let request_id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("write-claude-md:{host_id_str}:{project_path}").as_bytes(),
            );

            if let Some((_, sender)) = state.knowledge_requests.remove(&request_id) {
                let _ = sender.send(KnowledgeAgentMessage::ClaudeMdWritten {
                    project_path,
                    bytes_written,
                    error,
                });
            } else {
                tracing::info!(
                    host_id = %host_id,
                    project_path,
                    bytes_written,
                    "CLAUDE.md written (no waiting handler)"
                );
            }
        }
        KnowledgeAgentMessage::BootstrapComplete {
            project_path,
            files_indexed,
            memories_seeded,
            error,
        } => {
            if let Some(ref err) = error {
                tracing::warn!(
                    host_id = %host_id,
                    project_path,
                    error = %err,
                    "bootstrap failed"
                );
            } else {
                tracing::info!(
                    host_id = %host_id,
                    project_path,
                    files_indexed,
                    memories_seeded,
                    "bootstrap complete"
                );
            }
        }
        KnowledgeAgentMessage::SkillsGenerated {
            project_path,
            skills_written,
        } => {
            tracing::info!(
                host_id = %host_id,
                project_path,
                skills_written,
                "skills generated"
            );
        }
    }
    Ok(())
}

/// Upsert a host record in the database. Look up by hostname only.
/// If found, update the existing record. Otherwise, create a new host.
// TODO(phase-3): Validate token matches stored hash before allowing upsert to prevent hostname hijack
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
async fn cleanup_agent(state: &AppState, host_id: &HostId, generation: u64) {
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
                    s.host_id == *host_id && s.status != "closed" && s.status != "suspended"
                })
                .map(|(id, _)| *id)
                .collect();

            for &sid in &session_ids {
                if let Some(session) = sessions.get_mut(&sid) {
                    session.status = "suspended".to_string();
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
    }

    // Mark starting/active claude_sessions for this host as error
    if let Err(e) = sqlx::query(
        "UPDATE claude_sessions SET status = 'error', ended_at = ? \
         WHERE host_id = ? AND status IN ('starting', 'active')",
    )
    .bind(&now)
    .bind(&host_id_str)
    .execute(&state.db)
    .await
    {
        tracing::error!(host_id = %host_id, error = %e, "failed to mark claude sessions as error on disconnect");
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
                        let _ = state.events.send(ServerEvent::HostStatusChanged {
                            host_id: host_id.to_string(),
                            status: "offline".to_string(),
                        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use uuid::Uuid;
    use zremote_protocol::agentic::{AgenticAgentMessage, AgenticStatus};

    use crate::state::{AppState, ConnectionManager};

    async fn test_state() -> Arc<AppState> {
        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
        let connections = Arc::new(ConnectionManager::new());
        let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let agentic_loops = Arc::new(dashmap::DashMap::new());
        let (events_tx, _) = tokio::sync::broadcast::channel(1024);
        Arc::new(AppState {
            db: pool,
            connections,
            sessions,
            agentic_loops,
            agent_token_hash: crate::auth::hash_token("test-token"),
            shutdown: tokio_util::sync::CancellationToken::new(),
            events: events_tx,
            knowledge_requests: Arc::new(dashmap::DashMap::new()),
            claude_discover_requests: Arc::new(dashmap::DashMap::new()),
            directory_requests: Arc::new(dashmap::DashMap::new()),
            settings_get_requests: Arc::new(dashmap::DashMap::new()),
            settings_save_requests: Arc::new(dashmap::DashMap::new()),
            action_inputs_requests: Arc::new(dashmap::DashMap::new()),
        })
    }

    async fn insert_test_host(state: &AppState, id: &str, hostname: &str) {
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, agent_version, os, arch, \
             status, last_seen_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'testhash', '0.1.0', 'linux', 'x86_64', 'online', \
             '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(hostname)
        .bind(hostname)
        .execute(&state.db)
        .await
        .unwrap();
    }

    async fn insert_test_session(state: &AppState, session_id: &str, host_id: &str, status: &str) {
        sqlx::query("INSERT INTO sessions (id, host_id, status) VALUES (?, ?, ?)")
            .bind(session_id)
            .bind(host_id)
            .bind(status)
            .execute(&state.db)
            .await
            .unwrap();
    }

    // ── TERMINAL_MSG_TYPES / AGENTIC_MSG_TYPES ──

    #[test]
    fn terminal_msg_types_contains_expected() {
        assert!(TERMINAL_MSG_TYPES.contains(&"Register"));
        assert!(TERMINAL_MSG_TYPES.contains(&"Heartbeat"));
        assert!(TERMINAL_MSG_TYPES.contains(&"TerminalOutput"));
        assert!(TERMINAL_MSG_TYPES.contains(&"SessionCreated"));
        assert!(TERMINAL_MSG_TYPES.contains(&"SessionClosed"));
        assert!(TERMINAL_MSG_TYPES.contains(&"Error"));
        assert!(TERMINAL_MSG_TYPES.contains(&"ProjectDiscovered"));
        assert!(TERMINAL_MSG_TYPES.contains(&"ProjectList"));
        assert!(TERMINAL_MSG_TYPES.contains(&"SessionsRecovered"));
    }

    #[test]
    fn agentic_msg_types_contains_expected() {
        assert!(AGENTIC_MSG_TYPES.contains(&"LoopDetected"));
        assert!(AGENTIC_MSG_TYPES.contains(&"LoopStateUpdate"));
        assert!(AGENTIC_MSG_TYPES.contains(&"LoopEnded"));
        assert_eq!(AGENTIC_MSG_TYPES.len(), 3);
    }

    #[test]
    fn msg_types_are_disjoint() {
        for t in AGENTIC_MSG_TYPES {
            assert!(
                !TERMINAL_MSG_TYPES.contains(t),
                "type {t} appears in both TERMINAL and AGENTIC"
            );
        }
    }

    // ── upsert_host ──

    #[tokio::test]
    async fn upsert_host_creates_new_host() {
        let state = test_state().await;
        let host_id = upsert_host(&state, "myhost.local", "0.5.0", "linux", "x86_64", "secret")
            .await
            .unwrap();

        // Verify host was inserted
        let row: (String, String, String) =
            sqlx::query_as("SELECT id, hostname, status FROM hosts WHERE id = ?")
                .bind(host_id.to_string())
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(row.1, "myhost.local");
        assert_eq!(row.2, "online");
    }

    #[tokio::test]
    async fn upsert_host_updates_existing_host() {
        let state = test_state().await;

        // First call creates
        let id1 = upsert_host(&state, "myhost.local", "0.5.0", "linux", "x86_64", "secret")
            .await
            .unwrap();

        // Second call with same hostname updates (should return same ID)
        let id2 = upsert_host(
            &state,
            "myhost.local",
            "0.6.0",
            "darwin",
            "arm64",
            "new-secret",
        )
        .await
        .unwrap();

        assert_eq!(id1, id2, "same hostname should reuse the same host ID");

        // Verify updated fields
        let row: (String, String, String) =
            sqlx::query_as("SELECT agent_version, os, arch FROM hosts WHERE id = ?")
                .bind(id1.to_string())
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(row.0, "0.6.0");
        assert_eq!(row.1, "darwin");
        assert_eq!(row.2, "arm64");

        // Verify no duplicates
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hosts")
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(count.0, 1, "should not create duplicate host entries");
    }

    #[tokio::test]
    async fn upsert_host_returns_valid_uuid() {
        let state = test_state().await;
        let host_id = upsert_host(&state, "test.host", "0.1.0", "linux", "x86_64", "token")
            .await
            .unwrap();
        // HostId is Uuid -- verify it's a valid v4 UUID by round-tripping
        let parsed: Uuid = host_id.to_string().parse().unwrap();
        assert_eq!(parsed, host_id);
    }

    // ── fetch_loop_info ──

    #[tokio::test]
    async fn fetch_loop_info_returns_none_for_nonexistent() {
        let state = test_state().await;
        let result = fetch_loop_info(&state, &Uuid::new_v4().to_string()).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn fetch_loop_info_returns_data_for_existing() {
        let state = test_state().await;
        let loop_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let host_id = Uuid::new_v4();

        // Insert host and session first (FK constraints)
        insert_test_host(&state, &host_id.to_string(), "test-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        // Insert agentic loop
        sqlx::query(
            "INSERT INTO agentic_loops (id, session_id, project_path, tool_name, status) \
             VALUES (?, ?, '/tmp/project', 'bash', 'active')",
        )
        .bind(loop_id.to_string())
        .bind(session_id.to_string())
        .execute(&state.db)
        .await
        .unwrap();

        let info = fetch_loop_info(&state, &loop_id.to_string()).await.unwrap();
        assert_eq!(info.id, loop_id.to_string());
        assert_eq!(info.session_id, session_id.to_string());
        assert_eq!(info.project_path.as_deref(), Some("/tmp/project"));
        assert_eq!(info.tool_name, "bash");
        assert_eq!(info.status, "active");
        assert!(info.ended_at.is_none());
        assert!(info.end_reason.is_none());
    }

    // ── handle_agentic_message ──

    #[tokio::test]
    async fn handle_agentic_loop_detected_creates_entry() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        // Set up prerequisites
        insert_test_host(&state, &host_id.to_string(), "test-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        // Register connection so get_hostname works
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "test-host".to_string(), tx, false)
            .await;

        let msg = AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/home/user/project".to_string(),
            tool_name: "bash".to_string(),
        };

        handle_agentic_message(&state, host_id, msg).await.unwrap();

        // Verify in-memory state
        assert!(state.agentic_loops.contains_key(&loop_id));
        let entry = state.agentic_loops.get(&loop_id).unwrap();
        assert_eq!(entry.session_id, session_id);
        assert_eq!(entry.host_id, host_id);
        assert!(matches!(entry.status, AgenticStatus::Working));

        // Verify DB state
        let row: (String, String, String) =
            sqlx::query_as("SELECT id, session_id, tool_name FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(row.0, loop_id.to_string());
        assert_eq!(row.1, session_id.to_string());
        assert_eq!(row.2, "bash");
    }

    #[tokio::test]
    async fn handle_agentic_loop_ended_removes_from_memory() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        // Set up prerequisites
        insert_test_host(&state, &host_id.to_string(), "test-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "test-host".to_string(), tx, false)
            .await;

        // First detect the loop
        let detect_msg = AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/tmp/proj".to_string(),
            tool_name: "bash".to_string(),
        };
        handle_agentic_message(&state, host_id, detect_msg)
            .await
            .unwrap();
        assert!(state.agentic_loops.contains_key(&loop_id));

        // Now end it
        let end_msg = AgenticAgentMessage::LoopEnded {
            loop_id,
            reason: "user_stopped".to_string(),
        };
        handle_agentic_message(&state, host_id, end_msg)
            .await
            .unwrap();

        // Verify removed from in-memory
        assert!(!state.agentic_loops.contains_key(&loop_id));

        // Verify DB updated
        let row: (String, Option<String>, Option<String>) =
            sqlx::query_as("SELECT status, ended_at, end_reason FROM agentic_loops WHERE id = ?")
                .bind(loop_id.to_string())
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(row.0, "completed");
        assert!(row.1.is_some(), "ended_at should be set");
        assert_eq!(row.2.as_deref(), Some("user_stopped"));
    }

    #[tokio::test]
    async fn handle_agentic_loop_state_update() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        insert_test_host(&state, &host_id.to_string(), "test-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "test-host".to_string(), tx, false)
            .await;

        // Detect loop first
        let detect_msg = AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: String::new(),
            tool_name: "bash".to_string(),
        };
        handle_agentic_message(&state, host_id, detect_msg)
            .await
            .unwrap();

        // Update status
        let update_msg = AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status: AgenticStatus::WaitingForInput,
            task_name: Some("Fix the build".to_string()),
        };
        handle_agentic_message(&state, host_id, update_msg)
            .await
            .unwrap();

        // Verify in-memory update
        let entry = state.agentic_loops.get(&loop_id).unwrap();
        assert!(matches!(entry.status, AgenticStatus::WaitingForInput));
        assert_eq!(entry.task_name.as_deref(), Some("Fix the build"));
    }

    // ── cleanup_agent ──

    #[tokio::test]
    async fn cleanup_non_persistent_agent_closes_sessions() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        // Insert host and session in DB
        insert_test_host(&state, &host_id.to_string(), "cleanup-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        // Add session to in-memory store
        {
            let mut sessions = state.sessions.write().await;
            sessions.insert(
                session_id,
                zremote_core::state::SessionState::new(session_id, host_id),
            );
            sessions.get_mut(&session_id).unwrap().status = "active".to_string();
        }

        // Register connection (non-persistent)
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (_, generation) = state
            .connections
            .register(host_id, "cleanup-host".to_string(), tx, false)
            .await;

        cleanup_agent(&state, &host_id, generation).await;

        // In-memory session should be removed
        {
            let sessions = state.sessions.read().await;
            assert!(
                !sessions.contains_key(&session_id),
                "session should be removed from memory"
            );
        }

        // DB session should be closed
        let row: (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(row.0, "closed");

        // Host should be offline
        let host_row: (String,) = sqlx::query_as("SELECT status FROM hosts WHERE id = ?")
            .bind(host_id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(host_row.0, "offline");
    }

    #[tokio::test]
    async fn cleanup_persistent_agent_suspends_sessions() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        insert_test_host(&state, &host_id.to_string(), "persist-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        // Add session to in-memory store
        {
            let mut sessions = state.sessions.write().await;
            sessions.insert(
                session_id,
                zremote_core::state::SessionState::new(session_id, host_id),
            );
            sessions.get_mut(&session_id).unwrap().status = "active".to_string();
        }

        // Register as persistent
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (_, generation) = state
            .connections
            .register(host_id, "persist-host".to_string(), tx, true)
            .await;

        cleanup_agent(&state, &host_id, generation).await;

        // In-memory session should still exist but be suspended
        {
            let sessions = state.sessions.read().await;
            let session = sessions
                .get(&session_id)
                .expect("session should still exist in memory");
            assert_eq!(session.status, "suspended");
        }

        // DB session should be suspended
        let row: (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(row.0, "suspended");

        // Host should be offline
        let host_row: (String,) = sqlx::query_as("SELECT status FROM hosts WHERE id = ?")
            .bind(host_id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(host_row.0, "offline");
    }

    #[tokio::test]
    async fn cleanup_stale_generation_skips() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();

        insert_test_host(&state, &host_id.to_string(), "stale-host").await;

        // Register twice -- second registration replaces the first
        let (tx1, _rx1) = tokio::sync::mpsc::channel(16);
        let (_, gen1) = state
            .connections
            .register(host_id, "stale-host".to_string(), tx1, false)
            .await;

        let (tx2, _rx2) = tokio::sync::mpsc::channel(16);
        let (_, gen2) = state
            .connections
            .register(host_id, "stale-host".to_string(), tx2, false)
            .await;

        assert!(gen2 > gen1);

        // Cleanup with the old generation -- should be a no-op
        cleanup_agent(&state, &host_id, gen1).await;

        // Host should still be online (cleanup skipped)
        let host_row: (String,) = sqlx::query_as("SELECT status FROM hosts WHERE id = ?")
            .bind(host_id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(
            host_row.0, "online",
            "stale generation cleanup should not mark host offline"
        );

        // Connection should still exist
        assert!(
            state.connections.get_hostname(&host_id).await.is_some(),
            "newer connection should still be registered"
        );
    }
}
