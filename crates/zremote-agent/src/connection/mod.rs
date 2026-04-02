mod dispatch;
mod heartbeat;
mod registration;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use zremote_protocol::{AgentMessage, AgenticAgentMessage, SessionId};

use crate::agentic::analyzer::{AnalyzerEvent, AnalyzerPhase, OutputAnalyzer};
use crate::agentic::manager::AgenticLoopManager;
use crate::bridge::BridgeCommand;
use crate::bridge::{self, BridgeSenders};
use crate::config::AgentConfig;
use crate::hooks::mapper::SessionMapper;
use crate::hooks::server::HooksServer;
use crate::knowledge::KnowledgeManager;
use crate::knowledge::context_delivery::{
    ContextAssembler, ContextChangeEvent, ContextTrigger, DeliveryCoordinator, PtyTransport,
    SessionWriteRequest, SessionWriterHandle,
};
use crate::knowledge::file_watcher::ProjectFileWatcher;
use crate::project::ProjectScanner;
use crate::session::SessionManager;
use zremote_protocol::knowledge::KnowledgeServerMessage;

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
    use futures_util::SinkExt;
    let json = serde_json::to_string(msg).map_err(ConnectionError::Serialize)?;
    ws.send(Message::Text(json.into()))
        .await
        .map_err(ConnectionError::Send)
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

/// Process an analyzer event, mapping it to agentic protocol messages.
async fn handle_analyzer_event(
    session_id: SessionId,
    event: &AnalyzerEvent,
    agentic_tx: &mpsc::Sender<AgenticAgentMessage>,
    agentic_manager: &AgenticLoopManager,
    session_mapper: &SessionMapper,
    delivery_coordinator: &Arc<tokio::sync::Mutex<DeliveryCoordinator>>,
    session_manager: &mut SessionManager,
) {
    match event {
        AnalyzerEvent::AgentDetected { name, .. } => {
            tracing::info!(session = %session_id, agent = %name,
                "agent detected from output (loop created by process detector)");
        }
        AnalyzerEvent::PhaseChanged(phase) => {
            // Suppress analyzer phase updates when hooks are actively providing state
            if session_mapper.has_recent_hook_activity(&session_id, Duration::from_secs(5)) {
                return;
            }
            let status = match phase {
                AnalyzerPhase::Busy => zremote_protocol::AgenticStatus::Working,
                AnalyzerPhase::Idle | AnalyzerPhase::NeedsInput => {
                    zremote_protocol::AgenticStatus::WaitingForInput
                }
                _ => return,
            };
            // Check for deferred context nudges on idle transitions
            if matches!(phase, AnalyzerPhase::Idle | AnalyzerPhase::NeedsInput)
                && let Some(content) = delivery_coordinator.lock().await.on_phase_idle(&session_id)
            {
                tracing::info!(
                    session = %session_id,
                    content_len = content.len(),
                    "delivering deferred context nudge via PTY write"
                );
                if let Err(e) = session_manager.write_to(&session_id, content.as_bytes()) {
                    tracing::warn!(
                        session = %session_id,
                        error = %e,
                        "failed to deliver context nudge"
                    );
                }
            }
            if let Some(loop_id) = agentic_manager.loop_id_for_session(&session_id) {
                let _ = agentic_tx.try_send(AgenticAgentMessage::LoopStateUpdate {
                    loop_id,
                    status,
                    task_name: None,
                    prompt_message: None,
                    permission_mode: None,
                });
            }
        }
        AnalyzerEvent::TokenUpdate { .. } => {
            // Token metrics are sent separately using accumulated totals from the analyzer.
            // See the call site in the PTY output handler.
        }
        AnalyzerEvent::ToolCall { tool, args } => {
            tracing::debug!(session = %session_id, %tool, %args, "tool call detected");
        }
        AnalyzerEvent::CwdChanged(path) => {
            tracing::debug!(session = %session_id, cwd = %path, "working directory changed");
        }
        AnalyzerEvent::NodeCompleted(node) => {
            let loop_id = agentic_manager.loop_id_for_session(&session_id);
            let _ = agentic_tx.try_send(AgenticAgentMessage::ExecutionNode {
                session_id,
                loop_id,
                timestamp: node.timestamp,
                kind: node.kind.clone(),
                input: node.input.clone(),
                output_summary: node.output_summary.clone(),
                exit_code: node.exit_code,
                working_dir: node.working_dir.clone(),
                duration_ms: node.duration_ms,
            });
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

    let host_id = registration::register(&mut ws, config, supports_persistence).await?;

    // Write host_id for GUI bridge discovery (skip bridge for non-local sessions)
    bridge::write_host_id_file(&host_id).await;

    // Split the WebSocket for concurrent read/write
    let (ws_sink, mut ws_stream) = ws.split();

    // Channel for outbound agent messages (from main loop + PTY output)
    let (outbound_tx, outbound_rx) = mpsc::channel::<AgentMessage>(2048);

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

    // Run initial project scan in background and set up file watchers
    // for project directories. We use a oneshot channel to get project
    // paths back so we can watch them.
    let (project_paths_tx, project_paths_rx) = tokio::sync::oneshot::channel::<Vec<String>>();
    {
        let tx = outbound_tx.clone();
        let mut scanner = ProjectScanner::new();
        tokio::spawn(async move {
            let projects = tokio::task::spawn_blocking(move || scanner.scan())
                .await
                .unwrap_or_default();
            // Send project paths for file watching before forwarding to server
            let paths: Vec<String> = projects.iter().map(|p| p.path.clone()).collect();
            let _ = project_paths_tx.send(paths);
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
    let (agentic_tx, agentic_rx) = mpsc::channel::<AgenticAgentMessage>(64);

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

    // Shared delivery coordinator -- used by both the connection loop and
    // the hooks sidecar so that nudges queued from ContextChangeEvents are
    // visible to the HookContextProvider that builds `additionalContext`.
    let delivery_coordinator = Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new()));

    // Hooks sidecar (CC hook integration)
    // Use a per-connection shutdown signal so the HooksServer stops when this
    // connection ends (sender is dropped on function exit -> server stops).
    let (hooks_shutdown_tx, hooks_shutdown_rx) = tokio::sync::watch::channel(false);
    let hooks_server = HooksServer::new(
        agentic_tx.clone(),
        session_mapper.clone(),
        outbound_tx.clone(),
        sent_cc_session_ids.clone(),
        delivery_coordinator.clone(),
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

    // Channel for context change events from KnowledgeManager -> connection loop
    let (context_change_tx, mut context_change_rx) = mpsc::channel::<ContextChangeEvent>(64);

    // File watcher for project files (CLAUDE.md, Cargo.toml, etc.)
    let mut project_file_watcher = match ProjectFileWatcher::new(context_change_tx.clone()) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::debug!(error = %e, "project file watcher unavailable (non-fatal)");
            None
        }
    };

    // Channel for session write requests from DeliveryCoordinator -> connection loop
    let (session_write_tx, mut session_write_rx) = mpsc::channel::<SessionWriteRequest>(64);
    let session_writer_handle = SessionWriterHandle::new(session_write_tx);

    // PTY transport for context delivery (file-based injection)
    let _pty_transport = match PtyTransport::new(session_writer_handle) {
        Ok(transport) => Some(transport),
        Err(e) => {
            tracing::warn!(error = %e, "failed to create PTY transport for context delivery");
            None
        }
    };

    let mut knowledge_manager = if config.openviking_enabled {
        let mut mgr = KnowledgeManager::new(
            config.openviking_binary.clone(),
            config.openviking_port,
            config.openviking_config_dir.clone(),
            config.openviking_api_key.clone(),
            outbound_tx.clone(),
        );
        mgr.set_context_change_tx(context_change_tx);
        Some(mgr)
    } else {
        None
    };

    // Spawn sender task: drains outbound channel + agentic channel + heartbeats -> WS sink
    let sender_shutdown = shutdown.clone();
    let sender_handle = tokio::spawn(heartbeat::run_sender(
        ws_sink,
        sender_shutdown,
        outbound_rx,
        agentic_rx,
    ));

    // Periodic agentic tool detection
    let mut agentic_check_interval = tokio::time::interval(AGENTIC_CHECK_INTERVAL);
    // Skip the first immediate tick
    agentic_check_interval.tick().await;

    // Per-session output analyzers — seed with existing sessions (survived reconnect)
    let default_cwd = dirs::home_dir().map(|p| p.to_string_lossy().to_string());
    let mut session_analyzers: HashMap<SessionId, OutputAnalyzer> = session_manager
        .session_pids()
        .map(|(sid, _)| (sid, OutputAnalyzer::with_initial_cwd(default_cwd.clone())))
        .collect();
    let mut silence_check_interval = tokio::time::interval(Duration::from_secs(1));
    silence_check_interval.tick().await; // skip first immediate tick

    // delivery_coordinator is created above (shared with HooksServer)

    // Set up file watchers for initial project paths (non-blocking)
    if let Ok(paths) = project_paths_rx.await
        && let Some(ref mut watcher) = project_file_watcher
    {
        for path in &paths {
            if let Err(e) = watcher.watch_project(std::path::Path::new(path)) {
                tracing::debug!(path = %path, error = %e, "failed to watch project dir");
            }
        }
        if watcher.watched_count() > 0 {
            tracing::info!(
                count = watcher.watched_count(),
                "watching project files for changes"
            );
        }
    }

    // Main message loop
    let result = loop {
        tokio::select! {
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<zremote_protocol::ServerMessage>(&text) {
                            Ok(server_msg) => {
                                dispatch::handle_server_message(
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
                                    &mut session_analyzers,
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
                    session_analyzers.remove(&session_id);
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
                    // Feed through per-session analyzer
                    if let Some(analyzer) = session_analyzers.get_mut(&session_id) {
                        let events = analyzer.process_output(&data);
                        let has_token_update = events.iter().any(|e| matches!(e, AnalyzerEvent::TokenUpdate { .. }));
                        for event in &events {
                            handle_analyzer_event(
                                session_id, event, &agentic_tx, agentic_manager, session_mapper,
                                &delivery_coordinator, session_manager,
                            ).await;
                        }
                        // Send accumulated token totals (not raw deltas) for DB replacement
                        if has_token_update
                            && let Some(loop_id) = agentic_manager.loop_id_for_session(&session_id)
                        {
                                let metrics = analyzer.metrics();
                                let total_input: u64 = metrics.token_usage.values().map(|t| t.input_tokens).sum();
                                let total_output: u64 = metrics.token_usage.values().map(|t| t.output_tokens).sum();
                                let total_cost: Option<f64> = {
                                    let costs: Vec<f64> = metrics.token_usage.values().filter_map(|t| t.cost_usd).collect();
                                    if costs.is_empty() { None } else { Some(costs.iter().sum()) }
                                };
                                let _ = agentic_tx.try_send(AgenticAgentMessage::LoopMetricsUpdate {
                                    loop_id,
                                    input_tokens: total_input,
                                    output_tokens: total_output,
                                    cost_usd: total_cost,
                                });
                        }
                    }
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
                    session_analyzers.remove(&session_id);
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
            _ = silence_check_interval.tick() => {
                for (session_id, analyzer) in &mut session_analyzers {
                    if let Some(last) = analyzer.last_output_at()
                        && last.elapsed() > Duration::from_secs(3)
                        && let Some(event) = analyzer.check_silence()
                    {
                        handle_analyzer_event(*session_id, &event, &agentic_tx, agentic_manager, session_mapper, &delivery_coordinator, session_manager).await;
                    }
                }
            }
            Some(ctx_event) = context_change_rx.recv() => {
                match ctx_event {
                    ContextChangeEvent::MemoriesExtracted { loop_id, memories, project_path } => {
                        if let Some(session_id) = agentic_manager.session_id_for_loop(&loop_id) {
                            let project_name = project_path
                                .rsplit('/')
                                .next()
                                .unwrap_or(&project_path)
                                .to_string();
                            let context = ContextAssembler::assemble(
                                &project_name,
                                &project_path,
                                "unknown",
                                None,
                                &[],
                                &memories,
                                &[],
                                ContextTrigger::MemoryExtracted {
                                    loop_id,
                                    count: memories.len(),
                                },
                            );
                            delivery_coordinator.lock().await.on_context_changed(session_id, context);
                            tracing::info!(
                                session = %session_id,
                                loop_id = %loop_id,
                                memory_count = memories.len(),
                                "context change queued for delivery"
                            );
                        } else {
                            tracing::debug!(
                                loop_id = %loop_id,
                                "context change event for unknown loop, ignoring"
                            );
                        }
                    }
                    ContextChangeEvent::ProjectFileChanged { project_path, changed_file } => {
                        tracing::info!(
                            project = %project_path,
                            file = %changed_file,
                            "project file changed, triggering conventions update"
                        );
                        let detected = ProjectScanner::detect_at(std::path::Path::new(&project_path));
                        let project_type = detected.as_ref().map_or("unknown", |p| &p.project_type);
                        for session_id in agentic_manager.session_ids_for_project(&project_path) {
                            let context = ContextAssembler::assemble(
                                project_path.rsplit('/').next().unwrap_or(&project_path),
                                &project_path,
                                project_type,
                                None,
                                &[],
                                &[],
                                &[],
                                ContextTrigger::ConventionsUpdated {
                                    project_path: project_path.clone(),
                                },
                            );
                            delivery_coordinator.lock().await.on_context_changed(session_id, context);
                        }
                    }
                }
            }
            Some(write_req) = session_write_rx.recv() => {
                if let Err(e) = session_manager.write_to(&write_req.session_id, &write_req.data) {
                    tracing::warn!(
                        session_id = %write_req.session_id,
                        error = %e,
                        "failed to write session data from delivery coordinator"
                    );
                } else if let Some(analyzer) = session_analyzers.get_mut(&write_req.session_id) {
                    analyzer.mark_input_sent();
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
                        if result.is_ok()
                            && let Some(analyzer) = session_analyzers.get_mut(&session_id)
                        {
                            analyzer.mark_input_sent();
                        }
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

    // Sessions are NOT cleaned up here -- they survive across reconnects.
    // Final cleanup (detach/close) happens in run_agent() after the reconnect loop exits.

    // Stop the per-connection HooksServer (sender drop also works, but explicit is clearer)
    let _ = hooks_shutdown_tx.send(true);

    // Wait for sender task to finish and close the WebSocket cleanly
    match sender_handle.await {
        Ok(mut sink) => {
            use futures_util::SinkExt;
            let _ = sink.send(Message::Close(None)).await;
            let _ = sink.close().await;
        }
        Err(e) => {
            tracing::error!(error = %e, "sender task panicked");
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

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
