use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{interval, timeout};
use tokio_tungstenite::tungstenite::Message;
use zremote_protocol::claude::{ClaudeAgentMessage, ClaudeServerMessage};
use zremote_protocol::knowledge::KnowledgeServerMessage;
use zremote_protocol::{AgentMessage, AgenticAgentMessage, HostId, ServerMessage, SessionId};

use crate::agentic::manager::AgenticLoopManager;
use crate::bridge::BridgeCommand;
use crate::bridge::{self, BridgeSenders};
use crate::config::AgentConfig;
use crate::hooks::mapper::SessionMapper;
use crate::hooks::server::HooksServer;
use crate::knowledge::KnowledgeManager;
use crate::project::ProjectScanner;
use crate::project::git::GitInspector;
use crate::session::SessionManager;

fn default_shell() -> &'static str {
    static SHELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SHELL.get_or_init(|| {
        // Read login shell from passwd database instead of $SHELL.
        // $SHELL can be overridden by nix develop to a non-interactive bash
        // (without readline), which breaks PS1 escape processing in PTY sessions.
        // The passwd entry always has the user's actual login shell.
        login_shell_from_passwd()
            .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()))
    })
}

/// Read the current user's login shell from the passwd database.
fn login_shell_from_passwd() -> Option<String> {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    let output = std::process::Command::new("getent")
        .args(["passwd", uid.trim()])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    // passwd format: name:password:uid:gid:gecos:home:shell
    let shell = output.trim().rsplit(':').next()?;
    if shell.is_empty() {
        return None;
    }
    Some(shell.to_string())
}

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
const AGENTIC_CHECK_INTERVAL: Duration = Duration::from_secs(1);

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
async fn register(
    ws: &mut WsStream,
    config: &AgentConfig,
    supports_persistence: bool,
) -> Result<HostId, ConnectionError> {
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
        supports_persistent_sessions: supports_persistence,
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
                _ => {}
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

/// Serialize an `AgentMessage` to a WS text message.
fn serialize_agent_message(msg: &AgentMessage) -> Result<Message, ConnectionError> {
    let json = serde_json::to_string(msg).map_err(ConnectionError::Serialize)?;
    Ok(Message::Text(json.into()))
}

/// Serialize an `AgenticAgentMessage` to a WS text message.
fn serialize_agentic_message(msg: &AgenticAgentMessage) -> Result<Message, ConnectionError> {
    let json = serde_json::to_string(msg).map_err(ConnectionError::Serialize)?;
    Ok(Message::Text(json.into()))
}

/// Run a worktree lifecycle hook if configured in project settings.
async fn run_worktree_hook_server(
    project_path: &str,
    worktree_path: &str,
    branch: &str,
    hook_selector: impl FnOnce(&zremote_protocol::project::WorktreeSettings) -> Option<&str>,
) -> Option<zremote_protocol::HookResultInfo> {
    let pp = project_path.to_string();
    let settings = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(std::path::Path::new(&pp))
    })
    .await
    .ok()?
    .ok()
    .flatten()?;

    let wt_settings = settings.worktree.as_ref()?;
    let template = hook_selector(wt_settings)?;

    let worktree_name = std::path::Path::new(worktree_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let cmd = crate::project::hooks::expand_hook_template(
        template,
        project_path,
        worktree_path,
        branch,
        worktree_name,
    );
    let result = crate::project::hooks::execute_hook_async(
        cmd,
        std::path::PathBuf::from(worktree_path),
        vec![],
        None,
    )
    .await;

    Some(zremote_protocol::HookResultInfo {
        success: result.success,
        output: if result.output.is_empty() {
            None
        } else {
            Some(result.output)
        },
        duration_ms: result.duration.as_millis() as u64,
    })
}

/// Read worktree settings for a project, if configured.
async fn read_worktree_settings_server(
    project_path: &str,
) -> Option<zremote_protocol::project::WorktreeSettings> {
    let pp = project_path.to_string();
    let settings = tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(std::path::Path::new(&pp))
    })
    .await
    .ok()?
    .ok()
    .flatten()?;
    settings.worktree
}

/// Handle a `SessionCreate` message: spawn a PTY and send `SessionCreated` or `Error`.
#[allow(clippy::too_many_arguments)]
async fn handle_session_create(
    session_manager: &mut SessionManager,
    outbound_tx: &mpsc::Sender<AgentMessage>,
    session_id: SessionId,
    shell: Option<&str>,
    cols: u16,
    rows: u16,
    working_dir: Option<&str>,
    env: Option<&std::collections::HashMap<String, String>>,
    initial_command: Option<&str>,
) {
    let shell = shell.unwrap_or(default_shell());
    match session_manager
        .create(session_id, shell, cols, rows, working_dir, env)
        .await
    {
        Ok(pid) => {
            tracing::info!(session_id = %session_id, pid = pid, shell = shell, "PTY session created (available via bridge)");
            if outbound_tx
                .try_send(AgentMessage::SessionCreated {
                    session_id,
                    shell: shell.to_string(),
                    pid,
                })
                .is_err()
            {
                tracing::warn!("outbound channel full, message dropped");
            }
            // Write initial command to PTY after a short delay for shell init
            if let Some(cmd) = initial_command {
                let cmd_with_newline = format!("{cmd}\n");
                std::thread::sleep(std::time::Duration::from_millis(200));
                if let Err(e) = session_manager.write_to(&session_id, cmd_with_newline.as_bytes()) {
                    tracing::warn!(session_id = %session_id, error = %e, "failed to write initial_command to PTY");
                }
            }
        }
        Err(e) => {
            tracing::error!(session_id = %session_id, error = %e, "failed to create PTY session");
            if outbound_tx
                .try_send(AgentMessage::Error {
                    session_id: Some(session_id),
                    message: format!("failed to spawn PTY: {e}"),
                })
                .is_err()
            {
                tracing::warn!("outbound channel full, message dropped");
            }
        }
    }
}

/// Run a single connection lifecycle: connect, register, then process messages.
///
/// Returns `Ok(())` on clean disconnect, `Err` on failure.
/// The caller is responsible for reconnection logic.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn run_connection(
    config: &AgentConfig,
    shutdown: tokio::sync::watch::Receiver<bool>,
    session_manager: &mut SessionManager,
    pty_output_rx: &mut mpsc::Receiver<crate::session::PtyOutput>,
    agentic_manager: &mut AgenticLoopManager,
    session_mapper: &SessionMapper,
    sent_cc_session_ids: &Arc<tokio::sync::RwLock<std::collections::HashSet<String>>>,
    ccline_rx: &mut mpsc::Receiver<AgentMessage>,
    bridge_senders: &BridgeSenders,
    bridge_scrollback: &bridge::BridgeScrollbackStore,
    bridge_cmd_rx: &mut mpsc::Receiver<BridgeCommand>,
) -> Result<(), ConnectionError> {
    let supports_persistence = session_manager.supports_persistence();
    tracing::info!(server_url = %config.server_url, "connecting to server");

    let mut ws = connect(config).await?;
    tracing::info!("WebSocket connection established");

    let host_id = register(&mut ws, config, supports_persistence).await?;

    // Split the WebSocket for concurrent read/write
    let (mut ws_sink, mut ws_stream) = ws.split();

    // Channel for outbound agent messages (from main loop + PTY output)
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<AgentMessage>(2048);

    // Re-announce sessions that survived the reconnect + discover new ones
    {
        let mut recovered_sessions = Vec::new();

        // Sessions already tracked in the hoisted manager (survived reconnect)
        for (session_id, shell, pid) in session_manager.active_session_info() {
            recovered_sessions.push(zremote_protocol::RecoveredSession {
                session_id,
                shell,
                pid,
            });
        }

        // Capture survived count before appending discovered sessions
        let survived_count = recovered_sessions.len();

        // Discover sessions from previous agent lifecycle (daemon state files).
        // discover_existing() skips sessions already tracked in the manager.
        let discovered = session_manager.discover_existing().await;
        for (session_id, shell, pid, captured) in &discovered {
            recovered_sessions.push(zremote_protocol::RecoveredSession {
                session_id: *session_id,
                shell: shell.clone(),
                pid: *pid,
            });
            // Send captured pane content through the output channel so the
            // server receives it as TerminalOutput and populates scrollback.
            if let Some(data) = captured
                && session_manager
                    .output_tx()
                    .try_send(crate::session::PtyOutput {
                        session_id: *session_id,
                        pane_id: None,
                        data: data.clone(),
                    })
                    .is_err()
            {
                tracing::warn!(session_id = %session_id, "pty output channel full, scrollback dropped");
            }
        }

        if !recovered_sessions.is_empty() {
            tracing::info!(
                count = recovered_sessions.len(),
                survived = survived_count,
                discovered = discovered.len(),
                "re-announcing sessions after reconnect"
            );
            if outbound_tx
                .try_send(AgentMessage::SessionsRecovered {
                    sessions: recovered_sessions,
                })
                .is_err()
            {
                tracing::warn!("outbound channel full, SessionsRecovered dropped");
            }
        }
    }

    // Project scanner
    let mut project_scanner = ProjectScanner::new();

    // Run initial project scan in background
    {
        let tx = outbound_tx.clone();
        let mut scanner = ProjectScanner::new();
        tokio::spawn(async move {
            let projects = tokio::task::spawn_blocking(move || scanner.scan())
                .await
                .unwrap_or_default();
            if tx
                .send(AgentMessage::ProjectList { projects })
                .await
                .is_err()
            {
                tracing::warn!("outbound channel closed, initial project list dropped");
            }
        });
    }

    // Channel for outbound agentic messages
    let (agentic_tx, mut agentic_rx) = mpsc::channel::<AgenticAgentMessage>(64);

    // Re-announce active agentic loops so the server restores monitoring state.
    // Also prunes loops whose processes died during disconnect (sends LoopEnded).
    // Must happen after agentic_tx is created since we send through it.
    {
        let loop_messages = agentic_manager.re_announce_loops();
        if !loop_messages.is_empty() {
            tracing::info!(
                count = loop_messages.len(),
                "re-announcing agentic loops after reconnect"
            );
            for msg in loop_messages {
                if agentic_tx.try_send(msg).is_err() {
                    tracing::warn!("agentic channel full, loop re-announce message dropped");
                }
            }
        }
    }

    // Hooks sidecar (CC hook integration)
    // Use a per-connection shutdown signal so the HooksServer stops when this
    // connection ends (sender is dropped on function exit → server stops).
    let (hooks_shutdown_tx, hooks_shutdown_rx) = tokio::sync::watch::channel(false);
    let hooks_server = HooksServer::new(
        agentic_tx.clone(),
        session_mapper.clone(),
        outbound_tx.clone(),
        sent_cc_session_ids.clone(),
    );
    match hooks_server.start(hooks_shutdown_rx).await {
        Ok(addr) => {
            tracing::info!(port = addr.port(), "hooks sidecar started");
            // Install hooks into Claude Code settings
            if let Err(e) = crate::hooks::installer::install_hooks().await {
                tracing::warn!(error = %e, "failed to install CC hooks (non-fatal)");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to start hooks sidecar (non-fatal)");
        }
    }

    // Knowledge manager (optional, based on config)
    #[allow(clippy::similar_names)]
    let (knowledge_sender, mut knowledge_receiver) = if config.openviking_enabled {
        let (tx, rx) = mpsc::channel::<KnowledgeServerMessage>(64);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let mut knowledge_manager = if config.openviking_enabled {
        Some(KnowledgeManager::new(
            config.openviking_binary.clone(),
            config.openviking_port,
            config.openviking_config_dir.clone(),
            config.openviking_api_key.clone(),
            outbound_tx.clone(),
        ))
    } else {
        None
    };

    // Spawn sender task: drains outbound channel + agentic channel + heartbeats -> WS sink
    let sender_shutdown = shutdown.clone();
    let sender_handle = tokio::spawn(async move {
        let mut heartbeat_interval = interval(HEARTBEAT_INTERVAL);
        // Skip the first immediate tick
        heartbeat_interval.tick().await;

        loop {
            // biased: shutdown + heartbeat checked first so they aren't
            // starved when outbound_rx is saturated with PTY output.
            tokio::select! {
                biased;

                () = wait_for_shutdown(sender_shutdown.clone()) => {
                    tracing::debug!("sender task shutting down");
                    return ws_sink;
                }
                _ = heartbeat_interval.tick() => {
                    let msg = AgentMessage::Heartbeat {
                        timestamp: Utc::now(),
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
    });

    // Periodic agentic tool detection
    let mut agentic_check_interval = interval(AGENTIC_CHECK_INTERVAL);
    // Skip the first immediate tick
    agentic_check_interval.tick().await;

    // Main message loop
    let result = loop {
        tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(server_msg) => {
                                handle_server_message(
                                    &server_msg,
                                    &host_id,
                                    session_manager,
                                    agentic_manager,
                                    &mut project_scanner,
                                    &outbound_tx,
                                    &agentic_tx,
                                    knowledge_sender.as_ref(),
                                    session_mapper,
                                    bridge_senders,
                                    bridge_scrollback,
                                ).await;
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
            Some(pty_output) = pty_output_rx.recv() => {
                let session_id = pty_output.session_id;
                let data = pty_output.data;

                if data.is_empty() {
                    // Daemon session with process still alive: reconnect instead
                    // of killing. The daemon may have disconnected us due to
                    // backpressure (write timeout) -- data is in the ring buffer.
                    if session_manager.is_daemon_alive(&session_id) {
                        tracing::info!(session_id = %session_id, "daemon still alive, attempting reconnect");
                        match session_manager.reconnect_daemon(&session_id).await {
                            Ok(scrollback) => {
                                tracing::info!(session_id = %session_id, "daemon reconnect successful");
                                // Forward scrollback so the GUI/server can repaint
                                if let Some(sb) = scrollback
                                    && !sb.is_empty()
                                {
                                    bridge::fan_out(
                                        bridge_senders,
                                        session_id,
                                        zremote_core::state::BrowserMessage::Output {
                                            pane_id: None,
                                            data: sb.clone(),
                                        },
                                    ).await;
                                    bridge::record_output(bridge_scrollback, session_id, sb.clone()).await;
                                    if outbound_tx.try_send(AgentMessage::TerminalOutput {
                                        session_id,
                                        data: sb,
                                    }).is_err() {
                                        tracing::warn!("outbound channel full, scrollback dropped");
                                    }
                                }
                                continue;
                            }
                            Err(e) => {
                                tracing::warn!(session_id = %session_id, error = %e, "daemon reconnect failed, closing session");
                                // Fall through to close
                            }
                        }
                    }

                    // Session ended (EOF from main PTY reader)
                    if let Some(loop_ended) = agentic_manager.on_session_closed(&session_id)
                        && agentic_tx.try_send(loop_ended).is_err()
                    {
                        tracing::warn!("agentic channel full, LoopEnded dropped");
                    }
                    let exit_code = session_manager.close(&session_id);
                    tracing::info!(session_id = %session_id, exit_code = ?exit_code, "PTY session ended");
                    bridge::fan_out(
                        bridge_senders,
                        session_id,
                        zremote_core::state::BrowserMessage::SessionClosed { exit_code },
                    ).await;
                    bridge::remove_session(bridge_scrollback, &session_id).await;
                    if outbound_tx.try_send(AgentMessage::SessionClosed {
                        session_id,
                        exit_code,
                    }).is_err() {
                        tracing::warn!("outbound channel full, message dropped");
                    }
                } else {
                    // Forward output to server and direct bridge GUI connections
                    bridge::fan_out(
                        bridge_senders,
                        session_id,
                        zremote_core::state::BrowserMessage::Output {
                            pane_id: None,
                            data: data.clone(),
                        },
                    ).await;
                    bridge::record_output(bridge_scrollback, session_id, data.clone()).await;
                    if outbound_tx.try_send(AgentMessage::TerminalOutput {
                        session_id,
                        data,
                    }).is_err() {
                        tracing::warn!("outbound channel full, message dropped");
                    }
                }
            }
            msg = async {
                if let Some(ref mut rx) = knowledge_receiver {
                    rx.recv().await
                } else {
                    std::future::pending().await
                }
            } => {
                if let Some(msg) = msg
                    && let Some(ref mut mgr) = knowledge_manager
                {
                    mgr.handle_message(msg).await;
                }
            }
            _ = agentic_check_interval.tick() => {
                // Periodic GC: close sessions whose child process has died but
                // EOF was lost (try_send dropped it when channel was full).
                let dead_sessions: Vec<SessionId> = session_manager
                    .session_pids()
                    .filter(|(_, pid)| !std::path::Path::new(&format!("/proc/{pid}")).exists())
                    .map(|(id, _)| id)
                    .collect();
                for session_id in dead_sessions {
                    if let Some(loop_ended) = agentic_manager.on_session_closed(&session_id)
                        && agentic_tx.try_send(loop_ended).is_err()
                    {
                        tracing::warn!("agentic channel full, LoopEnded dropped");
                    }
                    let exit_code = session_manager.close(&session_id);
                    tracing::info!(session_id = %session_id, exit_code = ?exit_code, "GC: cleaned up dead session");
                    bridge::fan_out(
                        bridge_senders,
                        session_id,
                        zremote_core::state::BrowserMessage::SessionClosed { exit_code },
                    ).await;
                    bridge::remove_session(bridge_scrollback, &session_id).await;
                    if outbound_tx.try_send(AgentMessage::SessionClosed {
                        session_id,
                        exit_code,
                    }).is_err() {
                        tracing::warn!("outbound channel full, SessionClosed dropped");
                    }
                }

                let messages = agentic_manager.check_sessions(session_manager.session_pids());
                for msg in &messages {
                    // Register loop mapping when a new loop is detected
                    if let AgenticAgentMessage::LoopDetected { loop_id, session_id, .. } = msg {
                        let mapper = session_mapper.clone();
                        let lid = *loop_id;
                        let sid = *session_id;
                        tokio::spawn(async move {
                            mapper.register_loop(sid, lid).await;
                        });
                    }
                    // Clean up mapping when loop ends
                    if let AgenticAgentMessage::LoopEnded { loop_id, .. } = msg {
                        let mapper = session_mapper.clone();
                        let lid = *loop_id;
                        tokio::spawn(async move {
                            mapper.remove_loop(&lid).await;
                        });
                    }
                }
                for msg in messages {
                    if agentic_tx.try_send(msg).is_err() {
                        tracing::warn!("agentic channel full, message dropped");
                    }
                }
            }
            Some(ccline_msg) = ccline_rx.recv() => {
                if outbound_tx.try_send(ccline_msg).is_err() {
                    tracing::debug!("outbound channel full, ccline metrics dropped");
                }
            }
            Some(bridge_cmd) = bridge_cmd_rx.recv() => {
                match bridge_cmd {
                    BridgeCommand::Write { session_id, data } => {
                        let session_exists = session_manager.has_session(&session_id);
                        let result = session_manager.write_to(&session_id, &data);
                        if let Err(e) = result {
                            if session_exists {
                                tracing::warn!(
                                    session_id = %session_id,
                                    error = %e,
                                    "bridge: write I/O error"
                                );
                            } else {
                                let known: Vec<String> = session_manager
                                    .session_pids()
                                    .map(|(id, _)| format!("{}...", &id.to_string()[..8]))
                                    .collect();
                                tracing::warn!(
                                    session_id = %session_id,
                                    error = %e,
                                    known_sessions = ?known,
                                    "bridge: write failed, session not in agent SessionManager"
                                );
                                bridge::fan_out(
                                    bridge_senders,
                                    session_id,
                                    zremote_core::state::BrowserMessage::Error {
                                        message: format!("Session not found: {e}"),
                                    },
                                ).await;
                            }
                        }
                    }
                    BridgeCommand::Resize { session_id, cols, rows } => {
                        let session_exists = session_manager.has_session(&session_id);
                        let result = session_manager.resize(&session_id, cols, rows);
                        if let Err(e) = result {
                            if session_exists {
                                tracing::warn!(
                                    session_id = %session_id,
                                    cols = cols,
                                    rows = rows,
                                    error = %e,
                                    "bridge: resize I/O error"
                                );
                            } else {
                                tracing::warn!(
                                    session_id = %session_id,
                                    cols = cols,
                                    rows = rows,
                                    error = %e,
                                    "bridge: resize failed, session not in agent SessionManager"
                                );
                                bridge::fan_out(
                                    bridge_senders,
                                    session_id,
                                    zremote_core::state::BrowserMessage::Error {
                                        message: format!("Session not found: {e}"),
                                    },
                                ).await;
                            }
                        } else {
                            bridge::record_resize(bridge_scrollback, session_id, cols, rows).await;
                        }
                    }
                }
            }
            () = wait_for_shutdown(shutdown.clone()) => {
                tracing::info!(host_id = %host_id, "shutdown signal received, closing connection");
                break Ok(());
            }
        }
    };

    // Sessions are NOT cleaned up here — they survive across reconnects.
    // Final cleanup (detach/close) happens in run_agent() after the reconnect loop exits.

    // Stop the per-connection HooksServer (sender drop also works, but explicit is clearer)
    let _ = hooks_shutdown_tx.send(true);

    // Wait for sender task to finish and close the WebSocket cleanly
    match sender_handle.await {
        Ok(mut sink) => {
            let _ = sink.send(Message::Close(None)).await;
            let _ = sink.close().await;
        }
        Err(e) => {
            tracing::error!(error = %e, "sender task panicked");
        }
    }

    result
}

/// Decode PNG bytes, set the image on the system clipboard, and send Ctrl+V to the PTY.
fn set_clipboard_image_and_send_paste(
    session_manager: &mut SessionManager,
    session_id: uuid::Uuid,
    png_bytes: &[u8],
) -> Result<(), String> {
    let decoder = png::Decoder::new(png_bytes);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png decode: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png frame: {e}"))?;
    buf.truncate(info.buffer_size());

    let img_data = arboard::ImageData {
        width: info.width as usize,
        height: info.height as usize,
        bytes: std::borrow::Cow::Owned(buf),
    };

    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("clipboard init: {e}"))?;
    clipboard
        .set_image(img_data)
        .map_err(|e| format!("clipboard set: {e}"))?;

    session_manager
        .write_to(&session_id, &[0x16])
        .map_err(|e| format!("PTY write: {e}"))?;

    Ok(())
}

/// Handle a server message, dispatching session-related messages to the session manager.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn handle_server_message(
    msg: &ServerMessage,
    host_id: &HostId,
    session_manager: &mut SessionManager,
    agentic_manager: &mut AgenticLoopManager,
    project_scanner: &mut ProjectScanner,
    outbound_tx: &mpsc::Sender<AgentMessage>,
    agentic_tx: &mpsc::Sender<AgenticAgentMessage>,
    knowledge_tx: Option<&mpsc::Sender<KnowledgeServerMessage>>,
    session_mapper: &SessionMapper,
    bridge_senders: &BridgeSenders,
    bridge_scrollback: &bridge::BridgeScrollbackStore,
) {
    match msg {
        ServerMessage::HeartbeatAck { timestamp } => {
            tracing::debug!(host_id = %host_id, timestamp = %timestamp, "heartbeat acknowledged");
        }
        ServerMessage::SessionCreate {
            session_id,
            shell,
            cols,
            rows,
            working_dir,
            env,
            initial_command,
        } => {
            handle_session_create(
                session_manager,
                outbound_tx,
                *session_id,
                shell.as_deref(),
                *cols,
                *rows,
                working_dir.as_deref(),
                env.as_ref(),
                initial_command.as_deref(),
            )
            .await;
        }
        ServerMessage::SessionClose { session_id } => {
            // Clean up agentic loop if any
            if let Some(loop_ended) = agentic_manager.on_session_closed(session_id)
                && agentic_tx.try_send(loop_ended).is_err()
            {
                tracing::warn!("agentic channel full, LoopEnded dropped");
            }
            let exit_code = session_manager.close(session_id);
            tracing::info!(session_id = %session_id, exit_code = ?exit_code, "session closed by server");
            bridge::fan_out(
                bridge_senders,
                *session_id,
                zremote_core::state::BrowserMessage::SessionClosed { exit_code },
            )
            .await;
            bridge::remove_session(bridge_scrollback, session_id).await;
            if outbound_tx
                .try_send(AgentMessage::SessionClosed {
                    session_id: *session_id,
                    exit_code,
                })
                .is_err()
            {
                tracing::warn!("outbound channel full, message dropped");
            }
        }
        ServerMessage::TerminalInput { session_id, data } => {
            if let Err(e) = session_manager.write_to(session_id, data) {
                tracing::warn!(session_id = %session_id, error = %e, "failed to write to PTY");
            }
        }
        ServerMessage::TerminalImagePaste { session_id, data } => {
            let sid = *session_id;
            let png_bytes = data.clone();
            if let Err(e) = set_clipboard_image_and_send_paste(session_manager, sid, &png_bytes) {
                tracing::warn!(session_id = %sid, error = %e, "image paste failed");
            }
        }
        ServerMessage::TerminalResize {
            session_id,
            cols,
            rows,
        } => {
            if let Err(e) = session_manager.resize(session_id, *cols, *rows) {
                tracing::warn!(session_id = %session_id, error = %e, "failed to resize PTY");
            } else {
                bridge::record_resize(bridge_scrollback, *session_id, *cols, *rows).await;
            }
        }
        ServerMessage::Error { message } => {
            tracing::error!(host_id = %host_id, error = %message, "server error");
        }
        ServerMessage::RegisterAck { .. } => {
            tracing::warn!(host_id = %host_id, "received unexpected RegisterAck after registration");
        }
        ServerMessage::ProjectScan => {
            if project_scanner.should_debounce() {
                tracing::info!("project scan debounced, skipping");
                return;
            }
            let tx = outbound_tx.clone();
            let mut scanner = ProjectScanner::new();
            tokio::spawn(async move {
                match tokio::time::timeout(
                    Duration::from_secs(30),
                    tokio::task::spawn_blocking(move || scanner.scan()),
                )
                .await
                {
                    Ok(Ok(projects)) => {
                        if tx
                            .send(AgentMessage::ProjectList { projects })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, project list dropped");
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "project scan task panicked");
                    }
                    Err(_) => {
                        tracing::warn!("project scan timed out after 30s");
                    }
                }
            });
            // Update debounce tracking on the main scanner
            project_scanner.mark_scanned();
        }
        ServerMessage::ProjectRegister { path } => {
            tracing::info!(path = %path, "registering project path from server");
            if let Some(info) = ProjectScanner::detect_at(std::path::Path::new(path)) {
                if outbound_tx
                    .try_send(AgentMessage::ProjectDiscovered {
                        path: info.path,
                        name: info.name,
                        has_claude_config: info.has_claude_config,
                        has_zremote_config: info.has_zremote_config,
                        project_type: info.project_type,
                    })
                    .is_err()
                {
                    tracing::warn!("outbound channel full, ProjectDiscovered dropped");
                }
            } else {
                tracing::warn!(path = %path, "path is not a recognized project");
            }
        }
        ServerMessage::ProjectRemove { path } => {
            tracing::info!(path = %path, "project removal acknowledged");
        }
        ServerMessage::ListDirectory { request_id, path } => {
            let tx = outbound_tx.clone();
            let path = path.clone();
            let request_id = *request_id;
            tokio::task::spawn_blocking(move || {
                let entries_result =
                    crate::project::settings::list_directory(std::path::Path::new(&path));
                let msg = match entries_result {
                    Ok(entries) => AgentMessage::DirectoryListing {
                        request_id,
                        path,
                        entries,
                        error: None,
                    },
                    Err(e) => AgentMessage::DirectoryListing {
                        request_id,
                        path,
                        entries: vec![],
                        error: Some(e),
                    },
                };
                let _ = tx.blocking_send(msg);
            });
        }
        ServerMessage::ProjectGetSettings {
            request_id,
            project_path,
        } => {
            let tx = outbound_tx.clone();
            let project_path = project_path.clone();
            let request_id = *request_id;
            tokio::task::spawn_blocking(move || {
                let result =
                    crate::project::settings::read_settings(std::path::Path::new(&project_path));
                let msg = match result {
                    Ok(settings) => AgentMessage::ProjectSettingsResult {
                        request_id,
                        settings: settings.map(Box::new),
                        error: None,
                    },
                    Err(e) => AgentMessage::ProjectSettingsResult {
                        request_id,
                        settings: None,
                        error: Some(e),
                    },
                };
                let _ = tx.blocking_send(msg);
            });
        }
        ServerMessage::ProjectSaveSettings {
            request_id,
            project_path,
            settings,
        } => {
            let tx = outbound_tx.clone();
            let project_path = project_path.clone();
            let settings = settings.clone();
            let request_id = *request_id;
            tokio::task::spawn_blocking(move || {
                let result = crate::project::settings::write_settings(
                    std::path::Path::new(&project_path),
                    &settings,
                );
                let msg = match result {
                    Ok(()) => AgentMessage::ProjectSettingsSaved {
                        request_id,
                        error: None,
                    },
                    Err(e) => AgentMessage::ProjectSettingsSaved {
                        request_id,
                        error: Some(e),
                    },
                };
                let _ = tx.blocking_send(msg);
            });
        }
        ServerMessage::ProjectGitStatus { path } => {
            let tx = outbound_tx.clone();
            let path = path.clone();
            tokio::spawn(async move {
                let p = path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    GitInspector::inspect(std::path::Path::new(&p))
                })
                .await;
                match result {
                    Ok(Some((git_info, worktrees))) => {
                        if tx
                            .send(AgentMessage::GitStatusUpdate {
                                path,
                                git_info,
                                worktrees,
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, GitStatusUpdate dropped");
                        }
                    }
                    Ok(None) => {
                        tracing::warn!(path = %path, "path is not a git repository");
                    }
                    Err(e) => {
                        tracing::error!(path = %path, error = %e, "git inspect task panicked");
                    }
                }
            });
        }
        ServerMessage::WorktreeCreate {
            project_path,
            branch,
            path,
            new_branch,
        } => {
            let tx = outbound_tx.clone();
            let project_path = project_path.clone();
            let branch = branch.clone();
            let wt_path = path.clone();
            let new_branch = *new_branch;
            tokio::spawn(async move {
                // Check for custom create_command
                let wt_settings = read_worktree_settings_server(&project_path).await;

                if let Some(create_cmd) =
                    wt_settings.as_ref().and_then(|s| s.create_command.as_ref())
                {
                    // Custom command flow: run via execute_hook_async
                    let worktree_name = branch.replace('/', "-");
                    let cmd = create_cmd
                        .replace("{{project_path}}", &project_path)
                        .replace("{{branch}}", &branch)
                        .replace("{{worktree_name}}", &worktree_name);

                    let result = crate::project::hooks::execute_hook_async(
                        cmd,
                        std::path::PathBuf::from(&project_path),
                        vec![],
                        None,
                    )
                    .await;

                    if result.success {
                        // Inspect git to find the new worktree
                        let pp = project_path.clone();
                        let inspect_result = tokio::task::spawn_blocking(move || {
                            GitInspector::inspect(std::path::Path::new(&pp))
                        })
                        .await;

                        if let Ok(Some((_git_info, worktrees))) = inspect_result {
                            // Find a worktree matching the branch
                            if let Some(wt) = worktrees.iter().find(|w| {
                                w.branch.as_deref() == Some(&*branch)
                                    || w.path.ends_with(&worktree_name)
                            }) {
                                let worktree = zremote_protocol::project::WorktreeInfo {
                                    path: wt.path.clone(),
                                    branch: wt.branch.clone(),
                                    commit_hash: wt.commit_hash.clone(),
                                    is_detached: wt.is_detached,
                                    is_locked: wt.is_locked,
                                    is_dirty: wt.is_dirty,
                                    commit_message: wt.commit_message.clone(),
                                };

                                // Run on_create hook if configured
                                let hook_result = run_worktree_hook_server(
                                    &project_path,
                                    &worktree.path,
                                    worktree.branch.as_deref().unwrap_or_default(),
                                    |wt| wt.on_create.as_deref(),
                                )
                                .await;

                                if tx
                                    .send(AgentMessage::WorktreeCreated {
                                        project_path,
                                        worktree,
                                        hook_result,
                                    })
                                    .await
                                    .is_err()
                                {
                                    tracing::warn!(
                                        "outbound channel closed, WorktreeCreated dropped"
                                    );
                                }
                                return;
                            }
                        }

                        // Fallback: couldn't find worktree after custom command
                        if tx
                            .send(AgentMessage::WorktreeError {
                                project_path,
                                message:
                                    "custom create_command succeeded but worktree not found in git"
                                        .to_string(),
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeError dropped");
                        }
                    } else {
                        let msg = if result.output.is_empty() {
                            "custom create_command failed".to_string()
                        } else {
                            format!("custom create_command failed: {}", result.output)
                        };
                        if tx
                            .send(AgentMessage::WorktreeError {
                                project_path,
                                message: msg,
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeError dropped");
                        }
                    }
                    return;
                }

                // Default flow: existing GitInspector behavior
                let pp = project_path.clone();
                let b = branch.clone();
                let wp = wt_path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    GitInspector::create_worktree(
                        std::path::Path::new(&pp),
                        &b,
                        wp.as_ref().map(|p| std::path::Path::new(p.as_str())),
                        new_branch,
                    )
                })
                .await;
                match result {
                    Ok(Ok(worktree)) => {
                        // Run on_create hook if configured
                        let hook_result = run_worktree_hook_server(
                            &project_path,
                            &worktree.path,
                            worktree.branch.as_deref().unwrap_or_default(),
                            |wt| wt.on_create.as_deref(),
                        )
                        .await;

                        if tx
                            .send(AgentMessage::WorktreeCreated {
                                project_path,
                                worktree,
                                hook_result,
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeCreated dropped");
                        }
                    }
                    Ok(Err(msg)) => {
                        if tx
                            .send(AgentMessage::WorktreeError {
                                project_path,
                                message: msg,
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeError dropped");
                        }
                    }
                    Err(e) => {
                        if tx
                            .send(AgentMessage::WorktreeError {
                                project_path,
                                message: format!("worktree create task panicked: {e}"),
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeError dropped");
                        }
                    }
                }
            });
        }
        ServerMessage::WorktreeDelete {
            project_path,
            worktree_path,
            force,
        } => {
            let tx = outbound_tx.clone();
            let project_path = project_path.clone();
            let worktree_path = worktree_path.clone();
            let force = *force;
            tokio::spawn(async move {
                // Check for custom delete_command
                let wt_settings = read_worktree_settings_server(&project_path).await;

                if let Some(delete_cmd) =
                    wt_settings.as_ref().and_then(|s| s.delete_command.as_ref())
                {
                    // Run on_delete hook first
                    let hook_result =
                        run_worktree_hook_server(&project_path, &worktree_path, "", |wt| {
                            wt.on_delete.as_deref()
                        })
                        .await;

                    if let Some(ref hr) = hook_result {
                        let _ = tx
                            .send(AgentMessage::WorktreeHookResult {
                                project_path: project_path.clone(),
                                worktree_path: worktree_path.clone(),
                                hook_type: "on_delete".to_string(),
                                success: hr.success,
                                output: hr.output.clone(),
                                duration_ms: hr.duration_ms,
                            })
                            .await;
                    }

                    // Run custom delete command
                    let worktree_name = std::path::Path::new(&worktree_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();

                    let cmd = delete_cmd
                        .replace("{{project_path}}", &project_path)
                        .replace("{{worktree_path}}", &worktree_path)
                        .replace("{{worktree_name}}", &worktree_name)
                        .replace("{{branch}}", "");

                    let result = crate::project::hooks::execute_hook_async(
                        cmd,
                        std::path::PathBuf::from(&project_path),
                        vec![],
                        None,
                    )
                    .await;

                    if result.success {
                        if tx
                            .send(AgentMessage::WorktreeDeleted {
                                project_path,
                                worktree_path,
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeDeleted dropped");
                        }
                    } else {
                        let msg = if result.output.is_empty() {
                            "custom delete_command failed".to_string()
                        } else {
                            format!("custom delete_command failed: {}", result.output)
                        };
                        if tx
                            .send(AgentMessage::WorktreeError {
                                project_path,
                                message: msg,
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeError dropped");
                        }
                    }
                    return;
                }

                // Default flow: existing behavior
                // Run on_delete hook before removing worktree
                let hook_result =
                    run_worktree_hook_server(&project_path, &worktree_path, "", |wt| {
                        wt.on_delete.as_deref()
                    })
                    .await;

                if let Some(ref hr) = hook_result {
                    // Send hook result to server
                    let _ = tx
                        .send(AgentMessage::WorktreeHookResult {
                            project_path: project_path.clone(),
                            worktree_path: worktree_path.clone(),
                            hook_type: "on_delete".to_string(),
                            success: hr.success,
                            output: hr.output.clone(),
                            duration_ms: hr.duration_ms,
                        })
                        .await;
                }

                let pp = project_path.clone();
                let wp = worktree_path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    GitInspector::remove_worktree(
                        std::path::Path::new(&pp),
                        std::path::Path::new(&wp),
                        force,
                    )
                })
                .await;
                match result {
                    Ok(Ok(())) => {
                        if tx
                            .send(AgentMessage::WorktreeDeleted {
                                project_path,
                                worktree_path,
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeDeleted dropped");
                        }
                    }
                    Ok(Err(msg)) => {
                        if tx
                            .send(AgentMessage::WorktreeError {
                                project_path,
                                message: msg,
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeError dropped");
                        }
                    }
                    Err(e) => {
                        if tx
                            .send(AgentMessage::WorktreeError {
                                project_path,
                                message: format!("worktree delete task panicked: {e}"),
                            })
                            .await
                            .is_err()
                        {
                            tracing::warn!("outbound channel closed, WorktreeError dropped");
                        }
                    }
                }
            });
        }
        ServerMessage::ClaudeAction(claude_msg) => {
            handle_claude_server_message(claude_msg, session_manager, outbound_tx, session_mapper)
                .await;
        }
        ServerMessage::KnowledgeAction(knowledge_msg) => {
            if let Some(tx) = knowledge_tx {
                if tx.try_send(knowledge_msg.clone()).is_err() {
                    tracing::warn!("knowledge channel full, message dropped");
                }
            } else {
                tracing::warn!("received knowledge message but OpenViking is not configured");
                // Send error status back so the UI can display setup instructions
                if outbound_tx.try_send(AgentMessage::KnowledgeAction(
                    zremote_protocol::knowledge::KnowledgeAgentMessage::ServiceStatus {
                        status: zremote_protocol::knowledge::KnowledgeServiceStatus::Error,
                        version: None,
                        error: Some("OpenViking not enabled. Set OPENVIKING_ENABLED=true and restart agent.".to_string()),
                    },
                )).is_err() {
                    tracing::warn!("outbound channel full, knowledge error dropped");
                }
            }
        }
        ServerMessage::ResolveActionInputs {
            request_id,
            project_path,
            action_name,
        } => {
            let tx = outbound_tx.clone();
            let request_id = *request_id;
            let project_path = project_path.clone();
            let action_name = action_name.clone();
            tokio::spawn(async move {
                // Read settings
                let path = project_path.clone();
                let settings = match tokio::task::spawn_blocking(move || {
                    crate::project::settings::read_settings(std::path::Path::new(&path))
                })
                .await
                {
                    Ok(Ok(Some(settings))) => settings,
                    Ok(Ok(None)) => {
                        let _ = tx
                            .send(AgentMessage::ActionInputsResolved {
                                request_id,
                                inputs: vec![],
                                error: Some("no project settings found".to_string()),
                            })
                            .await;
                        return;
                    }
                    Ok(Err(e)) => {
                        let _ = tx
                            .send(AgentMessage::ActionInputsResolved {
                                request_id,
                                inputs: vec![],
                                error: Some(format!("failed to read settings: {e}")),
                            })
                            .await;
                        return;
                    }
                    Err(e) => {
                        let _ = tx
                            .send(AgentMessage::ActionInputsResolved {
                                request_id,
                                inputs: vec![],
                                error: Some(format!("task join error: {e}")),
                            })
                            .await;
                        return;
                    }
                };

                // Find action
                let action =
                    match crate::project::actions::find_action(&settings.actions, &action_name) {
                        Some(a) => a.clone(),
                        None => {
                            let _ = tx
                                .send(AgentMessage::ActionInputsResolved {
                                    request_id,
                                    inputs: vec![],
                                    error: Some(format!("action '{action_name}' not found")),
                                })
                                .await;
                            return;
                        }
                    };

                // Resolve inputs
                let inputs = crate::project::action_inputs::resolve_action_inputs(
                    &action,
                    std::path::Path::new(&project_path),
                    &settings.env,
                )
                .await;

                let _ = tx
                    .send(AgentMessage::ActionInputsResolved {
                        request_id,
                        inputs,
                        error: None,
                    })
                    .await;
            });
        }
    }
}

/// Handle a Claude server message: start sessions, discover sessions, etc.
#[allow(clippy::too_many_lines)]
async fn handle_claude_server_message(
    msg: &ClaudeServerMessage,
    session_manager: &mut SessionManager,
    outbound_tx: &mpsc::Sender<AgentMessage>,
    session_mapper: &SessionMapper,
) {
    match msg {
        ClaudeServerMessage::StartSession {
            session_id,
            claude_task_id,
            working_dir,
            model,
            initial_prompt,
            resume_cc_session_id,
            allowed_tools,
            skip_permissions,
            output_format,
            custom_flags,
            continue_last,
        } => {
            // Write large prompts to temp file to avoid PTY buffer overflow
            let prompt_file_path = initial_prompt
                .as_deref()
                .filter(|p| p.len() > 2048)
                .map(crate::claude::write_prompt_file);
            let prompt_file_path = match prompt_file_path {
                Some(Ok(path)) => Some(path),
                Some(Err(e)) => {
                    tracing::warn!(claude_task_id = %claude_task_id, error = %e, "failed to write prompt file");
                    let _ = outbound_tx.try_send(AgentMessage::ClaudeAction(
                        ClaudeAgentMessage::SessionStartFailed {
                            claude_task_id: *claude_task_id,
                            session_id: *session_id,
                            error: format!("failed to write prompt file: {e}"),
                        },
                    ));
                    return;
                }
                None => None,
            };

            // Build the claude CLI command
            let opts = crate::claude::CommandOptions {
                working_dir,
                model: model.as_deref(),
                initial_prompt: if prompt_file_path.is_some() {
                    None
                } else {
                    initial_prompt.as_deref()
                },
                prompt_file: prompt_file_path.as_deref(),
                resume_cc_session_id: resume_cc_session_id.as_deref(),
                continue_last: *continue_last,
                allowed_tools,
                skip_permissions: *skip_permissions,
                output_format: output_format.as_deref(),
                custom_flags: custom_flags.as_deref(),
            };
            let command = match crate::claude::CommandBuilder::build(&opts) {
                Ok(cmd) => cmd,
                Err(e) => {
                    tracing::warn!(claude_task_id = %claude_task_id, error = %e, "failed to build claude command");
                    let _ = outbound_tx.try_send(AgentMessage::ClaudeAction(
                        ClaudeAgentMessage::SessionStartFailed {
                            claude_task_id: *claude_task_id,
                            session_id: *session_id,
                            error: e,
                        },
                    ));
                    return;
                }
            };

            // Spawn PTY session using default shell
            let shell = default_shell();
            match session_manager
                .create(*session_id, shell, 120, 40, Some(working_dir), None)
                .await
            {
                Ok(pid) => {
                    tracing::info!(
                        session_id = %session_id,
                        claude_task_id = %claude_task_id,
                        pid = pid,
                        "Claude PTY session created"
                    );

                    // Notify that the PTY session is created
                    if outbound_tx
                        .try_send(AgentMessage::SessionCreated {
                            session_id: *session_id,
                            shell: shell.to_string(),
                            pid,
                        })
                        .is_err()
                    {
                        tracing::warn!("outbound channel full, SessionCreated dropped");
                    }

                    // Register this PTY session as a Claude task so hooks can
                    // capture the CC session ID and send SessionIdCaptured.
                    {
                        let mapper = session_mapper.clone();
                        let sid = *session_id;
                        let ctid = *claude_task_id;
                        tokio::spawn(async move {
                            mapper.register_claude_task(sid, ctid).await;
                        });
                    }

                    // Brief delay to let the shell initialize before writing the command
                    std::thread::sleep(std::time::Duration::from_millis(300));

                    // Write the claude command directly to the PTY stdin
                    if let Err(e) = session_manager.write_to(session_id, command.as_bytes()) {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "failed to write claude command to PTY"
                        );
                    }

                    // Notify that the Claude task session has started
                    if outbound_tx
                        .try_send(AgentMessage::ClaudeAction(
                            ClaudeAgentMessage::SessionStarted {
                                claude_task_id: *claude_task_id,
                                session_id: *session_id,
                            },
                        ))
                        .is_err()
                    {
                        tracing::warn!("outbound channel full, SessionStarted dropped");
                    }
                }
                Err(e) => {
                    tracing::error!(
                        session_id = %session_id,
                        claude_task_id = %claude_task_id,
                        error = %e,
                        "failed to create PTY session for Claude task"
                    );
                    let _ = outbound_tx.try_send(AgentMessage::ClaudeAction(
                        ClaudeAgentMessage::SessionStartFailed {
                            claude_task_id: *claude_task_id,
                            session_id: *session_id,
                            error: format!("failed to spawn PTY: {e}"),
                        },
                    ));
                }
            }
        }
        ClaudeServerMessage::DiscoverSessions { project_path } => {
            let path = project_path.clone();
            let tx = outbound_tx.clone();
            tokio::spawn(async move {
                let discover_path = path.clone();
                let sessions = tokio::task::spawn_blocking(move || {
                    crate::claude::SessionScanner::discover(&discover_path)
                })
                .await
                .unwrap_or_default();
                let _ = tx
                    .send(AgentMessage::ClaudeAction(
                        ClaudeAgentMessage::SessionsDiscovered {
                            project_path: path,
                            sessions,
                        },
                    ))
                    .await;
            });
        }
    }
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
    use uuid::Uuid;
    use zremote_protocol::ServerMessage;

    /// Helper to create test fixtures for `handle_server_message`.
    #[allow(clippy::type_complexity)]
    fn make_test_context() -> (
        SessionManager,
        AgenticLoopManager,
        ProjectScanner,
        mpsc::Sender<AgentMessage>,
        mpsc::Receiver<AgentMessage>,
        mpsc::Sender<AgenticAgentMessage>,
        mpsc::Receiver<AgenticAgentMessage>,
        Option<mpsc::Sender<KnowledgeServerMessage>>,
        SessionMapper,
        BridgeSenders,
        bridge::BridgeScrollbackStore,
    ) {
        let (pty_tx, _pty_rx) = mpsc::channel(16);
        let session_manager = SessionManager::new(pty_tx, crate::config::PersistenceBackend::None);
        let agentic_manager = AgenticLoopManager::new();
        let project_scanner = ProjectScanner::new();
        let (outbound_tx, outbound_rx) = mpsc::channel(16);
        let (agentic_tx, agentic_rx) = mpsc::channel(16);
        let session_mapper = SessionMapper::new();
        let bridge_senders: BridgeSenders =
            Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let bridge_scrollback: bridge::BridgeScrollbackStore =
            Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        (
            session_manager,
            agentic_manager,
            project_scanner,
            outbound_tx,
            outbound_rx,
            agentic_tx,
            agentic_rx,
            None,
            session_mapper,
            bridge_senders,
            bridge_scrollback,
        )
    }

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

    #[tokio::test]
    async fn handle_server_message_heartbeat_ack() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb) =
            make_test_context();
        let msg = ServerMessage::HeartbeatAck {
            timestamp: Utc::now(),
        };
        handle_server_message(
            &msg,
            &host_id,
            &mut sm,
            &mut am,
            &mut ps,
            &otx,
            &atx,
            ktx.as_ref(),
            &mapper,
            &bs,
            &bsb,
        )
        .await;
    }

    #[tokio::test]
    async fn handle_server_message_error() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb) =
            make_test_context();
        let msg = ServerMessage::Error {
            message: "test error".to_string(),
        };
        handle_server_message(
            &msg,
            &host_id,
            &mut sm,
            &mut am,
            &mut ps,
            &otx,
            &atx,
            ktx.as_ref(),
            &mapper,
            &bs,
            &bsb,
        )
        .await;
    }

    #[tokio::test]
    async fn handle_server_message_unexpected_register_ack() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb) =
            make_test_context();
        let msg = ServerMessage::RegisterAck {
            host_id: Uuid::new_v4(),
        };
        handle_server_message(
            &msg,
            &host_id,
            &mut sm,
            &mut am,
            &mut ps,
            &otx,
            &atx,
            ktx.as_ref(),
            &mapper,
            &bs,
            &bsb,
        )
        .await;
    }

    #[tokio::test]
    async fn handle_session_close_nonexistent_session() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb) =
            make_test_context();
        let session_id = Uuid::new_v4();
        let msg = ServerMessage::SessionClose { session_id };
        handle_server_message(
            &msg,
            &host_id,
            &mut sm,
            &mut am,
            &mut ps,
            &otx,
            &atx,
            ktx.as_ref(),
            &mapper,
            &bs,
            &bsb,
        )
        .await;

        // Should send SessionClosed with exit_code = None
        let sent = orx.try_recv().unwrap();
        match sent {
            AgentMessage::SessionClosed {
                session_id: sid,
                exit_code,
            } => {
                assert_eq!(sid, session_id);
                assert_eq!(exit_code, None);
            }
            other => panic!("expected SessionClosed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_terminal_input_nonexistent_session() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb) =
            make_test_context();
        let msg = ServerMessage::TerminalInput {
            session_id: Uuid::new_v4(),
            data: vec![0x41],
        };
        handle_server_message(
            &msg,
            &host_id,
            &mut sm,
            &mut am,
            &mut ps,
            &otx,
            &atx,
            ktx.as_ref(),
            &mapper,
            &bs,
            &bsb,
        )
        .await;
    }

    #[tokio::test]
    async fn handle_terminal_resize_nonexistent_session() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb) =
            make_test_context();
        let session_id = Uuid::new_v4();
        let msg = ServerMessage::TerminalResize {
            session_id,
            cols: 120,
            rows: 40,
        };
        handle_server_message(
            &msg,
            &host_id,
            &mut sm,
            &mut am,
            &mut ps,
            &otx,
            &atx,
            ktx.as_ref(),
            &mapper,
            &bs,
            &bsb,
        )
        .await;

        // Resize for nonexistent session should NOT create a phantom scrollback entry.
        let guard = bsb.read().await;
        assert!(
            guard.get(&session_id).is_none(),
            "scrollback entry should not exist for unknown session"
        );
    }

    #[tokio::test]
    async fn wait_for_shutdown_returns_immediately_if_already_true() {
        let (tx, rx) = tokio::sync::watch::channel(true);
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
            openviking_enabled: false,
            openviking_binary: "openviking".to_string(),
            openviking_port: 1933,
            openviking_config_dir: std::path::PathBuf::from("/tmp/ov"),
            openviking_api_key: None,
        };
        let result = connect(&config).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConnectionError::Connect(_)));
    }
}
