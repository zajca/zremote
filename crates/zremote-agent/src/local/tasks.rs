use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zremote_protocol::{AgentMessage, AgenticAgentMessage};

use super::AGENTIC_CHECK_INTERVAL;
use super::state::LocalAppState;
use crate::agentic::analyzer::{AnalyzerEvent, OutputAnalyzer};
use crate::hooks::server::HooksServer;

/// Start the hooks HTTP sidecar server.
///
/// This reuses the same `HooksServer` from the agent's server mode. Hook events
/// from Claude Code arrive via HTTP, are translated to `AgenticAgentMessage`,
/// and dispatched to the `AgenticProcessor` for local DB writes and event
/// broadcasting.
pub(crate) async fn start_hooks_server(state: Arc<LocalAppState>, shutdown: CancellationToken) {
    // Channel for agentic messages from hooks
    let (agentic_tx, agentic_rx) = mpsc::channel::<AgenticAgentMessage>(64);

    // The hooks server needs an outbound_tx for AgentMessage (used for SessionIdCaptured).
    // In local mode we don't need to send agent messages to a server, so we use a
    // dummy channel that we drain and discard.
    let (outbound_tx, _outbound_rx) = mpsc::channel::<AgentMessage>(64);

    let sent_cc_session_ids = std::sync::Arc::new(tokio::sync::RwLock::new(
        std::collections::HashSet::<String>::new(),
    ));
    let delivery_coordinator = std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::knowledge::context_delivery::DeliveryCoordinator::new(),
    ));
    let hooks_server = HooksServer::new(
        agentic_tx,
        state.session_mapper.clone(),
        outbound_tx,
        sent_cc_session_ids,
        delivery_coordinator,
    );

    // Convert CancellationToken to a watch channel for the hooks server
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let shutdown_for_hooks = shutdown.clone();
    tokio::spawn(async move {
        shutdown_for_hooks.cancelled().await;
        let _ = shutdown_tx.send(true);
    });

    match hooks_server.start(shutdown_rx).await {
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

    // Spawn a task to consume agentic messages from hooks and dispatch to processor
    spawn_hooks_message_consumer(state, agentic_rx);
}

/// Consume agentic messages from the hooks server channel and process them locally.
fn spawn_hooks_message_consumer(
    state: Arc<LocalAppState>,
    mut agentic_rx: mpsc::Receiver<AgenticAgentMessage>,
) {
    tokio::spawn(async move {
        while let Some(msg) = agentic_rx.recv().await {
            // Register/unregister loop mappings
            match &msg {
                AgenticAgentMessage::LoopDetected {
                    loop_id,
                    session_id,
                    ..
                } => {
                    state
                        .session_mapper
                        .register_loop(*session_id, *loop_id)
                        .await;
                }
                AgenticAgentMessage::LoopEnded { loop_id, .. } => {
                    tracing::info!(loop_id = %loop_id, "agentic loop ended (hook)");
                    state.session_mapper.remove_loop(loop_id).await;
                }
                _ => {}
            }

            if let Err(e) = state.agentic_processor.handle_message(msg).await {
                tracing::warn!(error = %e, "failed to process agentic hook message");
            }
        }
    });
}

/// Spawn a periodic task that scans process trees for agentic tools.
///
/// Every 1 second, checks all active PTY sessions for known agentic tool
/// processes (Claude Code, Codex, etc.) and emits detection/ended events.
pub(crate) fn spawn_agentic_detection_loop(state: Arc<LocalAppState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(AGENTIC_CHECK_INTERVAL);
        // Skip the first immediate tick
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Collect session PIDs
                    let session_pids: Vec<_> = {
                        let mgr = state.session_manager.lock().await;
                        mgr.session_pids().collect()
                    };

                    // Check for agentic tools
                    let messages = {
                        let mut mgr = state.agentic_manager.lock().await;
                        mgr.check_sessions(session_pids.into_iter())
                    };

                    // Register/unregister loop mappings and dispatch to processor
                    for msg in messages {
                        match &msg {
                            AgenticAgentMessage::LoopDetected {
                                loop_id,
                                session_id,
                                ..
                            } => {
                                state.session_mapper.register_loop(*session_id, *loop_id).await;
                            }
                            AgenticAgentMessage::LoopEnded { loop_id, .. } => {
                                tracing::info!(loop_id = %loop_id, "agentic loop ended (process exited)");
                                state.session_mapper.remove_loop(loop_id).await;
                            }
                            _ => {}
                        }

                        if let Err(e) = state.agentic_processor.handle_message(msg).await {
                            tracing::warn!(error = %e, "failed to process agentic detection message");
                        }
                    }

                    // Check for idle loops and transition to WaitingForInput
                    state.agentic_processor.check_idle_loops().await;
                }
                () = state.shutdown.cancelled() => {
                    tracing::debug!("agentic detection loop shutting down");
                    break;
                }
            }
        }
    });
}

/// Spawn a background task that reads PTY output from all sessions and routes it
/// to the in-memory `SessionState` scrollback buffer and all connected browser
/// WebSocket clients.
///
/// This is the local-mode equivalent of the PTY output handling in `connection.rs`.
/// Instead of sending output over a WebSocket to a remote server, we write directly
/// to the session store and browser senders.
///
/// Additionally, output is fed to the `AgenticLoopManager` for agentic state
/// detection (e.g., Claude Code approval prompts, completion patterns).
pub(crate) fn spawn_pty_output_loop(state: Arc<LocalAppState>) {
    tokio::spawn(async move {
        let mut pty_output_rx = state.pty_output_rx.lock().await;
        let mut session_analyzers: std::collections::HashMap<
            zremote_protocol::SessionId,
            OutputAnalyzer,
        > = std::collections::HashMap::new();

        while let Some(pty_output) = pty_output_rx.recv().await {
            let session_id = pty_output.session_id;
            let data = pty_output.data;

            if data.is_empty() {
                // EOF from main pane -- session ended

                // Clean up analyzer and channel dialog detector for this session
                session_analyzers.remove(&session_id);
                {
                    let mut detectors = state.channel_dialog_detectors.lock().await;
                    detectors.remove(&session_id);
                }

                // Notify agentic manager that session closed
                let loop_ended = {
                    let mut mgr = state.agentic_manager.lock().await;
                    mgr.on_session_closed(&session_id)
                };
                if let Some(msg) = loop_ended {
                    if let AgenticAgentMessage::LoopEnded { ref loop_id, .. } = msg {
                        state.session_mapper.remove_loop(loop_id).await;
                    }
                    if let Err(e) = state.agentic_processor.handle_message(msg).await {
                        tracing::warn!(error = %e, "failed to process LoopEnded on session close");
                    }
                }

                let exit_code = {
                    let mut mgr = state.session_manager.lock().await;
                    mgr.close(&session_id)
                };
                tracing::info!(
                    session_id = %session_id,
                    exit_code = ?exit_code,
                    "PTY session ended"
                );

                // Update DB status
                let session_id_str = session_id.to_string();
                let _ = sqlx::query(
                    "UPDATE sessions SET status = 'closed', exit_code = ?, closed_at = datetime('now') WHERE id = ?",
                )
                .bind(exit_code)
                .bind(&session_id_str)
                .execute(&state.db)
                .await;

                // Notify browser clients
                {
                    let mut sessions = state.sessions.write().await;
                    if let Some(session_state) = sessions.get_mut(&session_id) {
                        session_state.status = zremote_protocol::status::SessionStatus::Closed;
                        let msg = zremote_core::state::BrowserMessage::SessionClosed { exit_code };
                        session_state
                            .browser_senders
                            .retain(|tx| match tx.try_send(msg.clone()) {
                                Ok(()) => true,
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                            });
                    }
                }

                // Remove from in-memory store
                {
                    let mut sessions = state.sessions.write().await;
                    sessions.remove(&session_id);
                }

                // Broadcast event
                let _ = state
                    .events
                    .send(zremote_core::state::ServerEvent::SessionClosed {
                        session_id: session_id.to_string(),
                        exit_code,
                    });
            } else {
                // Check for dev channel dialog auto-approval.
                // Release detectors lock before acquiring session_manager to
                // avoid ABBA deadlock with route handlers.
                let should_approve = {
                    let mut detectors = state.channel_dialog_detectors.lock().await;
                    if let Some(detector) = detectors.get_mut(&session_id) {
                        let fired = detector.feed(&data);
                        if detector.triggered() {
                            detectors.remove(&session_id);
                        }
                        fired
                    } else {
                        false
                    }
                };
                if should_approve {
                    tracing::info!(%session_id, "auto-approving dev channel dialog");
                    let mut mgr = state.session_manager.lock().await;
                    if let Err(e) = mgr.write_to(&session_id, b"\r") {
                        tracing::warn!(
                            %session_id, error = %e,
                            "failed to auto-approve dev channel dialog"
                        );
                    }
                }

                // Feed through per-session analyzer
                let analyzer = session_analyzers.entry(session_id).or_insert_with(|| {
                    let default_cwd = dirs::home_dir().map(|p| p.to_string_lossy().to_string());
                    OutputAnalyzer::with_initial_cwd(default_cwd)
                });
                let events = analyzer.process_output(&data);
                for event in events {
                    if let AnalyzerEvent::NodeCompleted(node) = event {
                        let loop_id = {
                            let mgr = state.agentic_manager.lock().await;
                            mgr.loop_id_for_session(&session_id)
                        };
                        let msg = AgenticAgentMessage::ExecutionNode {
                            session_id,
                            loop_id,
                            timestamp: node.timestamp,
                            kind: node.kind,
                            input: node.input,
                            output_summary: node.output_summary,
                            exit_code: node.exit_code,
                            working_dir: node.working_dir,
                            duration_ms: node.duration_ms,
                        };
                        if let Err(e) = state.agentic_processor.handle_message(msg).await {
                            tracing::warn!(error = %e, "failed to process execution node");
                        }
                    }
                }

                // Main pane output -> scrollback + browser senders
                let mut sessions = state.sessions.write().await;
                if let Some(session_state) = sessions.get_mut(&session_id) {
                    session_state.append_scrollback(data.clone());
                    let msg = zremote_core::state::BrowserMessage::Output {
                        pane_id: None,
                        data,
                    };
                    session_state
                        .browser_senders
                        .retain(|tx| match tx.try_send(msg.clone()) {
                            Ok(()) => true,
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                        });
                }
            }
        }
    });
}
