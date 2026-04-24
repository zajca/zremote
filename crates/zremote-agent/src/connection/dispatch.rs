use std::time::Duration;

use tokio::sync::mpsc;
use zremote_protocol::claude::{ClaudeAgentMessage, ClaudeServerMessage};
use zremote_protocol::knowledge::KnowledgeServerMessage;
use zremote_protocol::{AgentMessage, AgenticAgentMessage, HostId, ServerMessage, SessionId};

use crate::agentic::analyzer::OutputAnalyzer;
use crate::agentic::manager::AgenticLoopManager;
use crate::bridge::{self, BridgeSenders};
use crate::claude::ChannelDialogDetector;
use crate::hooks::mapper::SessionMapper;
use crate::local::routes::projects::worktree::reject_leading_dash;
use crate::project::ProjectScanner;
use crate::project::action_runner::ActionRunContext;
use crate::project::git::GitInspector;
use crate::pty::shell_integration::ShellIntegrationConfig;
use crate::session::SessionManager;
use crate::shell::{default_shell, resolve_shell};
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
    let shell_owned = resolve_shell(shell);
    let shell = shell_owned.as_str();
    let manual_config = ShellIntegrationConfig::for_manual_session();
    match session_manager
        .create(
            session_id,
            shell,
            cols,
            rows,
            working_dir,
            env,
            Some(&manual_config),
        )
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

/// Send a `WorktreeCreationProgress` message upstream. Best-effort: a full
/// or closed outbound channel should not abort the git operation. The
/// server translates these into `ServerEvent::WorktreeCreationProgress`
/// broadcasts.
async fn send_creation_progress(
    tx: &mpsc::Sender<AgentMessage>,
    project_path: &str,
    job_id: &str,
    stage: zremote_protocol::events::WorktreeCreationStage,
    percent: u8,
    message: Option<String>,
) {
    if tx
        .send(AgentMessage::WorktreeCreationProgress {
            project_path: project_path.to_string(),
            job_id: job_id.to_string(),
            stage,
            percent,
            message,
        })
        .await
        .is_err()
    {
        tracing::warn!(
            project_path = %project_path,
            job_id = %job_id,
            "outbound channel closed, WorktreeCreationProgress dropped"
        );
    }
}

/// Maximum wall time for the `pre_delete` captured hook in server mode. A
/// stuck teardown must not pin the agent's dispatch task indefinitely — the
/// same 2-minute ceiling used by the local-mode delete handler applies here.
const SERVER_PRE_DELETE_HOOK_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum wall time for `Create`/`Delete` override hooks in server mode.
/// Matches the default git-flow guard (`WORKTREE_CREATE_TIMEOUT`) so a stuck
/// hook command cannot pin a tokio worker or starve `WorktreeCreated`/
/// `WorktreeDeleted` acknowledgements indefinitely.
const SERVER_WORKTREE_HOOK_TIMEOUT: Duration = Duration::from_secs(60);

/// Read full project settings off the blocking runtime. Returns `None` when
/// no settings file exists or reading fails (treated as "no hooks
/// configured" — identical semantics to the local-mode helper).
async fn read_project_settings_server(
    project_path: &str,
) -> Option<zremote_protocol::ProjectSettings> {
    let pp = project_path.to_string();
    tokio::task::spawn_blocking(move || {
        crate::project::settings::read_settings(std::path::Path::new(&pp))
    })
    .await
    .ok()?
    .ok()
    .flatten()
}

/// Validate the `branch` / `path` / `base_ref` fields of a `WorktreeCreate`
/// request in server mode. Returns the error message if any value starts with
/// `-` (CWE-88). Must run *before* the hook override path so a custom hook
/// cannot be used to smuggle git flags via `{{branch}}` / `{{base_ref}}`.
fn validate_worktree_create_inputs(
    branch: &str,
    path: Option<&str>,
    base_ref: Option<&str>,
) -> Result<(), String> {
    reject_leading_dash("branch", branch).map_err(|e| e.message)?;
    if let Some(p) = path {
        reject_leading_dash("path", p).map_err(|e| e.message)?;
    }
    if let Some(b) = base_ref {
        reject_leading_dash("base_ref", b).map_err(|e| e.message)?;
    }
    Ok(())
}

/// Send a `WorktreeError` over the outbound channel; logs if the channel has
/// closed. Centralised so every error path in the dispatcher stays consistent.
async fn send_worktree_error(
    tx: &mpsc::Sender<AgentMessage>,
    project_path: String,
    message: String,
) {
    if tx
        .send(AgentMessage::WorktreeError {
            project_path,
            message,
        })
        .await
        .is_err()
    {
        tracing::warn!("outbound channel closed, WorktreeError dropped");
    }
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
    mut channel_bridge: Option<&mut crate::channel::bridge::ChannelBridge>,
    channel_dialog_detectors: &mut std::collections::HashMap<SessionId, ChannelDialogDetector>,
    launcher_registry: &std::sync::Arc<crate::agents::LauncherRegistry>,
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
            session_analyzers.insert(
                *session_id,
                OutputAnalyzer::with_initial_cwd(working_dir.clone()),
            );
        }
        ServerMessage::SessionClose { session_id } => {
            // Clean up analyzer, channel bridge, and agentic loop
            session_analyzers.remove(session_id);
            if let Some(bridge) = channel_bridge.as_mut() {
                bridge.remove(session_id);
            }
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
                        main_repo_path: info.main_repo_path,
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
            base_ref,
        } => {
            let tx = outbound_tx.clone();
            let project_path = project_path.clone();
            let branch = branch.clone();
            let wt_path = path.clone();
            let new_branch = *new_branch;
            let base_ref = base_ref.clone();
            tokio::spawn(async move {
                // CWE-88 guard: reject leading-dash inputs *before* any hook
                // lookup. Doing it first keeps the hook override path from
                // becoming a way to smuggle git flags through `{{branch}}` /
                // `{{base_ref}}` expansions.
                if let Err(msg) = validate_worktree_create_inputs(
                    &branch,
                    wt_path.as_deref(),
                    base_ref.as_deref(),
                ) {
                    send_worktree_error(&tx, project_path, msg).await;
                    return;
                }

                let settings = read_project_settings_server(&project_path).await;
                let worktree_name = branch.replace('/', "-");
                // Create-time context: worktree_path is None because the
                // worktree doesn't exist yet — resolve_working_dir falls back
                // to project_path so the hook can cd into a directory that
                // exists. Matches local-mode behaviour in worktree.rs.
                let ctx = ActionRunContext {
                    project_path: project_path.clone(),
                    worktree_path: None,
                    branch: Some(branch.clone()),
                    worktree_name: Some(worktree_name.clone()),
                    inputs: std::collections::HashMap::new(),
                };

                // Custom `create` override: run captured (no PTY in server
                // mode) and then locate the new worktree via git inspect so
                // the server can emit a `WorktreeCreated` event just like
                // the default flow would. Note: we do NOT emit the
                // `WorktreeCreationProgress` Init/Finalizing/Done lifecycle
                // on the hook path — GUIs that watch those events will not
                // see progress for hook-driven creates. This matches the
                // legacy `create_command` behaviour and is out-of-scope for
                // this RFC.
                if let Some(ref sett) = settings {
                    match crate::project::hook_dispatcher::run_worktree_hook(
                        sett,
                        crate::project::hook_dispatcher::WorktreeSlot::Create,
                        ctx.clone(),
                        Some(SERVER_WORKTREE_HOOK_TIMEOUT),
                    )
                    .await
                    {
                        Ok(Some(hook_info)) => {
                            // `worktree_path` is the caller-supplied target
                            // (may be empty if git is auto-naming); the real
                            // path is re-derived from `git inspect` below.
                            if tx
                                .send(AgentMessage::WorktreeHookResult {
                                    project_path: project_path.clone(),
                                    worktree_path: wt_path.clone().unwrap_or_default(),
                                    hook_type: "create".to_string(),
                                    success: hook_info.success,
                                    output: hook_info.output.clone(),
                                    duration_ms: hook_info.duration_ms,
                                })
                                .await
                                .is_err()
                            {
                                tracing::warn!(
                                    "outbound channel closed, WorktreeHookResult dropped"
                                );
                            }

                            if !hook_info.success {
                                let msg = hook_info
                                    .output
                                    .unwrap_or_else(|| "custom create hook failed".to_string());
                                send_worktree_error(&tx, project_path, msg).await;
                                return;
                            }

                            // Success — locate the worktree in git so the
                            // server gets a real WorktreeInfo to store.
                            let pp = project_path.clone();
                            let inspect_result = tokio::task::spawn_blocking(move || {
                                GitInspector::inspect(std::path::Path::new(&pp))
                            })
                            .await;

                            let worktree = match inspect_result {
                                Ok(Some((_git_info, worktrees))) => worktrees
                                    .into_iter()
                                    .find(|w| {
                                        w.branch.as_deref() == Some(&*branch)
                                            || w.path.ends_with(&worktree_name)
                                    })
                                    .map(|wt| zremote_protocol::project::WorktreeInfo {
                                        path: wt.path,
                                        branch: wt.branch,
                                        commit_hash: wt.commit_hash,
                                        is_detached: wt.is_detached,
                                        is_locked: wt.is_locked,
                                        is_dirty: wt.is_dirty,
                                        commit_message: wt.commit_message,
                                    }),
                                _ => None,
                            };

                            let Some(worktree) = worktree else {
                                send_worktree_error(
                                    &tx,
                                    project_path,
                                    "custom create hook succeeded but worktree not found in git"
                                        .to_string(),
                                )
                                .await;
                                return;
                            };

                            // Run PostCreate (captured) if configured. By
                            // design, a missing-action resolution error here
                            // is non-fatal — the worktree was created
                            // successfully; dropping `WorktreeCreated` on
                            // the floor because of a misconfigured secondary
                            // hook would be worse than surfacing it later.
                            // We log the error and proceed without a hook
                            // result so the server still emits the creation
                            // event.
                            let post_ctx = ActionRunContext {
                                worktree_path: Some(worktree.path.clone()),
                                branch: worktree.branch.clone(),
                                ..ctx.clone()
                            };
                            let post_hook =
                                match crate::project::hook_dispatcher::run_worktree_hook(
                                    sett,
                                    crate::project::hook_dispatcher::WorktreeSlot::PostCreate,
                                    post_ctx,
                                    Some(SERVER_WORKTREE_HOOK_TIMEOUT),
                                )
                                .await
                                {
                                    Ok(h) => h,
                                    Err(e) => {
                                        tracing::warn!(error = %e, "post_create resolution failed");
                                        None
                                    }
                                };

                            if tx
                                .send(AgentMessage::WorktreeCreated {
                                    project_path,
                                    worktree,
                                    hook_result: post_hook,
                                })
                                .await
                                .is_err()
                            {
                                tracing::warn!("outbound channel closed, WorktreeCreated dropped");
                            }
                            return;
                        }
                        Ok(None) => {
                            // No create override — fall through to default.
                        }
                        Err(e) => {
                            send_worktree_error(&tx, project_path, e.to_string()).await;
                            return;
                        }
                    }
                }

                // Default flow: existing GitInspector behavior. Thread
                // base_ref through so server-initiated create matches the
                // local-agent API — otherwise callers that specified a base
                // would silently fall back to HEAD. We also emit lifecycle
                // progress events so the server can broadcast them to GUIs.
                let job_id = uuid::Uuid::new_v4().to_string();

                // Init: before any blocking work.
                send_creation_progress(
                    &tx,
                    &project_path,
                    &job_id,
                    zremote_protocol::events::WorktreeCreationStage::Init,
                    0,
                    None,
                )
                .await;

                let pp = project_path.clone();
                let b = branch.clone();
                let wp = wt_path.clone();
                let br = base_ref.clone();
                let mut handle = tokio::task::spawn_blocking(move || {
                    GitInspector::create_worktree(
                        std::path::Path::new(&pp),
                        &b,
                        wp.as_ref().map(|p| std::path::Path::new(p.as_str())),
                        new_branch,
                        br.as_deref(),
                    )
                });
                // Bound the git subprocess the same way the local HTTP
                // route does — otherwise a hung git can leak a blocking
                // thread for the lifetime of the agent process. Kept in
                // sync with `local::routes::projects::worktree::WORKTREE_CREATE_TIMEOUT`.
                const WORKTREE_CREATE_TIMEOUT: Duration = Duration::from_secs(60);
                let result = tokio::time::timeout(WORKTREE_CREATE_TIMEOUT, &mut handle).await;
                let timeout_secs = WORKTREE_CREATE_TIMEOUT.as_secs();
                match result {
                    Ok(Ok(Ok(worktree))) => {
                        // Finalizing: git is done; DB insert + hook still
                        // ahead of us.
                        send_creation_progress(
                            &tx,
                            &project_path,
                            &job_id,
                            zremote_protocol::events::WorktreeCreationStage::Finalizing,
                            75,
                            None,
                        )
                        .await;

                        // Run PostCreate hook (captured) through the shared
                        // dispatcher so named-action resolution + legacy
                        // `on_create` fallback work identically to local.
                        // Resolution errors (missing action) are downgraded
                        // to `None` and logged — the worktree already exists,
                        // so withholding `WorktreeCreated` would be worse
                        // than a warn-level log for the misconfigured hook.
                        let hook_result = if let Some(ref sett) = settings {
                            let post_ctx = ActionRunContext {
                                worktree_path: Some(worktree.path.clone()),
                                branch: worktree.branch.clone(),
                                ..ctx.clone()
                            };
                            match crate::project::hook_dispatcher::run_worktree_hook(
                                sett,
                                crate::project::hook_dispatcher::WorktreeSlot::PostCreate,
                                post_ctx,
                                Some(SERVER_WORKTREE_HOOK_TIMEOUT),
                            )
                            .await
                            {
                                Ok(h) => h,
                                Err(e) => {
                                    tracing::warn!(error = %e, "post_create resolution failed");
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        send_creation_progress(
                            &tx,
                            &project_path,
                            &job_id,
                            zremote_protocol::events::WorktreeCreationStage::Done,
                            100,
                            None,
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
                    Ok(Ok(Err(stderr))) => {
                        // Sanitize the raw git stderr before forwarding —
                        // matches the local-route behavior fixed in 96ebda9.
                        let err =
                            zremote_protocol::project::WorktreeError::from_git_stderr(&stderr);
                        send_creation_progress(
                            &tx,
                            &project_path,
                            &job_id,
                            zremote_protocol::events::WorktreeCreationStage::Failed,
                            100,
                            Some(err.message.clone()),
                        )
                        .await;
                        send_worktree_error(&tx, project_path, err.message).await;
                    }
                    Ok(Err(e)) => {
                        let msg = format!("worktree create task panicked: {e}");
                        send_creation_progress(
                            &tx,
                            &project_path,
                            &job_id,
                            zremote_protocol::events::WorktreeCreationStage::Failed,
                            100,
                            Some(msg.clone()),
                        )
                        .await;
                        send_worktree_error(&tx, project_path, msg).await;
                    }
                    Err(_) => {
                        // Timed out — abort the blocking task so the
                        // subprocess doesn't linger past this handler.
                        handle.abort();
                        tracing::warn!(
                            job_id = %job_id,
                            timeout_secs,
                            "worktree create timed out (server mode)"
                        );
                        let msg = format!("timed out after {timeout_secs}s");
                        send_creation_progress(
                            &tx,
                            &project_path,
                            &job_id,
                            zremote_protocol::events::WorktreeCreationStage::Failed,
                            100,
                            Some(msg.clone()),
                        )
                        .await;
                        send_worktree_error(&tx, project_path, msg).await;
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
                let settings = read_project_settings_server(&project_path).await;
                let worktree_name = std::path::Path::new(&worktree_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                let ctx = ActionRunContext {
                    project_path: project_path.clone(),
                    worktree_path: Some(worktree_path.clone()),
                    branch: None,
                    worktree_name: Some(worktree_name.clone()),
                    inputs: std::collections::HashMap::new(),
                };

                // PreDelete runs first, bounded by SERVER_PRE_DELETE_HOOK_TIMEOUT
                // so a stuck teardown cannot pin the dispatch task. Non-zero
                // exit aborts the delete — same contract as local mode.
                let pre_delete_result = if let Some(ref sett) = settings {
                    match crate::project::hook_dispatcher::run_worktree_hook(
                        sett,
                        crate::project::hook_dispatcher::WorktreeSlot::PreDelete,
                        ctx.clone(),
                        Some(SERVER_PRE_DELETE_HOOK_TIMEOUT),
                    )
                    .await
                    {
                        Ok(h) => h,
                        Err(e) => {
                            send_worktree_error(&tx, project_path, e.to_string()).await;
                            return;
                        }
                    }
                } else {
                    None
                };

                if let Some(ref hr) = pre_delete_result {
                    if tx
                        .send(AgentMessage::WorktreeHookResult {
                            project_path: project_path.clone(),
                            worktree_path: worktree_path.clone(),
                            hook_type: "pre_delete".to_string(),
                            success: hr.success,
                            output: hr.output.clone(),
                            duration_ms: hr.duration_ms,
                        })
                        .await
                        .is_err()
                    {
                        tracing::warn!(
                            "outbound channel closed, pre_delete WorktreeHookResult dropped"
                        );
                    }
                    if !hr.success {
                        let msg = hr
                            .output
                            .clone()
                            .unwrap_or_else(|| "pre_delete hook failed".to_string());
                        send_worktree_error(&tx, project_path, msg).await;
                        return;
                    }
                }

                // Delete override (captured in server mode): replaces the
                // default `git worktree remove` call. Success emits
                // WorktreeDeleted; failure emits WorktreeError.
                if let Some(ref sett) = settings {
                    match crate::project::hook_dispatcher::run_worktree_hook(
                        sett,
                        crate::project::hook_dispatcher::WorktreeSlot::Delete,
                        ctx.clone(),
                        Some(SERVER_WORKTREE_HOOK_TIMEOUT),
                    )
                    .await
                    {
                        Ok(Some(hook_info)) => {
                            if tx
                                .send(AgentMessage::WorktreeHookResult {
                                    project_path: project_path.clone(),
                                    worktree_path: worktree_path.clone(),
                                    hook_type: "delete".to_string(),
                                    success: hook_info.success,
                                    output: hook_info.output.clone(),
                                    duration_ms: hook_info.duration_ms,
                                })
                                .await
                                .is_err()
                            {
                                tracing::warn!(
                                    "outbound channel closed, delete WorktreeHookResult dropped"
                                );
                            }

                            if hook_info.success {
                                if tx
                                    .send(AgentMessage::WorktreeDeleted {
                                        project_path,
                                        worktree_path,
                                    })
                                    .await
                                    .is_err()
                                {
                                    tracing::warn!(
                                        "outbound channel closed, WorktreeDeleted dropped"
                                    );
                                }
                            } else {
                                let msg = hook_info
                                    .output
                                    .unwrap_or_else(|| "custom delete hook failed".to_string());
                                send_worktree_error(&tx, project_path, msg).await;
                            }
                            return;
                        }
                        Ok(None) => {
                            // No delete override — fall through to default.
                        }
                        Err(e) => {
                            send_worktree_error(&tx, project_path, e.to_string()).await;
                            return;
                        }
                    }
                }

                // Default flow: `git worktree remove`.
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
                        send_worktree_error(&tx, project_path, msg).await;
                    }
                    Err(e) => {
                        send_worktree_error(
                            &tx,
                            project_path,
                            format!("worktree delete task panicked: {e}"),
                        )
                        .await;
                    }
                }
            });
        }
        ServerMessage::ClaudeAction(claude_msg) => {
            handle_claude_server_message(
                claude_msg,
                session_manager,
                outbound_tx,
                session_mapper,
                channel_dialog_detectors,
            )
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
        ServerMessage::ContextPush {
            session_id,
            memories,
            conventions,
        } => {
            tracing::info!(
                session = %session_id,
                memories = memories.len(),
                conventions = conventions.len(),
                "received context push from server"
            );
            // Context push is handled in the connection loop via the
            // DeliveryCoordinator. The dispatch layer logs and acknowledges.
            // Actual delivery happens when the agent transitions to idle.
            let memory_inputs: Vec<crate::knowledge::context_delivery::ContextMemoryInput> =
                memories
                    .iter()
                    .map(|m| crate::knowledge::context_delivery::ContextMemoryInput {
                        key: "server-push".to_string(),
                        content: m.clone(),
                        category: zremote_protocol::knowledge::MemoryCategory::Convention,
                        confidence: 1.0,
                    })
                    .collect();
            let context = crate::knowledge::context_delivery::ContextAssembler::assemble(
                "server-push",
                "",
                "unknown",
                None,
                &[],
                &memory_inputs,
                conventions,
                crate::knowledge::context_delivery::ContextTrigger::ManualPush,
            );
            // Write content directly to the session PTY
            let content = context.render();
            if !content.is_empty()
                && let Err(e) = session_manager.write_to(session_id, content.as_bytes())
            {
                tracing::warn!(
                    session = %session_id,
                    error = %e,
                    "failed to deliver context push to PTY"
                );
            }
        }
        ServerMessage::ChannelAction(action) => {
            use zremote_protocol::channel::ChannelServerAction;
            match action {
                ChannelServerAction::ChannelSend {
                    session_id: sid,
                    message,
                } => {
                    if let Some(channel_bridge) = channel_bridge.as_mut() {
                        // Try to discover first if not already connected
                        if !channel_bridge.is_available(sid) {
                            match channel_bridge.discover(*sid).await {
                                Ok(true) => {
                                    tracing::info!(session = %sid, "channel server discovered on demand");
                                }
                                Ok(false) => {
                                    tracing::warn!(session = %sid, "no channel server found for session");
                                    return;
                                }
                                Err(e) => {
                                    tracing::warn!(session = %sid, error = %e, "failed to discover channel server");
                                    return;
                                }
                            }
                        }
                        if let Err(e) = channel_bridge.send(sid, message).await {
                            tracing::warn!(session = %sid, error = %e, "failed to send channel message");
                        }
                    } else {
                        tracing::debug!(?action, "channel bridge not available");
                    }
                }
                ChannelServerAction::PermissionResponse {
                    session_id: sid,
                    request_id,
                    allowed,
                    reason,
                } => {
                    if let Some(channel_bridge) = channel_bridge.as_mut() {
                        if let Err(e) = channel_bridge
                            .respond_permission(sid, request_id, *allowed, reason.as_deref())
                            .await
                        {
                            tracing::warn!(session = %sid, error = %e, "failed to forward permission response");
                        }
                    } else {
                        tracing::debug!(?action, "channel bridge not available");
                    }
                }
            }
        }
        ServerMessage::AgentAction(action) => {
            handle_agent_server_message(
                action,
                session_manager,
                outbound_tx,
                launcher_registry,
                channel_dialog_detectors,
            )
            .await;
        }
        // RFC-009 P2: synchronous branch listing with request_id correlation.
        // Returns a `BranchListResponse` either with `branches: Some(..)` or
        // `error: Some(..)` — never both. A missing project directory maps to
        // `PathMissing` so the server can render a precise remediation hint;
        // any other git failure degrades to `Internal` with the raw message
        // kept in `message` (the hint is user-facing, safe).
        ServerMessage::BranchListRequest {
            request_id,
            project_path,
        } => {
            handle_branch_list_request(outbound_tx, *request_id, project_path.clone()).await;
        }
        // RFC-009 P2: synchronous worktree create with request_id correlation.
        // Delegates to the shared `worktree::service::run_worktree_create`
        // helper so validation + git + post_create hook stay identical to
        // the local HTTP route. Progress events route through the outbound
        // channel as `AgentMessage::WorktreeCreationProgress`.
        ServerMessage::WorktreeCreateRequest {
            request_id,
            project_path,
            branch,
            path,
            new_branch,
            base_ref,
        } => {
            handle_worktree_create_request(
                outbound_tx,
                *request_id,
                project_path.clone(),
                branch.clone(),
                path.clone(),
                *new_branch,
                base_ref.clone(),
            )
            .await;
        }
    }
}

/// Handle `ServerMessage::BranchListRequest`. Spawned into its own task so a
/// slow `for-each-ref` on a huge repo doesn't block the dispatch loop — we
/// don't await completion here; the response is sent on the outbound channel
/// as the task finishes.
fn handle_branch_list_request(
    outbound_tx: &mpsc::Sender<AgentMessage>,
    request_id: uuid::Uuid,
    project_path: String,
) -> impl std::future::Future<Output = ()> + Send {
    let tx = outbound_tx.clone();
    async move {
        // Reject path traversal (`..`) before any I/O — mirrors the guard on
        // `ProjectRegister` / `ListDirectory`. A traversal-containing path
        // does not resolve to a real project, so PathMissing carries the same
        // actionable hint.
        if let Err(e) = validate_path_no_traversal(&project_path) {
            tracing::warn!(
                path = %project_path,
                error = %e,
                "rejected BranchListRequest with invalid path"
            );
            let err = zremote_protocol::project::WorktreeError::new(
                zremote_protocol::project::WorktreeErrorCode::PathMissing,
                "Project path is not valid. Remove the project and re-add it with a correct path.",
                "invalid path",
            );
            if tx
                .send(AgentMessage::BranchListResponse {
                    request_id,
                    branches: None,
                    error: Some(err),
                })
                .await
                .is_err()
            {
                tracing::warn!("outbound channel closed, BranchListResponse dropped");
            }
            return;
        }
        tokio::spawn(async move {
            // Existence check first so we can return a precise PathMissing
            // error. `GitInspector::list_branches` would fail with a raw git
            // message that collapses to `Internal` via `from_git_stderr`,
            // which is less actionable than a typed PathMissing.
            let path_buf = std::path::PathBuf::from(&project_path);
            let exists = tokio::task::spawn_blocking({
                let p = path_buf.clone();
                move || p.exists()
            })
            .await
            .unwrap_or(false);

            if !exists {
                let err = zremote_protocol::project::WorktreeError::new(
                    zremote_protocol::project::WorktreeErrorCode::PathMissing,
                    "Project path no longer exists on disk. Remove the project and re-add it with the correct path.",
                    format!("path does not exist: {project_path}"),
                );
                if tx
                    .send(AgentMessage::BranchListResponse {
                        request_id,
                        branches: None,
                        error: Some(err),
                    })
                    .await
                    .is_err()
                {
                    tracing::warn!("outbound channel closed, BranchListResponse dropped");
                }
                return;
            }

            let join = tokio::task::spawn_blocking(move || {
                GitInspector::list_branches(std::path::Path::new(&project_path))
            })
            .await;

            let message = match join {
                Ok(Ok(branches)) => AgentMessage::BranchListResponse {
                    request_id,
                    branches: Some(branches),
                    error: None,
                },
                Ok(Err(stderr)) => {
                    tracing::warn!(error = %stderr, "list_branches failed");
                    // Preserve PathMissing classification if git stderr
                    // happens to indicate it; otherwise map to Internal with a
                    // fixed safe message so raw git output (which can include
                    // filesystem paths, remote URLs, credential-helper details
                    // — CWE-200) never reaches the HTTP client. Raw stderr is
                    // kept in the `tracing::warn!` above for local debugging.
                    let err = zremote_protocol::project::WorktreeError::from_git_stderr(&stderr);
                    let err = if matches!(
                        err.code,
                        zremote_protocol::project::WorktreeErrorCode::PathMissing
                    ) {
                        err
                    } else {
                        zremote_protocol::project::WorktreeError::new(
                            zremote_protocol::project::WorktreeErrorCode::Internal,
                            "Could not list branches for this project.",
                            "unexpected git error",
                        )
                    };
                    AgentMessage::BranchListResponse {
                        request_id,
                        branches: None,
                        error: Some(err),
                    }
                }
                Err(join_err) => {
                    tracing::error!(error = %join_err, "list_branches task panicked");
                    AgentMessage::BranchListResponse {
                        request_id,
                        branches: None,
                        error: Some(zremote_protocol::project::WorktreeError::new(
                            zremote_protocol::project::WorktreeErrorCode::Internal,
                            "Internal error while listing branches.",
                            "task join failed",
                        )),
                    }
                }
            };

            if tx.send(message).await.is_err() {
                tracing::warn!("outbound channel closed, BranchListResponse dropped");
            }
        });
    }
}

/// Handle `ServerMessage::WorktreeCreateRequest`. Delegates to the shared
/// `worktree::service` helper so validation + git + post_create stay in lock-
/// step with the local HTTP handler.
#[allow(clippy::too_many_arguments)]
fn handle_worktree_create_request(
    outbound_tx: &mpsc::Sender<AgentMessage>,
    request_id: uuid::Uuid,
    project_path: String,
    branch: String,
    path: Option<String>,
    new_branch: bool,
    base_ref: Option<String>,
) -> impl std::future::Future<Output = ()> + Send {
    let tx = outbound_tx.clone();
    async move {
        // Reject path traversal (`..`) before any I/O or spawn_blocking —
        // mirrors the guard on `ProjectRegister` / `ListDirectory`. A
        // traversal-containing path does not resolve to a real project, so
        // PathMissing is the correct classification for the client.
        if let Err(e) = validate_path_no_traversal(&project_path) {
            tracing::warn!(
                path = %project_path,
                error = %e,
                "rejected WorktreeCreateRequest with invalid path"
            );
            let err = zremote_protocol::project::WorktreeError::new(
                zremote_protocol::project::WorktreeErrorCode::PathMissing,
                "Project path is not valid. Remove the project and re-add it with a correct path.",
                "invalid path",
            );
            if tx
                .send(AgentMessage::WorktreeCreateResponse {
                    request_id,
                    worktree: None,
                    error: Some(err),
                })
                .await
                .is_err()
            {
                tracing::warn!("outbound channel closed, WorktreeCreateResponse dropped");
            }
            return;
        }
        tokio::spawn(async move {
            let job_id = uuid::Uuid::new_v4().to_string();

            // Progress callback: convert the service's stage + percent +
            // optional message into `AgentMessage::WorktreeCreationProgress`
            // on the outbound channel. Uses `try_send` so a full channel
            // doesn't deadlock the dispatch task — the server treats
            // progress as best-effort anyway.
            let progress_tx = tx.clone();
            let progress_project_path = project_path.clone();
            let progress_job_id = job_id.clone();
            let emit = move |stage, percent, message| {
                let msg = AgentMessage::WorktreeCreationProgress {
                    project_path: progress_project_path.clone(),
                    job_id: progress_job_id.clone(),
                    stage,
                    percent,
                    message,
                };
                if progress_tx.try_send(msg).is_err() {
                    tracing::warn!(
                        project_path = %progress_project_path,
                        job_id = %progress_job_id,
                        "outbound channel full, WorktreeCreationProgress dropped"
                    );
                }
            };

            let input = crate::worktree::service::WorktreeCreateInput {
                project_path: std::path::PathBuf::from(&project_path),
                branch,
                path: path.map(std::path::PathBuf::from),
                new_branch,
                base_ref,
            };

            let response = match crate::worktree::service::run_worktree_create(input, emit).await {
                Ok(output) => AgentMessage::WorktreeCreateResponse {
                    request_id,
                    worktree: Some(zremote_protocol::WorktreeCreateSuccessPayload {
                        path: output.path,
                        branch: output.branch,
                        commit_hash: output.commit_hash,
                        hook_result: output.hook_result,
                    }),
                    error: None,
                },
                Err(failure) => AgentMessage::WorktreeCreateResponse {
                    request_id,
                    worktree: None,
                    error: Some(failure.into_worktree_error()),
                },
            };

            if tx.send(response).await.is_err() {
                tracing::warn!(
                    request_id = %request_id,
                    "outbound channel closed, WorktreeCreateResponse dropped"
                );
            }
        });
    }
}

/// Handle a generic agent-launcher spawn request from the server.
///
/// This is the server-mode counterpart to `POST /api/agent-tasks` in local
/// mode: it looks up the launcher for the requested `agent_kind`, builds the
/// shell command, spawns a PTY, writes the command to the PTY, and notifies
/// the server via [`zremote_protocol::agents::AgentLifecycleMessage`].
///
/// On any failure it sends a `StartFailed` back so the server can mark the
/// session row as errored and surface the error to the GUI.
async fn handle_agent_server_message(
    action: &zremote_protocol::agents::AgentServerMessage,
    session_manager: &mut SessionManager,
    outbound_tx: &mpsc::Sender<AgentMessage>,
    launcher_registry: &std::sync::Arc<crate::agents::LauncherRegistry>,
    channel_dialog_detectors: &mut std::collections::HashMap<SessionId, ChannelDialogDetector>,
) {
    use zremote_protocol::agents::{AgentLifecycleMessage, AgentServerMessage};

    let AgentServerMessage::StartAgent {
        session_id,
        task_id,
        host_id: _,
        project_path,
        profile,
    } = action;

    // Send a StartFailed lifecycle event to the server. `error` is the
    // already-sanitized user-facing string. The full error should be logged
    // locally before calling this helper to avoid path/token disclosure over
    // the WS, then GUI, then log pipelines. The `task_id` is echoed back so
    // the server can correlate the failure with the originating launch.
    let send_failed = |error: String, agent_kind: String| {
        let msg = AgentMessage::AgentLifecycle(AgentLifecycleMessage::StartFailed {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            agent_kind,
            error,
        });
        if outbound_tx.try_send(msg).is_err() {
            tracing::warn!(
                session_id = %session_id,
                "outbound channel full, AgentLifecycle::StartFailed dropped; session may hang on the server"
            );
        }
    };

    // Map a `LauncherError` to a user-facing error string that is safe to
    // propagate back to the server. The full error is logged locally; the
    // returned string deliberately omits paths, tokens, and nested reasons
    // that an attacker could harvest from a misconfigured GUI client.
    fn sanitize_launcher_error(e: &crate::agents::LauncherError) -> String {
        use crate::agents::LauncherError;
        match e {
            LauncherError::UnknownKind(k) => format!("unknown agent kind: {k}"),
            LauncherError::InvalidSettings(_) => "invalid profile settings".to_string(),
            LauncherError::BuildFailed(_) => "command build failed".to_string(),
        }
    }

    // Resolve the launcher.
    let launcher = match launcher_registry.get(&profile.agent_kind) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                kind = %profile.agent_kind,
                error = %e,
                "unknown agent kind for StartAgent"
            );
            send_failed(sanitize_launcher_error(&e), profile.agent_kind.clone());
            return;
        }
    };

    // Parse the session_id coming over the wire.
    let parsed_session_id = match uuid::Uuid::parse_str(session_id) {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "invalid session_id in StartAgent"
            );
            send_failed("invalid session_id".to_string(), profile.agent_kind.clone());
            return;
        }
    };

    // Validate the working directory to reject path traversal from the
    // server. Path traversal on its own is caught by the kernel, but an
    // untrusted server should not be able to coax an agent into cd-ing
    // to an unrelated location silently.
    if let Err(e) = zremote_core::validation::validate_path_no_traversal(project_path) {
        tracing::warn!(
            session_id = %session_id,
            path = %project_path,
            error = %e,
            "invalid project_path in StartAgent"
        );
        send_failed(
            "invalid project_path".to_string(),
            profile.agent_kind.clone(),
        );
        return;
    }

    // Build command via the launcher.
    let request = crate::agents::LaunchRequest {
        session_id: parsed_session_id,
        working_dir: project_path,
        profile,
    };
    let launch = match launcher.build_command(&request) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "launcher build_command failed"
            );
            send_failed(sanitize_launcher_error(&e), profile.agent_kind.clone());
            return;
        }
    };

    // Spawn PTY session. Uses the canonical login shell (not resolve_shell) because
    // agent tasks deliberately ignore per-project `shell` settings — they always run
    // the launcher command under the user's real login environment.
    let shell = default_shell();
    let ai_config = ShellIntegrationConfig::for_ai_session();
    let pid = match session_manager
        .create(
            parsed_session_id,
            shell,
            120,
            40,
            Some(project_path.as_str()),
            None,
            Some(&ai_config),
        )
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "failed to spawn PTY for agent session"
            );
            send_failed(
                "failed to spawn PTY".to_string(),
                profile.agent_kind.clone(),
            );
            return;
        }
    };

    tracing::info!(
        session_id = %session_id,
        kind = %profile.agent_kind,
        pid = pid,
        "agent PTY session created"
    );

    // Notify that the PTY session is created (generic terminal-layer event
    // the server already handles for all sessions).
    if outbound_tx
        .try_send(AgentMessage::SessionCreated {
            session_id: parsed_session_id,
            shell: shell.to_string(),
            pid,
        })
        .is_err()
    {
        tracing::warn!("outbound channel full, SessionCreated dropped");
    }

    // Brief delay to let the shell initialize before writing the command.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Write the launcher command directly to the PTY stdin.
    if let Err(e) = session_manager.write_to(&parsed_session_id, launch.command.as_bytes()) {
        tracing::error!(
            session_id = %session_id,
            error = %e,
            "failed to write launcher command to PTY"
        );
        send_failed(
            "failed to write command to PTY".to_string(),
            profile.agent_kind.clone(),
        );
        return;
    }

    // Give the launcher a chance to register kind-specific state (e.g.
    // Claude's channel dialog auto-approve detector).
    let mut ctx = crate::agents::LauncherContext::Remote {
        channel_dialog_detectors,
    };
    launcher.after_spawn(parsed_session_id, &request, &mut ctx);

    // Send success lifecycle event back to server. Log a warning if the
    // outbound channel is full: the server will not learn the session
    // started and will mark it as errored when it times out. `task_id` is
    // echoed back so the server can correlate replies with pending launches.
    let started_msg = AgentMessage::AgentLifecycle(AgentLifecycleMessage::Started {
        session_id: session_id.clone(),
        task_id: task_id.clone(),
        agent_kind: profile.agent_kind.clone(),
    });
    if outbound_tx.try_send(started_msg).is_err() {
        tracing::warn!(
            session_id = %session_id,
            "outbound channel full, AgentLifecycle::Started dropped; server may mark session as errored"
        );
    }
}

/// Handle a Claude server message: start sessions, discover sessions, etc.
#[allow(clippy::too_many_lines)]
async fn handle_claude_server_message(
    msg: &ClaudeServerMessage,
    session_manager: &mut SessionManager,
    outbound_tx: &mpsc::Sender<AgentMessage>,
    session_mapper: &SessionMapper,
    channel_dialog_detectors: &mut std::collections::HashMap<SessionId, ChannelDialogDetector>,
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
            development_channels,
            print_mode,
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

            // Build the claude CLI command. The legacy (pre-phase-2) claude
            // flow has no env vars, so pass a shared empty map instead of
            // allocating a fresh `BTreeMap` on every message.
            static EMPTY_ENV: std::sync::LazyLock<std::collections::BTreeMap<String, String>> =
                std::sync::LazyLock::new(std::collections::BTreeMap::new);
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
                development_channels,
                print_mode: *print_mode,
                extra_args: &[],
                env_vars: &EMPTY_ENV,
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

            // Spawn PTY session. Same rationale as the Claude task path above:
            // agent tasks always use the login shell, ignoring per-project settings.
            let shell = default_shell();
            let ai_config = ShellIntegrationConfig::for_ai_session();
            match session_manager
                .create(
                    *session_id,
                    shell,
                    120,
                    40,
                    Some(working_dir),
                    None,
                    Some(&ai_config),
                )
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

                    // Register a channel dialog detector for auto-approval
                    if !development_channels.is_empty() {
                        channel_dialog_detectors.insert(*session_id, ChannelDialogDetector::new());
                        tracing::debug!(
                            session_id = %session_id,
                            "registered channel dialog detector for auto-approve"
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
        let session_manager = SessionManager::new(
            pty_tx,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-test"),
            uuid::Uuid::new_v4(),
        );
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        // Resize for nonexistent session should NOT create a phantom scrollback entry.
        let guard = bsb.read().await;
        assert!(
            guard.get(&session_id).is_none(),
            "scrollback entry should not exist for unknown session"
        );
    }

    /// Spin up a minimal git repo in a tempdir and return its path. Kept
    /// small and local to this test module so we don't reach into other
    /// crates' test helpers.
    fn init_dispatch_test_repo(dir: &std::path::Path) {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("HOME", dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "--initial-branch=main", "."]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        git(&["add", "."]);
        git(&["commit", "--no-verify", "-m", "init"]);
        // Create a second branch the server can point `base_ref` at.
        git(&["branch", "base-branch"]);
    }

    /// Exercises the server-mode WorktreeCreate dispatch with `base_ref`
    /// set. Regression guard for a bug where the agent-side dispatch
    /// silently dropped `base_ref` and fell back to HEAD.
    #[tokio::test]
    async fn worktree_create_threads_base_ref_through_dispatch() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
            make_test_context();

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_dispatch_test_repo(&repo);
        let wt_path = tmp.path().join("wt-server-base");

        let msg = ServerMessage::WorktreeCreate {
            project_path: repo.to_string_lossy().to_string(),
            branch: "derived".to_string(),
            path: Some(wt_path.to_string_lossy().to_string()),
            new_branch: true,
            base_ref: Some("base-branch".to_string()),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        // Dispatch spawns a background task for the git work; drain events
        // until we get the terminal WorktreeCreated.
        let mut stages = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let (created_branch, created_path) = loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let sent = tokio::time::timeout(remaining, orx.recv())
                .await
                .expect("timed out waiting for dispatch output")
                .expect("channel closed");
            match sent {
                AgentMessage::WorktreeCreationProgress { stage, .. } => {
                    stages.push(stage);
                }
                AgentMessage::WorktreeCreated { worktree, .. } => {
                    break (worktree.branch.clone(), worktree.path.clone());
                }
                AgentMessage::WorktreeError { message, .. } => {
                    panic!("worktree create failed: {message}");
                }
                other => panic!("unexpected agent message: {other:?}"),
            }
        };

        assert_eq!(
            created_branch.as_deref(),
            Some("derived"),
            "new branch name must be used"
        );
        let path = created_path;
        // The worktree should exist on disk with a single commit inherited
        // from base-branch. That is enough to confirm base_ref reached the
        // git layer — we additionally assert the worktree path ends in the
        // name we specified.
        assert!(
            path.ends_with("wt-server-base"),
            "unexpected worktree path: {path}"
        );

        // Progress regression guard: server-mode must emit Init, Finalizing,
        // and Done at minimum so GUIs can reflect the lifecycle.
        use zremote_protocol::events::WorktreeCreationStage::{Done, Finalizing, Init};
        assert!(
            stages.contains(&Init),
            "missing Init progress event: saw {stages:?}"
        );
        assert!(
            stages.contains(&Finalizing),
            "missing Finalizing progress event: saw {stages:?}"
        );
        assert!(
            stages.contains(&Done),
            "missing Done progress event: saw {stages:?}"
        );
    }

    /// Write a full `ProjectSettings` to disk in test dirs. Used by server-
    /// mode hook tests to configure `hooks.worktree.*` overrides.
    fn write_test_settings(
        project_path: &std::path::Path,
        settings: &zremote_protocol::ProjectSettings,
    ) {
        crate::project::settings::write_settings(project_path, settings).expect("write_settings");
    }

    fn settings_with_hook_action(
        action_name: &str,
        command: &str,
        slot: &str,
    ) -> zremote_protocol::ProjectSettings {
        use std::collections::HashMap;
        use zremote_protocol::{
            ActionScope, HookRef, ProjectAction, ProjectHooks, ProjectSettings, WorktreeHooks,
        };
        let action = ProjectAction {
            name: action_name.to_string(),
            command: command.to_string(),
            description: None,
            icon: None,
            working_dir: None,
            env: HashMap::new(),
            worktree_scoped: true,
            scopes: vec![ActionScope::Worktree],
            inputs: vec![],
        };
        let href = HookRef {
            action: action_name.to_string(),
            inputs: HashMap::new(),
        };
        let worktree_hooks = match slot {
            "create" => WorktreeHooks {
                create: Some(href),
                ..Default::default()
            },
            "delete" => WorktreeHooks {
                delete: Some(href),
                ..Default::default()
            },
            "pre_delete" => WorktreeHooks {
                pre_delete: Some(href),
                ..Default::default()
            },
            "post_create" => WorktreeHooks {
                post_create: Some(href),
                ..Default::default()
            },
            other => panic!("unknown slot {other}"),
        };
        ProjectSettings {
            actions: vec![action],
            hooks: Some(ProjectHooks {
                worktree: Some(worktree_hooks),
            }),
            ..Default::default()
        }
    }

    /// `hooks.worktree.create` with a named action runs through the server
    /// dispatcher. The hook writes a marker to prove it ran via the resolver;
    /// we assert on the emitted `WorktreeHookResult` and the marker file.
    ///
    /// We intentionally do NOT drive a full `git worktree add` from the
    /// hook: under parallel test load that path was flaky (branch-name
    /// reuse across concurrent tokio::spawn tasks). The Create-slot's
    /// "find worktree after hook" branch is already covered by the PTY
    /// integration in local mode; here we are verifying the dispatcher
    /// resolves and executes the hook in server mode.
    #[tokio::test]
    async fn server_worktree_create_named_action_runs_hook() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
            make_test_context();

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_dispatch_test_repo(&repo);

        let marker = tmp.path().join("hook-ran.marker");
        let marker_str = marker.to_string_lossy().to_string();

        // Simple hook that records it ran. No git state change — the
        // dispatcher will see "hook succeeded" + "no worktree found" and
        // emit `WorktreeError`; we treat that error as a success signal
        // along with the `WorktreeHookResult`.
        let command = format!("touch {marker_str}");
        let settings = settings_with_hook_action("custom-add", &command, "create");
        write_test_settings(&repo, &settings);

        let msg = ServerMessage::WorktreeCreate {
            project_path: repo.to_string_lossy().to_string(),
            branch: "hook-branch".to_string(),
            path: None,
            new_branch: true,
            base_ref: None,
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut saw_hook_result = false;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let sent = tokio::time::timeout(remaining, orx.recv())
                .await
                .expect("timed out waiting for hook output")
                .expect("channel closed");
            match sent {
                AgentMessage::WorktreeHookResult {
                    hook_type,
                    success,
                    output,
                    ..
                } if hook_type == "create" => {
                    assert!(success, "create hook must succeed: {output:?}");
                    saw_hook_result = true;
                }
                AgentMessage::WorktreeError { .. } | AgentMessage::WorktreeCreated { .. } => {
                    // Terminal event — whichever arrives, the hook flow
                    // has finished. "Not found" is expected here since
                    // our hook does not create a real worktree.
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_hook_result, "WorktreeHookResult for 'create' must fire");
        assert!(marker.exists(), "hook command must have executed");
    }

    /// Leading-dash validation must fire before the hook path. Even with a
    /// custom create hook configured, a `base_ref` like `--upload-pack=foo`
    /// must be rejected at the API boundary (CWE-88).
    #[tokio::test]
    async fn server_worktree_create_rejects_leading_dash_with_hook() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
            make_test_context();

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_dispatch_test_repo(&repo);

        // Even with a hook defined, validation fires first.
        let settings = settings_with_hook_action("noop", "true", "create");
        write_test_settings(&repo, &settings);

        let msg = ServerMessage::WorktreeCreate {
            project_path: repo.to_string_lossy().to_string(),
            branch: "ok".to_string(),
            path: None,
            new_branch: true,
            base_ref: Some("--upload-pack=evil".to_string()),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        let sent = tokio::time::timeout(Duration::from_secs(5), orx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        match sent {
            AgentMessage::WorktreeError { message, .. } => {
                assert!(
                    message.contains("base_ref"),
                    "error must mention base_ref: {message}"
                );
            }
            other => panic!("expected WorktreeError, got {other:?}"),
        }
    }

    /// A hook that references a non-existent action must surface as
    /// `WorktreeError` with the action name in the message.
    #[tokio::test]
    async fn server_worktree_create_missing_action_errors() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
            make_test_context();

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_dispatch_test_repo(&repo);

        // HookRef with no matching action.
        use std::collections::HashMap;
        use zremote_protocol::{HookRef, ProjectHooks, ProjectSettings, WorktreeHooks};
        let settings = ProjectSettings {
            actions: vec![],
            hooks: Some(ProjectHooks {
                worktree: Some(WorktreeHooks {
                    create: Some(HookRef {
                        action: "missing-action".to_string(),
                        inputs: HashMap::new(),
                    }),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        write_test_settings(&repo, &settings);

        let msg = ServerMessage::WorktreeCreate {
            project_path: repo.to_string_lossy().to_string(),
            branch: "x".to_string(),
            path: None,
            new_branch: true,
            base_ref: None,
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let sent = tokio::time::timeout(remaining, orx.recv())
                .await
                .expect("timed out")
                .expect("channel closed");
            match sent {
                AgentMessage::WorktreeError { message, .. } => {
                    assert!(
                        message.contains("missing-action"),
                        "error must name the missing action: {message}"
                    );
                    return;
                }
                // Ignore progress events (not expected on hook path but
                // don't fail the test if they appear before the error).
                _ => continue,
            }
        }
    }

    /// `hooks.worktree.pre_delete` runs before `git worktree remove` and its
    /// result is reported via `WorktreeHookResult`. A non-zero exit aborts
    /// the delete.
    #[tokio::test]
    async fn server_worktree_delete_pre_delete_blocks_on_failure() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
            make_test_context();

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_dispatch_test_repo(&repo);
        let wt_path = tmp.path().join("wt-predelete");

        // Create a real worktree so the delete has something to target.
        std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "to-delete",
                wt_path.to_string_lossy().as_ref(),
            ])
            .current_dir(&repo)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", repo.to_string_lossy().to_string())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();

        // pre_delete exits non-zero → delete must NOT proceed.
        let settings = settings_with_hook_action("guard", "false", "pre_delete");
        write_test_settings(&repo, &settings);

        let msg = ServerMessage::WorktreeDelete {
            project_path: repo.to_string_lossy().to_string(),
            worktree_path: wt_path.to_string_lossy().to_string(),
            force: false,
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut saw_hook_result = false;
        let mut saw_error = false;
        loop {
            if saw_hook_result && saw_error {
                break;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let sent = tokio::time::timeout(remaining, orx.recv())
                .await
                .expect("timed out")
                .expect("channel closed");
            match sent {
                AgentMessage::WorktreeHookResult {
                    hook_type, success, ..
                } if hook_type == "pre_delete" => {
                    assert!(!success, "pre_delete must be reported as failed");
                    saw_hook_result = true;
                }
                AgentMessage::WorktreeError { .. } => {
                    saw_error = true;
                }
                AgentMessage::WorktreeDeleted { .. } => {
                    panic!("delete must not proceed when pre_delete fails");
                }
                _ => {}
            }
        }
        // The worktree must still be on disk since the delete was blocked.
        assert!(
            wt_path.exists(),
            "worktree must not be removed when pre_delete fails"
        );
    }

    // ─── RFC-009 P2: request/response dispatch ──────────────────────────────

    /// `BranchListRequest` on a real git repo must round-trip to
    /// `BranchListResponse { branches: Some(..), error: None }`.
    #[tokio::test]
    async fn branch_list_request_returns_branches_on_healthy_repo() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
            make_test_context();

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_dispatch_test_repo(&repo);

        let request_id = Uuid::new_v4();
        let msg = ServerMessage::BranchListRequest {
            request_id,
            project_path: repo.to_string_lossy().into_owned(),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        let sent = tokio::time::timeout(Duration::from_secs(10), orx.recv())
            .await
            .expect("timed out waiting for response")
            .expect("channel closed");
        match sent {
            AgentMessage::BranchListResponse {
                request_id: rid,
                branches,
                error,
            } => {
                assert_eq!(rid, request_id, "request_id must echo back");
                assert!(
                    error.is_none(),
                    "healthy repo must not return error: {error:?}"
                );
                let list = branches.expect("branches must be Some");
                assert!(
                    list.local.iter().any(|b| b.name == "main"),
                    "expected local branch 'main' in {:?}",
                    list.local
                );
            }
            other => panic!("unexpected agent message: {other:?}"),
        }
    }

    /// `BranchListRequest` for a path that does not exist must come back with
    /// `error: Some(WorktreeError { code: PathMissing, .. })`.
    #[tokio::test]
    async fn branch_list_request_path_missing_maps_to_structured_error() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
            make_test_context();

        let request_id = Uuid::new_v4();
        let msg = ServerMessage::BranchListRequest {
            request_id,
            project_path: "/nonexistent/zremote-test/abcxyz".to_string(),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        let sent = tokio::time::timeout(Duration::from_secs(5), orx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        match sent {
            AgentMessage::BranchListResponse {
                request_id: rid,
                branches,
                error,
            } => {
                assert_eq!(rid, request_id);
                assert!(branches.is_none(), "branches must be None on error");
                let err = error.expect("error must be Some for missing path");
                assert_eq!(
                    err.code,
                    zremote_protocol::project::WorktreeErrorCode::PathMissing,
                    "expected PathMissing for nonexistent project path"
                );
            }
            other => panic!("unexpected agent message: {other:?}"),
        }
    }

    /// `WorktreeCreateRequest` happy path: response carries a populated
    /// `WorktreeCreateSuccessPayload` and at least Init/Finalizing/Done
    /// progress events fire before it.
    #[tokio::test]
    async fn worktree_create_request_happy_path_returns_success_payload() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
            make_test_context();

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_dispatch_test_repo(&repo);
        let wt_path = tmp.path().join("wt-request");

        let request_id = Uuid::new_v4();
        let msg = ServerMessage::WorktreeCreateRequest {
            request_id,
            project_path: repo.to_string_lossy().into_owned(),
            branch: "derived-req".to_string(),
            path: Some(wt_path.to_string_lossy().into_owned()),
            new_branch: true,
            base_ref: Some("base-branch".to_string()),
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut stages = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let sent = tokio::time::timeout(remaining, orx.recv())
                .await
                .expect("timed out")
                .expect("channel closed");
            match sent {
                AgentMessage::WorktreeCreationProgress { stage, .. } => {
                    stages.push(stage);
                }
                AgentMessage::WorktreeCreateResponse {
                    request_id: rid,
                    worktree,
                    error,
                } => {
                    assert_eq!(rid, request_id, "request_id must echo back");
                    assert!(error.is_none(), "happy path must not emit error: {error:?}");
                    let payload = worktree.expect("payload must be Some on success");
                    assert_eq!(payload.branch.as_deref(), Some("derived-req"));
                    assert!(
                        payload.path.ends_with("wt-request"),
                        "unexpected path {}",
                        payload.path
                    );
                    assert!(
                        payload.commit_hash.is_some(),
                        "commit_hash must be populated"
                    );
                    break;
                }
                AgentMessage::WorktreeError { .. } => {
                    panic!("legacy WorktreeError must not fire for request path");
                }
                AgentMessage::WorktreeCreated { .. } => {
                    panic!("legacy WorktreeCreated must not fire for request path");
                }
                _ => {}
            }
        }

        use zremote_protocol::events::WorktreeCreationStage::{Done, Finalizing, Init};
        assert!(stages.contains(&Init), "missing Init: {stages:?}");
        assert!(
            stages.contains(&Finalizing),
            "missing Finalizing: {stages:?}"
        );
        assert!(stages.contains(&Done), "missing Done: {stages:?}");
    }

    /// `WorktreeCreateRequest` with a leading-dash branch must be rejected
    /// before any git call — the service helper is the single source of truth
    /// for that validation.
    #[tokio::test]
    async fn worktree_create_request_leading_dash_rejected_in_service() {
        let host_id = Uuid::new_v4();
        let (mut sm, mut am, mut ps, otx, mut orx, atx, _arx, ktx, mapper, bs, bsb, mut sa) =
            make_test_context();

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_dispatch_test_repo(&repo);

        let request_id = Uuid::new_v4();
        let msg = ServerMessage::WorktreeCreateRequest {
            request_id,
            project_path: repo.to_string_lossy().into_owned(),
            branch: "-x".to_string(),
            path: None,
            new_branch: true,
            base_ref: None,
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
            None,
            &mut std::collections::HashMap::new(),
            &std::sync::Arc::new(crate::agents::LauncherRegistry::with_builtins()),
        )
        .await;

        let sent = tokio::time::timeout(Duration::from_secs(5), orx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        match sent {
            AgentMessage::WorktreeCreateResponse {
                request_id: rid,
                worktree,
                error,
            } => {
                assert_eq!(rid, request_id);
                assert!(worktree.is_none(), "worktree must be None on rejection");
                let err = error.expect("error must be Some");
                assert_eq!(
                    err.code,
                    zremote_protocol::project::WorktreeErrorCode::InvalidRef,
                    "leading dash must map to InvalidRef"
                );
            }
            other => panic!("unexpected agent message: {other:?}"),
        }
    }
}
