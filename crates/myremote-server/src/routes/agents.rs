use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use chrono::Utc;
use myremote_protocol::{AgentMessage, HostId, ServerMessage};
use myremote_protocol::agentic::{AgenticAgentMessage, AgenticStatus};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::auth;
use crate::state::{AgenticLoopState, AppState, HostInfo, PendingToolCall, ServerEvent, SessionInfo};

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
}

/// Receive a raw `AgentMessage` during registration (before the main loop).
async fn recv_terminal_message(socket: &mut WebSocket) -> Option<AgentMessage> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<AgentMessage>(&text) {
                    Ok(msg) => return Some(msg),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to deserialize register message");
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

/// Perform the registration handshake: wait for Register message, validate
/// token, upsert host, register connection, and send `RegisterAck`.
/// Returns `None` if any step fails (errors are sent to the agent).
async fn register_agent(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
) -> Option<RegisteredAgent> {
    // 1. Wait for Register message with timeout
    let register_msg = match tokio::time::timeout(REGISTER_TIMEOUT, recv_terminal_message(socket))
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

    Some(RegisteredAgent {
        host_id,
        generation,
        rx,
        hostname,
        agent_version,
        os,
        arch,
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
    "Register", "Heartbeat", "TerminalOutput", "SessionCreated", "SessionClosed", "Error",
    "ProjectDiscovered", "ProjectList", "KnowledgeAction",
];

/// Known `AgenticAgentMessage` type tags.
const AGENTIC_MSG_TYPES: &[&str] = &[
    "LoopDetected", "LoopStateUpdate", "LoopToolCall", "LoopToolResult",
    "LoopTranscript", "LoopMetrics", "LoopEnded",
];

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

                let msg_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("").to_owned();

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
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|e| {
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
        AgentMessage::TerminalOutput { session_id, data } => {
            let mut sessions = state.sessions.write().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                let browser_msg = crate::state::BrowserMessage::Output { data: data.clone() };
                session.append_scrollback(data);
                // Forward to all browser senders, remove dead ones
                session.browser_senders.retain(|sender| {
                    sender.try_send(browser_msg.clone()).is_ok()
                });
            }
        }
        AgentMessage::SessionCreated {
            session_id,
            shell,
            pid,
        } => {
            // Update DB
            let session_id_str = session_id.to_string();
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(e) = sqlx::query(
                "UPDATE sessions SET status = 'active', shell = ?, pid = ?, created_at = ? WHERE id = ?",
            )
            .bind(&shell)
            .bind(i64::from(pid))
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
        AgentMessage::ProjectDiscovered {
            path,
            name,
            has_claude_config,
            project_type,
        } => {
            let host_id_str = host_id.to_string();
            let project_id = Uuid::new_v4().to_string();
            if let Err(e) = sqlx::query(
                "INSERT INTO projects (id, host_id, path, name, has_claude_config, project_type) \
                 VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(host_id, path) DO UPDATE SET \
                 name = excluded.name, has_claude_config = excluded.has_claude_config, \
                 project_type = excluded.project_type",
            )
            .bind(&project_id)
            .bind(&host_id_str)
            .bind(&path)
            .bind(&name)
            .bind(has_claude_config)
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
        AgentMessage::KnowledgeAction(knowledge_msg) => {
            if let Err(e) = handle_knowledge_message(state, host_id, knowledge_msg).await {
                tracing::error!(host_id = %host_id, error = %e, "error handling knowledge message");
            }
        }
        AgentMessage::ProjectList { projects } => {
            let host_id_str = host_id.to_string();
            tracing::info!(host_id = %host_id, count = projects.len(), "received project list");
            for project in projects {
                let project_id = Uuid::new_v4().to_string();
                if let Err(e) = sqlx::query(
                    "INSERT INTO projects (id, host_id, path, name, has_claude_config, project_type) \
                     VALUES (?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(host_id, path) DO UPDATE SET \
                     name = excluded.name, has_claude_config = excluded.has_claude_config, \
                     project_type = excluded.project_type",
                )
                .bind(&project_id)
                .bind(&host_id_str)
                .bind(&project.path)
                .bind(&project.name)
                .bind(project.has_claude_config)
                .bind(&project.project_type)
                .execute(&state.db)
                .await
                {
                    tracing::warn!(host_id = %host_id, path = %project.path, error = %e, "failed to upsert project");
                }
            }
            let _ = state.events.send(ServerEvent::ProjectsUpdated {
                host_id: host_id.to_string(),
            });
        }
    }
    Ok(())
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
            model,
        } => {
            let loop_id_str = loop_id.to_string();
            let session_id_str = session_id.to_string();

            if let Err(e) = sqlx::query(
                "INSERT INTO agentic_loops (id, session_id, project_path, tool_name, model) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&loop_id_str)
            .bind(&session_id_str)
            .bind(&project_path)
            .bind(&tool_name)
            .bind(&model)
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
                    status: AgenticStatus::Working,
                    pending_tool_calls: std::collections::VecDeque::new(),
                    tokens_in: 0,
                    tokens_out: 0,
                    estimated_cost_usd: 0.0,
                    last_updated: Instant::now(),
                },
            );

            tracing::info!(host_id = %host_id, loop_id = %loop_id, tool_name = %tool_name, "agentic loop detected");
        }
        AgenticAgentMessage::LoopStateUpdate {
            loop_id,
            status,
            ..
        } => {
            // Update in-memory state
            let session_id_for_event = state.agentic_loops.get(&loop_id).map(|e| e.session_id);
            if let Some(mut entry) = state.agentic_loops.get_mut(&loop_id) {
                entry.status = status;
                entry.last_updated = Instant::now();
            }

            // Update DB
            let loop_id_str = loop_id.to_string();
            let status_str = serde_json::to_value(status)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{status:?}").to_lowercase());

            if let Err(e) = sqlx::query(
                "UPDATE agentic_loops SET status = ? WHERE id = ?",
            )
            .bind(&status_str)
            .bind(&loop_id_str)
            .execute(&state.db)
            .await
            {
                tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop status in DB");
            }

            // Look up tool_name from DB for the event
            let tool_name: String = sqlx::query_scalar(
                "SELECT tool_name FROM agentic_loops WHERE id = ?",
            )
            .bind(&loop_id_str)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();

            let hostname = state.connections.get_hostname(&host_id).await.unwrap_or_default();
            let _ = state.events.send(ServerEvent::LoopStatusChanged {
                loop_id: loop_id_str,
                session_id: session_id_for_event.map_or_else(String::new, |s| s.to_string()),
                host_id: host_id.to_string(),
                hostname,
                status: status_str,
                tool_name,
            });
        }
        AgenticAgentMessage::LoopToolCall {
            loop_id,
            tool_call_id,
            tool_name,
            arguments_json,
            status,
        } => {
            // Validate arguments_json is valid JSON
            let arguments_json = match serde_json::from_str::<serde_json::Value>(&arguments_json) {
                Ok(_) => arguments_json,
                Err(e) => {
                    tracing::warn!(loop_id = %loop_id, tool_call_id = %tool_call_id, error = %e, "invalid arguments_json, replacing with empty object");
                    "{}".to_string()
                }
            };

            let tool_call_id_str = tool_call_id.to_string();
            let loop_id_str = loop_id.to_string();
            let status_str = serde_json::to_value(status)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{status:?}").to_lowercase());

            if let Err(e) = sqlx::query(
                "INSERT INTO tool_calls (id, loop_id, tool_name, arguments_json, status) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&tool_call_id_str)
            .bind(&loop_id_str)
            .bind(&tool_name)
            .bind(&arguments_json)
            .bind(&status_str)
            .execute(&state.db)
            .await
            {
                tracing::warn!(loop_id = %loop_id, error = %e, "failed to insert tool call");
            }

            // Add to in-memory pending queue if pending
            if status == myremote_protocol::ToolCallStatus::Pending {
                if let Some(mut entry) = state.agentic_loops.get_mut(&loop_id) {
                    entry.pending_tool_calls.push_back(PendingToolCall {
                        tool_call_id,
                        tool_name: tool_name.clone(),
                        arguments_json: arguments_json.clone(),
                    });
                    entry.last_updated = Instant::now();
                }

                // Truncate arguments preview for the event
                let arguments_preview = if arguments_json.len() > 200 {
                    format!("{}...", &arguments_json[..200])
                } else {
                    arguments_json
                };

                let hostname = state.connections.get_hostname(&host_id).await.unwrap_or_default();
                let _ = state.events.send(ServerEvent::ToolCallPending {
                    loop_id: loop_id_str,
                    tool_call_id: tool_call_id_str,
                    host_id: host_id.to_string(),
                    hostname,
                    tool_name,
                    arguments_preview,
                });
            }
        }
        AgenticAgentMessage::LoopToolResult {
            loop_id,
            tool_call_id,
            result_preview,
            duration_ms,
        } => {
            let tool_call_id_str = tool_call_id.to_string();
            let now = chrono::Utc::now().to_rfc3339();

            if let Err(e) = sqlx::query(
                "UPDATE tool_calls SET status = 'completed', result_preview = ?, \
                 duration_ms = ?, resolved_at = ? WHERE id = ?",
            )
            .bind(&result_preview)
            .bind(i64::try_from(duration_ms).unwrap_or(i64::MAX))
            .bind(&now)
            .bind(&tool_call_id_str)
            .execute(&state.db)
            .await
            {
                tracing::warn!(loop_id = %loop_id, error = %e, "failed to update tool call result");
            }

            // Remove from pending queue
            if let Some(mut entry) = state.agentic_loops.get_mut(&loop_id) {
                entry.pending_tool_calls.retain(|tc| tc.tool_call_id != tool_call_id);
                entry.last_updated = Instant::now();
            }
        }
        AgenticAgentMessage::LoopTranscript {
            loop_id,
            role,
            content,
            tool_call_id,
            timestamp,
        } => {
            let loop_id_str = loop_id.to_string();
            let role_str = serde_json::to_value(role)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{role:?}").to_lowercase());
            let tool_call_id_str = tool_call_id.map(|id: uuid::Uuid| id.to_string());
            let timestamp_str = timestamp.to_rfc3339();

            if let Err(e) = sqlx::query(
                "INSERT INTO transcript_entries (loop_id, role, content, tool_call_id, timestamp) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&loop_id_str)
            .bind(&role_str)
            .bind(&content)
            .bind(&tool_call_id_str)
            .bind(&timestamp_str)
            .execute(&state.db)
            .await
            {
                tracing::warn!(loop_id = %loop_id, error = %e, "failed to insert transcript entry");
            }
        }
        AgenticAgentMessage::LoopMetrics {
            loop_id,
            tokens_in,
            tokens_out,
            estimated_cost_usd,
            ..
        } => {
            // Update in-memory state
            if let Some(mut entry) = state.agentic_loops.get_mut(&loop_id) {
                entry.tokens_in = tokens_in;
                entry.tokens_out = tokens_out;
                entry.estimated_cost_usd = estimated_cost_usd;
                entry.last_updated = Instant::now();
            }

            // Update DB
            let loop_id_str = loop_id.to_string();
            if let Err(e) = sqlx::query(
                "UPDATE agentic_loops SET total_tokens_in = ?, total_tokens_out = ?, \
                 estimated_cost_usd = ? WHERE id = ?",
            )
            .bind(i64::try_from(tokens_in).unwrap_or(i64::MAX))
            .bind(i64::try_from(tokens_out).unwrap_or(i64::MAX))
            .bind(estimated_cost_usd)
            .bind(&loop_id_str)
            .execute(&state.db)
            .await
            {
                tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop metrics in DB");
            }
        }
        AgenticAgentMessage::LoopEnded {
            loop_id,
            reason,
            summary,
        } => {
            let loop_id_str = loop_id.to_string();
            let now = chrono::Utc::now().to_rfc3339();

            // Grab cost before removing from in-memory state
            let cost = state
                .agentic_loops
                .get(&loop_id)
                .map_or(0.0, |e| e.estimated_cost_usd);

            if let Err(e) = sqlx::query(
                "UPDATE agentic_loops SET status = 'completed', ended_at = ?, \
                 end_reason = ?, summary = ? WHERE id = ?",
            )
            .bind(&now)
            .bind(&reason)
            .bind(&summary)
            .bind(&loop_id_str)
            .execute(&state.db)
            .await
            {
                tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop ended in DB");
            }

            // Remove from in-memory state
            state.agentic_loops.remove(&loop_id);

            let hostname = state.connections.get_hostname(&host_id).await.unwrap_or_default();
            let _ = state.events.send(ServerEvent::LoopEnded {
                loop_id: loop_id_str.clone(),
                host_id: host_id.to_string(),
                hostname,
                reason: reason.clone(),
                summary: summary.clone(),
                cost,
            });

            tracing::info!(host_id = %host_id, loop_id = %loop_id, reason = %reason, "agentic loop ended");

            // Auto-extract memories if configured
            {
                let auto_extract: Option<(String,)> = sqlx::query_as(
                    "SELECT value FROM config_global WHERE key = 'openviking.auto_extract'"
                )
                .fetch_optional(&state.db)
                .await
                .unwrap_or(None);

                let should_extract = auto_extract
                    .is_some_and(|(v,)| v == "true" || v == "1");

                if should_extract {
                    // Fetch project_path for this loop
                    let project_path: Option<(Option<String>,)> = sqlx::query_as(
                        "SELECT project_path FROM agentic_loops WHERE id = ?"
                    )
                    .bind(&loop_id_str)
                    .fetch_optional(&state.db)
                    .await
                    .unwrap_or(None);

                    if let Some((Some(ref path),)) = project_path
                        && !path.is_empty()
                    {
                        // Fetch transcript
                        let transcript_rows: Vec<(String, String, String)> = sqlx::query_as(
                            "SELECT role, content, timestamp FROM transcript_entries WHERE loop_id = ? ORDER BY id"
                        )
                        .bind(&loop_id_str)
                        .fetch_all(&state.db)
                        .await
                        .unwrap_or_default();

                        if !transcript_rows.is_empty() {
                            let transcript: Vec<myremote_protocol::knowledge::TranscriptFragment> = transcript_rows
                                .into_iter()
                                .map(|(role, content, timestamp)| myremote_protocol::knowledge::TranscriptFragment {
                                    role,
                                    content,
                                    timestamp: timestamp.parse().unwrap_or_else(|_| chrono::Utc::now()),
                                })
                                .collect();

                            if let Some(sender) = state.connections.get_sender(&host_id).await {
                                let _ = sender.send(myremote_protocol::ServerMessage::KnowledgeAction(
                                    myremote_protocol::knowledge::KnowledgeServerMessage::ExtractMemory {
                                        loop_id,
                                        project_path: path.clone(),
                                        transcript,
                                    }
                                )).await;
                                tracing::info!(loop_id = %loop_id, project_path = %path, "triggered auto memory extraction");
                            }
                        }
                    }
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
    msg: myremote_protocol::knowledge::KnowledgeAgentMessage,
) -> Result<(), String> {
    use myremote_protocol::knowledge::KnowledgeAgentMessage;

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

            let _ = state.events.send(crate::state::ServerEvent::KnowledgeStatusChanged {
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
            let project: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM projects WHERE host_id = ? AND path = ?",
            )
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

                let _ = state.events.send(crate::state::ServerEvent::IndexingProgress {
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
            let project_path: Option<(String,)> = sqlx::query_as(
                "SELECT project_path FROM agentic_loops WHERE id = ?",
            )
            .bind(&loop_id_str)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

            if let Some((ref path,)) = project_path {
                let project: Option<(String,)> = sqlx::query_as(
                    "SELECT id FROM projects WHERE host_id = ? AND path = ?",
                )
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
                            Some((existing_id, existing_conf)) if memory.confidence > existing_conf => {
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

                    let _ = state.events.send(crate::state::ServerEvent::MemoryExtracted {
                        project_id,
                        loop_id: loop_id_str,
                        memory_count,
                    });
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
    let removed = state.connections.unregister_if_generation(host_id, generation).await;

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
