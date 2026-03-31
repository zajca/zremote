use std::time::Duration;

use tokio::sync::mpsc;
use zremote_protocol::claude::{ClaudeAgentMessage, ClaudeServerMessage};
use zremote_protocol::knowledge::KnowledgeServerMessage;
use zremote_protocol::{AgentMessage, AgenticAgentMessage, HostId, ServerMessage, SessionId};

use super::registration::default_shell;
use crate::agentic::analyzer::OutputAnalyzer;
use crate::agentic::manager::AgenticLoopManager;
use crate::bridge::{self, BridgeSenders};
use crate::hooks::mapper::SessionMapper;
use crate::project::ProjectScanner;
use crate::project::git::GitInspector;
use crate::session::SessionManager;
use zremote_core::validation::validate_path_no_traversal;

/// Handle a `SessionCreate` message: spawn a PTY and send `SessionCreated` or `Error`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_session_create(
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
pub(super) async fn handle_server_message(
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
    session_analyzers: &mut std::collections::HashMap<SessionId, OutputAnalyzer>,
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
            session_analyzers.insert(*session_id, OutputAnalyzer::new());
        }
        ServerMessage::SessionClose { session_id } => {
            // Clean up analyzer and agentic loop
            session_analyzers.remove(session_id);
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
            if let Some(analyzer) = session_analyzers.get_mut(session_id) {
                analyzer.mark_input_sent();
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
            if let Err(e) = validate_path_no_traversal(path) {
                tracing::warn!(path = %path, error = %e, "rejected ProjectRegister with invalid path");
                return;
            }
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
            if let Err(e) = validate_path_no_traversal(path) {
                tracing::warn!(path = %path, error = %e, "rejected ListDirectory with invalid path");
                let _ = outbound_tx.try_send(AgentMessage::DirectoryListing {
                    request_id: *request_id,
                    path: path.clone(),
                    entries: vec![],
                    error: Some(format!("invalid path: {e}")),
                });
                return;
            }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::Utc;
    use tokio::sync::mpsc;
    use uuid::Uuid;
    use zremote_protocol::knowledge::KnowledgeServerMessage;
    use zremote_protocol::{AgentMessage, AgenticAgentMessage, ServerMessage};

    use super::*;
    use crate::agentic::manager::AgenticLoopManager;
    use crate::bridge::{self, BridgeSenders};
    use crate::hooks::mapper::SessionMapper;
    use crate::project::ProjectScanner;
    use crate::session::SessionManager;

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
        std::collections::HashMap<SessionId, OutputAnalyzer>,
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
        let session_analyzers = std::collections::HashMap::new();
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
            session_analyzers,
        )
    }

    #[tokio::test]
    async fn handle_server_message_heartbeat_ack() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
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
            &mut sa,
        )
        .await;
    }

    #[tokio::test]
    async fn handle_server_message_error() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
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
            &mut sa,
        )
        .await;
    }

    #[tokio::test]
    async fn handle_server_message_unexpected_register_ack() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
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
            &mut sa,
        )
        .await;
    }

    #[tokio::test]
    async fn handle_session_close_nonexistent_session() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
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
            &mut sa,
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
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
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
            &mut sa,
        )
        .await;
    }

    #[tokio::test]
    async fn handle_terminal_resize_nonexistent_session() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, _orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
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
            &mut sa,
        )
        .await;

        // Resize for nonexistent session should NOT create a phantom scrollback entry.
        let guard = bsb.read().await;
        assert!(
            guard.get(&session_id).is_none(),
            "scrollback entry should not exist for unknown session"
        );
    }
}
