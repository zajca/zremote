use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use myremote_protocol::claude::{ClaudeAgentMessage, ClaudeServerMessage};
use myremote_protocol::knowledge::KnowledgeServerMessage;
use myremote_protocol::{
    AgentMessage, AgenticAgentMessage, AgenticServerMessage, HostId, ServerMessage, SessionId,
    UserAction,
};
use tokio::sync::mpsc;
use tokio::time::{interval, timeout};
use tokio_tungstenite::tungstenite::Message;

use crate::agentic::manager::AgenticLoopManager;
use crate::config::AgentConfig;
use crate::hooks::mapper::SessionMapper;
use crate::hooks::permission::{PermissionDecision, PermissionManager};
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
const AGENTIC_CHECK_INTERVAL: Duration = Duration::from_secs(3);

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
    use_tmux: bool,
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
        supports_persistent_sessions: use_tmux,
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

/// Handle a `SessionCreate` message: spawn a PTY and send `SessionCreated` or `Error`.
fn handle_session_create(
    session_manager: &mut SessionManager,
    outbound_tx: &mpsc::Sender<AgentMessage>,
    session_id: SessionId,
    shell: Option<&str>,
    cols: u16,
    rows: u16,
    working_dir: Option<&str>,
) {
    let shell = shell.unwrap_or(default_shell());
    match session_manager.create(session_id, shell, cols, rows, working_dir) {
        Ok(pid) => {
            tracing::info!(session_id = %session_id, pid = pid, shell = shell, "PTY session created");
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
#[allow(clippy::too_many_lines)]
pub async fn run_connection(
    config: &AgentConfig,
    shutdown: tokio::sync::watch::Receiver<bool>,
    use_tmux: bool,
) -> Result<(), ConnectionError> {
    tracing::info!(server_url = %config.server_url, "connecting to server");

    let mut ws = connect(config).await?;
    tracing::info!("WebSocket connection established");

    let host_id = register(&mut ws, config, use_tmux).await?;

    // Split the WebSocket for concurrent read/write
    let (mut ws_sink, mut ws_stream) = ws.split();

    // Channel for outbound agent messages (from main loop + PTY output)
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<AgentMessage>(256);

    // Channel for PTY output data
    let (pty_output_tx, mut pty_output_rx) = mpsc::channel::<(SessionId, Vec<u8>)>(256);

    // Session manager
    let mut session_manager = SessionManager::new(pty_output_tx, use_tmux);

    // Discover and report recovered tmux sessions
    {
        let recovered = session_manager.discover_existing();
        if !recovered.is_empty() {
            let sessions: Vec<myremote_protocol::RecoveredSession> = recovered
                .iter()
                .map(
                    |(session_id, shell, pid)| myremote_protocol::RecoveredSession {
                        session_id: *session_id,
                        shell: shell.clone(),
                        pid: *pid,
                    },
                )
                .collect();
            tracing::info!(count = sessions.len(), "reporting recovered tmux sessions");
            if outbound_tx
                .try_send(AgentMessage::SessionsRecovered { sessions })
                .is_err()
            {
                tracing::warn!("outbound channel full, SessionsRecovered dropped");
            }
        }
    }

    // Agentic loop manager
    let mut agentic_manager = AgenticLoopManager::new();

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

    // Hooks sidecar (CC hook integration)
    let session_mapper = SessionMapper::new();
    let permission_manager = Arc::new(PermissionManager::new());
    let hooks_server = HooksServer::new(
        agentic_tx.clone(),
        session_mapper.clone(),
        permission_manager.clone(),
        outbound_tx.clone(),
    );
    match hooks_server.start(shutdown.clone()).await {
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
            tokio::select! {
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
                () = wait_for_shutdown(sender_shutdown.clone()) => {
                    tracing::debug!("sender task shutting down");
                    return ws_sink;
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
                                    &mut session_manager,
                                    &mut agentic_manager,
                                    &mut project_scanner,
                                    &outbound_tx,
                                    &agentic_tx,
                                    knowledge_sender.as_ref(),
                                    &permission_manager,
                                    &session_mapper,
                                );
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
            Some((session_id, data)) = pty_output_rx.recv() => {
                if data.is_empty() {
                    // Session ended (EOF from PTY reader)
                    if let Some(loop_ended) = agentic_manager.on_session_closed(&session_id)
                        && agentic_tx.try_send(loop_ended).is_err()
                    {
                        tracing::warn!("agentic channel full, LoopEnded dropped");
                    }
                    let exit_code = session_manager.close(&session_id);
                    tracing::info!(session_id = %session_id, exit_code = ?exit_code, "PTY session ended");
                    if outbound_tx.try_send(AgentMessage::SessionClosed {
                        session_id,
                        exit_code,
                    }).is_err() {
                        tracing::warn!("outbound channel full, message dropped");
                    }
                } else {
                    // Pass output through agentic manager for parsing
                    let agentic_msgs = agentic_manager.process_output(&session_id, &data);
                    for msg in agentic_msgs {
                        if agentic_tx.try_send(msg).is_err() {
                            tracing::warn!("agentic channel full, message dropped");
                        }
                    }

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
            () = wait_for_shutdown(shutdown.clone()) => {
                tracing::info!(host_id = %host_id, "shutdown signal received, closing connection");
                break Ok(());
            }
        }
    };

    // Clean up sessions: detach tmux (they survive), kill PTY
    if session_manager.use_tmux() {
        session_manager.detach_all();
    } else {
        session_manager.close_all();
    }

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

/// Handle a server message, dispatching session-related messages to the session manager.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn handle_server_message(
    msg: &ServerMessage,
    host_id: &HostId,
    session_manager: &mut SessionManager,
    agentic_manager: &mut AgenticLoopManager,
    project_scanner: &mut ProjectScanner,
    outbound_tx: &mpsc::Sender<AgentMessage>,
    agentic_tx: &mpsc::Sender<AgenticAgentMessage>,
    knowledge_tx: Option<&mpsc::Sender<KnowledgeServerMessage>>,
    permission_manager: &Arc<PermissionManager>,
    session_mapper: &SessionMapper,
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
        } => {
            handle_session_create(
                session_manager,
                outbound_tx,
                *session_id,
                shell.as_deref(),
                *cols,
                *rows,
                working_dir.as_deref(),
            );
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
        ServerMessage::TerminalResize {
            session_id,
            cols,
            rows,
        } => {
            if let Err(e) = session_manager.resize(session_id, *cols, *rows) {
                tracing::warn!(session_id = %session_id, error = %e, "failed to resize PTY");
            }
        }
        ServerMessage::Error { message } => {
            tracing::error!(host_id = %host_id, error = %message, "server error");
        }
        ServerMessage::RegisterAck { .. } => {
            tracing::warn!(host_id = %host_id, "received unexpected RegisterAck after registration");
        }
        ServerMessage::AgenticAction(agentic_msg) => {
            handle_agentic_server_message(
                agentic_msg,
                session_manager,
                agentic_manager,
                permission_manager,
            );
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
                        if tx
                            .send(AgentMessage::WorktreeCreated {
                                project_path,
                                worktree,
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
            handle_claude_server_message(claude_msg, session_manager, outbound_tx, session_mapper);
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
                    myremote_protocol::knowledge::KnowledgeAgentMessage::ServiceStatus {
                        status: myremote_protocol::knowledge::KnowledgeServiceStatus::Error,
                        version: None,
                        error: Some("OpenViking not enabled. Set OPENVIKING_ENABLED=true and restart agent.".to_string()),
                    },
                )).is_err() {
                    tracing::warn!("outbound channel full, knowledge error dropped");
                }
            }
        }
    }
}

/// Handle a Claude server message: start sessions, discover sessions, etc.
#[allow(clippy::too_many_lines)]
fn handle_claude_server_message(
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
            // Build the claude CLI command
            let opts = crate::claude::CommandOptions {
                working_dir,
                model: model.as_deref(),
                initial_prompt: initial_prompt.as_deref(),
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
            match session_manager.create(*session_id, shell, 120, 40, Some(working_dir)) {
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

                    // Write the claude command directly to the PTY stdin.
                    // The shell buffers stdin and will execute the command once it initializes.
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

/// Handle an agentic server message (user actions forwarded from the server).
fn handle_agentic_server_message(
    msg: &AgenticServerMessage,
    session_manager: &mut SessionManager,
    agentic_manager: &mut AgenticLoopManager,
    permission_manager: &Arc<PermissionManager>,
) {
    match msg {
        AgenticServerMessage::UserAction {
            loop_id,
            action,
            payload,
        } => {
            // Resolve any pending permission requests for this loop
            if *action == UserAction::Approve || *action == UserAction::Reject {
                let pm = permission_manager.clone();
                let lid = *loop_id;
                let decision = if *action == UserAction::Approve {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny
                };
                tokio::spawn(async move {
                    pm.resolve_any_pending(lid, decision).await;
                });
            }

            if let Some((session_id, bytes)) =
                agentic_manager.handle_user_action(loop_id, *action, payload.as_deref())
            {
                if let Err(e) = session_manager.write_to(&session_id, &bytes) {
                    tracing::warn!(
                        loop_id = %loop_id,
                        session_id = %session_id,
                        error = %e,
                        "failed to write agentic action to PTY"
                    );
                }
            } else {
                tracing::warn!(loop_id = %loop_id, "user action for unknown loop");
            }
        }
        AgenticServerMessage::PermissionRulesUpdate { rules } => {
            tracing::info!(count = rules.len(), "received permission rules update");
            let pm = permission_manager.clone();
            let rules = rules.clone();
            tokio::spawn(async move {
                pm.update_rules(rules).await;
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
    use myremote_protocol::ServerMessage;
    use uuid::Uuid;

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
        Arc<PermissionManager>,
        SessionMapper,
    ) {
        let (pty_tx, _pty_rx) = mpsc::channel(16);
        let session_manager = SessionManager::new(pty_tx, false);
        let agentic_manager = AgenticLoopManager::new();
        let project_scanner = ProjectScanner::new();
        let (outbound_tx, outbound_rx) = mpsc::channel(16);
        let (agentic_tx, agentic_rx) = mpsc::channel(16);
        let permission_manager = Arc::new(PermissionManager::new());
        let session_mapper = SessionMapper::new();
        (
            session_manager,
            agentic_manager,
            project_scanner,
            outbound_tx,
            outbound_rx,
            agentic_tx,
            agentic_rx,
            None,
            permission_manager,
            session_mapper,
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
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, pm, mapper) = make_test_context();
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
            &pm,
            &mapper,
        );
    }

    #[tokio::test]
    async fn handle_server_message_error() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, pm, mapper) = make_test_context();
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
            &pm,
            &mapper,
        );
    }

    #[tokio::test]
    async fn handle_server_message_unexpected_register_ack() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, pm, mapper) = make_test_context();
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
            &pm,
            &mapper,
        );
    }

    #[tokio::test]
    async fn handle_session_close_nonexistent_session() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, pm, mapper) =
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
            &pm,
            &mapper,
        );

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
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, pm, mapper) = make_test_context();
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
            &pm,
            &mapper,
        );
    }

    #[tokio::test]
    async fn handle_terminal_resize_nonexistent_session() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, pm, mapper) = make_test_context();
        let msg = ServerMessage::TerminalResize {
            session_id: Uuid::new_v4(),
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
            &pm,
            &mapper,
        );
    }

    #[tokio::test]
    async fn handle_agentic_user_action_unknown_loop() {
        let (mut sm, mut am, _ps, _otx, _orx, _atx, _arx, _ktx, pm, _mapper) = make_test_context();
        let msg = AgenticServerMessage::UserAction {
            loop_id: Uuid::new_v4(),
            action: myremote_protocol::UserAction::Approve,
            payload: None,
        };
        // Should not panic
        handle_agentic_server_message(&msg, &mut sm, &mut am, &pm);
    }

    #[tokio::test]
    async fn handle_agentic_permission_rules_update() {
        let (mut sm, mut am, _ps, _otx, _orx, _atx, _arx, _ktx, pm, _mapper) = make_test_context();
        let msg = AgenticServerMessage::PermissionRulesUpdate { rules: vec![] };
        handle_agentic_server_message(&msg, &mut sm, &mut am, &pm);
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
