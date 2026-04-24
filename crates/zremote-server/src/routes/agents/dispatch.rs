//! Message dispatch: `handle_agent_message` and its match arms for different message types.

use std::collections::HashSet;

use axum::extract::ws::WebSocket;
use chrono::Utc;
use tokio::time::Instant;
use uuid::Uuid;
use zremote_protocol::agentic::{AgenticAgentMessage, AgenticStatus};
use zremote_protocol::claude::ClaudeTaskStatus;
use zremote_protocol::status::SessionStatus;
use zremote_protocol::{AgentMessage, HostId, ServerMessage};

use crate::state::{AgenticLoopState, AppState, LoopInfo, ServerEvent, SessionInfo};

use super::send_server_message;

/// DB row for an agentic loop, enriched with the project name resolved via
/// `LEFT JOIN projects ON (host_id, path)`. `project_name` is `None` when no
/// registered project matches the loop's working directory.
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
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: Option<f64>,
    project_name: Option<String>,
}

/// Fetch a `LoopInfo` from the DB with `project_name` resolved from the
/// `projects` table (matching on the session's host_id + the loop's path).
pub(super) async fn fetch_loop_info(state: &AppState, loop_id: &str) -> Option<LoopInfo> {
    let row: LoopRow = sqlx::query_as(
        "SELECT l.id, l.session_id, l.project_path, l.tool_name, l.status, l.started_at, \
         l.ended_at, l.end_reason, l.task_name, l.input_tokens, l.output_tokens, l.cost_usd, \
         p.name AS project_name \
         FROM agentic_loops l \
         LEFT JOIN sessions s ON s.id = l.session_id \
         LEFT JOIN projects  p ON p.host_id = s.host_id AND p.path = l.project_path \
         WHERE l.id = ?",
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
        status: zremote_core::queries::loops::parse_status(&row.status),
        started_at: row.started_at,
        ended_at: row.ended_at,
        end_reason: row.end_reason,
        task_name: row.task_name,
        prompt_message: None,
        permission_mode: None,
        action_tool_name: None,
        action_description: None,
        input_tokens: row.input_tokens.cast_unsigned(),
        output_tokens: row.output_tokens.cast_unsigned(),
        cost_usd: row.cost_usd,
        channel_available: None,
        project_name: row.project_name,
    })
}

/// Upsert a worktree child project row for a successful create. Looks up the
/// parent project by `(host_id, parent_project_path)`; if missing, the upsert
/// is skipped and `Ok(None)` is returned (same behaviour as the legacy
/// `WorktreeCreated` handler -- the agent may report a worktree for a parent
/// that the server hasn't ingested yet, and we don't want to stub a bogus
/// parent row here).
///
/// Shared between the legacy fire-and-forget `WorktreeCreated` handler and the
/// RFC-009 request/response `WorktreeCreateResponse` handler so the SQL lives
/// in exactly one place.
async fn upsert_worktree_row(
    state: &AppState,
    host_id_str: &str,
    parent_project_path: &str,
    worktree_path: &str,
    branch: Option<&str>,
    commit_hash: Option<&str>,
) -> Option<String> {
    let parent: Option<(String,)> =
        sqlx::query_as("SELECT id FROM projects WHERE host_id = ? AND path = ?")
            .bind(host_id_str)
            .bind(parent_project_path)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

    let (parent_id,) = parent?;
    let wt_id = Uuid::new_v4().to_string();
    let wt_name = std::path::Path::new(worktree_path)
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
    .bind(host_id_str)
    .bind(worktree_path)
    .bind(&wt_name)
    .bind(&parent_id)
    .bind(branch)
    .bind(commit_hash)
    .execute(&state.db)
    .await
    {
        tracing::warn!(host_id = %host_id_str, path = %worktree_path, error = %e, "failed to insert worktree child");
        return None;
    }

    // On conflict (UPDATE path), the stable id is the pre-existing one, not
    // `wt_id`. Read back the authoritative id so callers get the right value.
    let row: Option<(String,)> =
        sqlx::query_as("SELECT id FROM projects WHERE host_id = ? AND path = ?")
            .bind(host_id_str)
            .bind(worktree_path)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    row.map(|(id,)| id)
}

/// Resolve a pending `BranchListRequest` oneshot with the agent's reply.
/// Extracted from `handle_agent_message` so the logic is unit-testable
/// without a WebSocket.
pub(super) fn handle_branch_list_response(
    state: &AppState,
    host_id: HostId,
    request_id: Uuid,
    branches: Option<zremote_protocol::project::BranchList>,
    error: Option<zremote_protocol::project::WorktreeError>,
) {
    let response = crate::state::BranchListResponse { branches, error };
    if let Some((_, pending)) = state.branch_list_requests.remove(&request_id) {
        // Receiver may have been dropped on timeout — swallow.
        let _ = pending.sender.send(response);
    } else {
        // Late reply (HTTP handler already timed out and reaper cleared the
        // entry) or stray message. Not an error.
        tracing::debug!(
            host_id = %host_id,
            request_id = %request_id,
            "BranchListResponse for unknown request_id; dropping (likely late reply)"
        );
    }
}

/// Resolve a pending `WorktreeCreateRequest` oneshot, upserting the worktree
/// child row into the DB and broadcasting `ProjectsUpdated` when the agent
/// reports a successful create. Extracted from `handle_agent_message` so the
/// logic is unit-testable without a WebSocket.
pub(super) async fn handle_worktree_create_response(
    state: &AppState,
    host_id: HostId,
    request_id: Uuid,
    worktree: Option<zremote_protocol::WorktreeCreateSuccessPayload>,
    error: Option<zremote_protocol::project::WorktreeError>,
) {
    let host_id_str = host_id.to_string();
    let pending = state.worktree_create_requests.remove(&request_id);

    let project_id = match (&worktree, &pending) {
        (Some(wt), Some((_, entry))) => {
            let id = upsert_worktree_row(
                state,
                &host_id_str,
                &entry.parent_project_path,
                &wt.path,
                wt.branch.as_deref(),
                wt.commit_hash.as_deref(),
            )
            .await;
            let _ = state.events.send(ServerEvent::ProjectsUpdated {
                host_id: host_id_str.clone(),
            });
            id
        }
        (Some(_), None) => {
            // Late reply after HTTP timeout: we've lost the parent project
            // context, so we can't upsert the worktree row here. The agent
            // will emit a `GitStatusUpdate` / `ProjectList` soon and the
            // reconciliation path will pick up the new child. Still
            // broadcast `ProjectsUpdated` so connected GUIs refresh.
            tracing::debug!(
                host_id = %host_id,
                request_id = %request_id,
                "WorktreeCreateResponse with success payload for unknown request_id; \
                 skipping DB upsert (no parent context), broadcasting refresh"
            );
            let _ = state.events.send(ServerEvent::ProjectsUpdated {
                host_id: host_id_str.clone(),
            });
            None
        }
        (None, _) => None,
    };

    if let Some((_, entry)) = pending {
        let response = crate::state::WorktreeCreateResponse {
            worktree,
            error,
            project_id,
        };
        let _ = entry.sender.send(response);
    }
}

/// Handle a single agent message.
#[allow(clippy::too_many_lines)]
pub(super) async fn handle_agent_message(
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
            ..
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
                session.status = SessionStatus::Active;
            }

            // Emit SessionCreated event
            let _ = state.events.send(ServerEvent::SessionCreated {
                session: SessionInfo {
                    id: session_id.to_string(),
                    host_id: host_id.to_string(),
                    shell: Some(shell.clone()),
                    status: SessionStatus::Active,
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
                    "UPDATE claude_sessions SET status = 'error', ended_at = ?, error_message = 'session closed before task started' \
                     WHERE session_id = ? AND status = 'starting'",
                )
                .bind(&now_ct)
                .bind(&session_id_str)
                .execute(&state.db)
                .await
                    && result.rows_affected() > 0
                    && let Ok(Some((task_id, cs_pp, cs_tn))) = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
                        "SELECT id, project_path, task_name FROM claude_sessions WHERE session_id = ? AND status = 'error'",
                    )
                    .bind(&session_id_str)
                    .fetch_optional(&state.db)
                    .await
                {
                    let _ = state.events.send(ServerEvent::ClaudeTaskEnded {
                        task_id,
                        status: ClaudeTaskStatus::Error,
                        summary: Some("session closed before Claude started".to_string()),
                        session_id: Some(session_id_str.clone()),
                        host_id: Some(host_id.to_string()),
                        project_path: cs_pp,
                        task_name: cs_tn,
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
                    && let Ok(Some((task_id, cs_pp, cs_tn))) = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
                        "SELECT id, project_path, task_name FROM claude_sessions WHERE session_id = ?",
                    )
                    .bind(&session_id_str)
                    .fetch_optional(&state.db)
                    .await
                {
                    let _ = state.events.send(ServerEvent::ClaudeTaskEnded {
                        task_id,
                        status: ClaudeTaskStatus::Completed,
                        summary: None,
                        session_id: Some(session_id_str.clone()),
                        host_id: Some(host_id.to_string()),
                        project_path: cs_pp,
                        task_name: cs_tn,
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
            handle_sessions_recovered(state, host_id, sessions).await;
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
            main_repo_path,
        } => {
            let host_id_str = host_id.to_string();
            let project_id = Uuid::new_v4().to_string();

            let parent_project_id = if let Some(ref main_path) = main_repo_path {
                match zremote_core::queries::projects::get_project_by_host_and_path(
                    &state.db,
                    &host_id_str,
                    main_path,
                )
                .await
                {
                    Ok(parent) => Some(parent.id),
                    Err(crate::error::AppError::Database(sqlx::Error::RowNotFound)) => {
                        // Main repo not yet registered. Mirror `apply_project_list`
                        // and create a stub parent row so the worktree is still
                        // linkable — otherwise a user registering a worktree
                        // directly (via `ServerMessage::ProjectRegister`) leaves
                        // a permanent orphan.
                        let stub_id = Uuid::new_v4().to_string();
                        let stub_name = main_path
                            .rsplit('/')
                            .next()
                            .unwrap_or("unknown")
                            .to_string();
                        match zremote_core::queries::projects::insert_project(
                            &state.db,
                            &stub_id,
                            &host_id_str,
                            main_path,
                            &stub_name,
                        )
                        .await
                        {
                            Ok(_) => {
                                // Re-resolve canonical id (INSERT OR IGNORE may
                                // have skipped on concurrent insert).
                                match zremote_core::queries::projects::get_project_by_host_and_path(
                                    &state.db,
                                    &host_id_str,
                                    main_path,
                                )
                                .await
                                {
                                    Ok(row) => Some(row.id),
                                    Err(e) => {
                                        tracing::warn!(
                                            host_id = %host_id,
                                            main_repo_path = %main_path,
                                            error = %e,
                                            "failed to resolve stubbed parent id"
                                        );
                                        None
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    host_id = %host_id,
                                    main_repo_path = %main_path,
                                    error = %e,
                                    "failed to stub parent project for worktree"
                                );
                                None
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            host_id = %host_id,
                            path = %path,
                            main_repo_path = %main_path,
                            error = %e,
                            "transient error resolving parent for worktree"
                        );
                        None
                    }
                }
            } else {
                None
            };

            let effective_type = if parent_project_id.is_some() {
                "worktree"
            } else {
                project_type.as_str()
            };

            if let Err(e) = sqlx::query(
                "INSERT INTO projects (id, host_id, path, name, has_claude_config, has_zremote_config, project_type, parent_project_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(host_id, path) DO UPDATE SET \
                 name = excluded.name, has_claude_config = excluded.has_claude_config, \
                 has_zremote_config = excluded.has_zremote_config, \
                 project_type = excluded.project_type, \
                 parent_project_id = COALESCE(excluded.parent_project_id, projects.parent_project_id)",
            )
            .bind(&project_id)
            .bind(&host_id_str)
            .bind(&path)
            .bind(&name)
            .bind(has_claude_config)
            .bind(has_zremote_config)
            .bind(effective_type)
            .bind(parent_project_id.as_deref())
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
            let _ = upsert_worktree_row(
                state,
                &host_id_str,
                &project_path,
                &worktree.path,
                worktree.branch.as_deref(),
                worktree.commit_hash.as_deref(),
            )
            .await;

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
        AgentMessage::WorktreeCreationProgress {
            project_path,
            job_id,
            stage,
            percent,
            message,
        } => {
            // Resolve the project's UUID for this host so GUIs can match
            // the event against the open modal's `parent_project_id`
            // (which is always a UUID). Falls back to `project_path`
            // only when the project row doesn't exist yet — e.g., early
            // `Init` before the row is upserted. This fallback still
            // won't match in the GUI but keeps the event flowing for
            // debugging / future path-aware consumers.
            let host_id_str = host_id.to_string();
            let project_id: String = sqlx::query_scalar::<_, String>(
                "SELECT id FROM projects WHERE host_id = ? AND path = ?",
            )
            .bind(&host_id_str)
            .bind(&project_path)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .unwrap_or(project_path);
            let _ = state.events.send(ServerEvent::WorktreeCreationProgress {
                project_id,
                job_id,
                stage,
                percent,
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
            apply_project_list(state, host_id, &projects).await;
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
            if let Some((_, pending)) = state.directory_requests.remove(&request_id) {
                let _ = pending
                    .sender
                    .send(crate::state::DirectoryListingResponse { entries, error });
            } else {
                tracing::warn!(request_id = %request_id, "no pending request for directory listing");
            }
        }
        AgentMessage::ProjectSettingsResult {
            request_id,
            settings,
            error,
        } => {
            if let Some((_, pending)) = state.settings_get_requests.remove(&request_id) {
                let _ = pending
                    .sender
                    .send(crate::state::SettingsGetResponse { settings, error });
            } else {
                tracing::warn!(request_id = %request_id, "no pending request for settings get");
            }
        }
        AgentMessage::ProjectSettingsSaved { request_id, error } => {
            if let Some((_, pending)) = state.settings_save_requests.remove(&request_id) {
                let _ = pending
                    .sender
                    .send(crate::state::SettingsSaveResponse { error });
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
            if let Some((_, pending)) = state.action_inputs_requests.remove(&request_id) {
                let _ = pending
                    .sender
                    .send(crate::state::ActionInputsResolveResponse { inputs, error });
            } else {
                tracing::warn!(
                    request_id = %request_id,
                    "received ActionInputsResolved for unknown request"
                );
            }
        }
        AgentMessage::ChannelAction(action) => {
            use zremote_protocol::channel::ChannelAgentAction;
            let host_id_str = host_id.to_string();
            match action {
                ChannelAgentAction::WorkerResponse {
                    session_id,
                    response,
                } => {
                    if let zremote_protocol::channel::ChannelResponse::Reply {
                        ref message,
                        ref metadata,
                    } = response
                    {
                        let _ =
                            state
                                .events
                                .send(zremote_protocol::ServerEvent::ChannelWorkerReply {
                                    session_id: session_id.to_string(),
                                    host_id: host_id_str,
                                    message: message.clone(),
                                    metadata: metadata.clone(),
                                });
                    }
                    tracing::info!(
                        session = %session_id,
                        ?response,
                        "channel worker response"
                    );
                }
                ChannelAgentAction::PermissionRequest {
                    session_id,
                    request_id,
                    tool_name,
                    ..
                } => {
                    let _ = state.events.send(
                        zremote_protocol::ServerEvent::ChannelPermissionRequested {
                            session_id: session_id.to_string(),
                            host_id: host_id_str,
                            request_id: request_id.clone(),
                            tool_name: tool_name.clone(),
                        },
                    );
                    tracing::info!(
                        session = %session_id,
                        request_id,
                        tool_name,
                        "channel permission request"
                    );
                }
                ChannelAgentAction::ChannelStatus {
                    session_id,
                    available,
                } => {
                    tracing::info!(
                        session = %session_id,
                        available,
                        "channel status update"
                    );
                }
            }
        }
        AgentMessage::AgentLifecycle(lifecycle) => {
            use zremote_protocol::agents::AgentLifecycleMessage;
            match lifecycle {
                AgentLifecycleMessage::Started {
                    session_id,
                    task_id,
                    agent_kind,
                } => {
                    tracing::info!(
                        host_id = %host_id,
                        session_id = %session_id,
                        task_id = %task_id,
                        agent_kind = %agent_kind,
                        "agent launcher started"
                    );
                }
                AgentLifecycleMessage::StartFailed {
                    session_id,
                    task_id,
                    agent_kind,
                    error,
                } => {
                    tracing::warn!(
                        host_id = %host_id,
                        session_id = %session_id,
                        task_id = %task_id,
                        agent_kind = %agent_kind,
                        error = %error,
                        "agent launcher failed"
                    );
                    // Mark the session row as errored so the UI sees the
                    // failure instead of a dangling `creating` row. The
                    // `sessions` table has no `updated_at` column — we use
                    // `closed_at` instead. `mark_session_error` is a no-op
                    // when the row does not exist (the agent may reject
                    // the spawn before the server committed the row).
                    if let Err(e) =
                        zremote_core::queries::sessions::mark_session_error(&state.db, &session_id)
                            .await
                    {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "failed to mark session as errored"
                        );
                    }
                }
            }
        }
        AgentMessage::BranchListResponse {
            request_id,
            branches,
            error,
        } => {
            handle_branch_list_response(state, host_id, request_id, branches, error);
        }
        AgentMessage::WorktreeCreateResponse {
            request_id,
            worktree,
            error,
        } => {
            handle_worktree_create_response(state, host_id, request_id, worktree, error).await;
        }
    }
    Ok(())
}

/// Handle `SessionsRecovered` message: resume recovered sessions and suspend unrecovered ones.
///
/// Extracted from `handle_agent_message` for testability.
pub(super) async fn handle_sessions_recovered(
    state: &AppState,
    host_id: HostId,
    sessions: Vec<zremote_protocol::RecoveredSession>,
) {
    tracing::info!(
        host_id = %host_id,
        count = sessions.len(),
        "agent reported recovered sessions"
    );

    let now = chrono::Utc::now().to_rfc3339();

    // Get all non-closed sessions for this host from BOTH in-memory store and DB.
    let host_session_ids: Vec<uuid::Uuid> = {
        let mut ids: HashSet<uuid::Uuid> = {
            let sessions_store = state.sessions.read().await;
            sessions_store
                .iter()
                .filter(|(_, s)| s.host_id == host_id && s.status != SessionStatus::Closed)
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

    let recovered_ids: HashSet<uuid::Uuid> = sessions.iter().map(|s| s.session_id).collect();

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
            session.status = SessionStatus::Active;
            // Notify connected browsers
            let resume_msg = crate::state::BrowserMessage::SessionResumed;
            session
                .browser_senders
                .retain(|sender| match sender.try_send(resume_msg.clone()) {
                    Ok(()) => true,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                });
        } else {
            // Session was not in memory (e.g., server restarted too). Create it.
            sessions_store.insert(
                recovered.session_id,
                crate::state::SessionState::new(recovered.session_id, host_id),
            );
            if let Some(session) = sessions_store.get_mut(&recovered.session_id) {
                session.status = SessionStatus::Active;
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

        // Resume linked claude_sessions that were suspended during disconnect
        if let Err(e) = sqlx::query(
            "UPDATE claude_sessions SET status = 'active', disconnect_reason = NULL \
             WHERE session_id = ? AND status = 'suspended'",
        )
        .bind(recovered.session_id.to_string())
        .execute(&state.db)
        .await
        {
            tracing::error!(session_id = %recovered.session_id, error = %e, "failed to resume claude session on reconnect");
        }
    }

    // Suspend sessions that were NOT recovered by the agent
    // (daemon may still be alive, so don't close them permanently)
    for sid in &host_session_ids {
        if !recovered_ids.contains(sid) {
            // Skip sessions already suspended to avoid duplicate browser
            // notifications and unnecessary suspended_at overwrites.
            let already_suspended = {
                let sessions_store = state.sessions.read().await;
                sessions_store
                    .get(sid)
                    .is_some_and(|s| s.status == SessionStatus::Suspended)
            };
            if already_suspended {
                tracing::debug!(session_id = %sid, "session already suspended, skipping");
                continue;
            }

            let sid_str = sid.to_string();

            // Update DB: mark as suspended (only if not already suspended)
            if let Err(e) = sqlx::query(
                "UPDATE sessions SET status = 'suspended', suspended_at = ? WHERE id = ? AND status != 'suspended'",
            )
            .bind(&now)
            .bind(&sid_str)
            .execute(&state.db)
            .await
            {
                tracing::error!(session_id = %sid, error = %e, "failed to suspend unrecovered session in DB");
            }

            // Update in-memory status + notify browsers
            {
                let mut sessions_store = state.sessions.write().await;
                if let Some(session) = sessions_store.get_mut(sid) {
                    session.status = SessionStatus::Suspended;
                    let suspend_msg = crate::state::BrowserMessage::SessionSuspended;
                    session.browser_senders.retain(|sender| {
                        match sender.try_send(suspend_msg.clone()) {
                            Ok(()) => true,
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
                        }
                    });
                }
            }

            // Emit SessionSuspended event
            let _ = state
                .events
                .send(crate::state::ServerEvent::SessionSuspended {
                    session_id: sid_str,
                });

            tracing::info!(session_id = %sid, "suspended unrecovered session (daemon may still be alive)");
        }
    }
}

/// Apply an agent-reported `ProjectList` to the server DB.
///
/// Partitions into main repos and linked worktrees (by `main_repo_path`).
/// Main repos are inserted first so worktrees can resolve their
/// `parent_project_id`. If a worktree's main repo isn't reported in the same
/// batch, a stub parent row is created.
///
/// Extracted from `handle_agent_message` for testability (no WebSocket required).
pub(super) async fn apply_project_list(
    state: &AppState,
    host_id: HostId,
    projects: &[zremote_protocol::ProjectInfo],
) {
    let host_id_str = host_id.to_string();
    tracing::info!(host_id = %host_id, count = projects.len(), "received project list");

    let (worktrees, main_repos): (Vec<_>, Vec<_>) = projects
        .iter()
        .partition(|info| info.main_repo_path.is_some());

    for project in &main_repos {
        let project_id = Uuid::new_v4().to_string();
        if let Err(e) = zremote_core::queries::projects::insert_project(
            &state.db,
            &project_id,
            &host_id_str,
            &project.path,
            &project.name,
        )
        .await
        {
            tracing::warn!(host_id = %host_id, path = %project.path, error = %e, "failed to insert main project");
            continue;
        }
        match zremote_core::queries::projects::get_project_by_host_and_path(
            &state.db,
            &host_id_str,
            &project.path,
        )
        .await
        {
            Ok(row) => {
                if let Err(e) = zremote_core::queries::projects::update_project_metadata_from_info(
                    &state.db, &row.id, project,
                )
                .await
                {
                    tracing::warn!(host_id = %host_id, path = %project.path, error = %e, "failed to update project metadata");
                }
            }
            Err(e) => {
                tracing::warn!(host_id = %host_id, path = %project.path, error = %e, "failed to resolve main project row");
            }
        }

        // Legacy path: agents that still ship worktrees inside the parent's
        // `worktrees` array (without main_repo_path on sibling entries).
        if !project.worktrees.is_empty() {
            upsert_worktree_children(state, &host_id_str, &project.path, &project.worktrees).await;
        }
    }

    for project in &worktrees {
        let Some(main_path) = project.main_repo_path.as_deref() else {
            continue;
        };
        let parent_id = match zremote_core::queries::projects::get_project_by_host_and_path(
            &state.db,
            &host_id_str,
            main_path,
        )
        .await
        {
            Ok(row) => row.id,
            Err(crate::error::AppError::Database(sqlx::Error::RowNotFound)) => {
                let stub_id = Uuid::new_v4().to_string();
                let stub_name = main_path
                    .rsplit('/')
                    .next()
                    .unwrap_or("unknown")
                    .to_string();
                if let Err(e) = zremote_core::queries::projects::insert_project(
                    &state.db,
                    &stub_id,
                    &host_id_str,
                    main_path,
                    &stub_name,
                )
                .await
                {
                    tracing::warn!(host_id = %host_id, path = %main_path, error = %e, "failed to stub parent project");
                    continue;
                }
                match zremote_core::queries::projects::get_project_by_host_and_path(
                    &state.db,
                    &host_id_str,
                    main_path,
                )
                .await
                {
                    Ok(row) => row.id,
                    Err(e) => {
                        tracing::warn!(host_id = %host_id, path = %main_path, error = %e, "failed to resolve stubbed parent");
                        continue;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(host_id = %host_id, path = %main_path, error = %e, "failed to resolve parent for worktree");
                continue;
            }
        };

        let wt_id = Uuid::new_v4().to_string();
        if let Err(e) = zremote_core::queries::projects::insert_project_with_parent(
            &state.db,
            &wt_id,
            &host_id_str,
            &project.path,
            &project.name,
            Some(&parent_id),
            "worktree",
        )
        .await
        {
            tracing::warn!(host_id = %host_id, path = %project.path, error = %e, "failed to insert worktree project");
            continue;
        }

        // Resolve canonical id (may be a pre-existing row) and ensure parent
        // linkage + metadata.
        match zremote_core::queries::projects::get_project_by_host_and_path(
            &state.db,
            &host_id_str,
            &project.path,
        )
        .await
        {
            Ok(row) => {
                // Only set the parent linkage when the row has none yet. This
                // avoids silently re-linking a worktree row whose parent was
                // set deliberately (manual fix, upsert_worktree_children path,
                // or a previous scan) to a different project.
                if row.parent_project_id.is_none()
                    && let Err(e) = zremote_core::queries::projects::set_parent_project_id(
                        &state.db, &row.id, &parent_id, "worktree",
                    )
                    .await
                {
                    tracing::warn!(host_id = %host_id, path = %project.path, error = %e, "failed to link worktree parent");
                }
                if let Err(e) = zremote_core::queries::projects::update_project_metadata_from_info(
                    &state.db, &row.id, project,
                )
                .await
                {
                    tracing::warn!(host_id = %host_id, path = %project.path, error = %e, "failed to update worktree metadata");
                }
            }
            Err(e) => {
                tracing::warn!(host_id = %host_id, path = %project.path, error = %e, "failed to resolve worktree row");
            }
        }
    }
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

/// Handle an agentic agent message: update DB and in-memory state.
#[allow(clippy::too_many_lines)]
pub(super) async fn handle_agentic_message(
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
                    permission_mode: None,
                    last_updated: Instant::now(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cost_usd: None,
                },
            );

            tracing::info!(host_id = %host_id, loop_id = %loop_id, tool_name = %tool_name, "agentic loop detected");

            // Link loop to claude_session if one exists, or auto-create one for manually-started sessions
            let link_result = sqlx::query(
                "UPDATE claude_sessions SET loop_id = ?, status = 'active', disconnect_reason = NULL WHERE session_id = ? AND status IN ('starting', 'suspended', 'active') AND loop_id IS NULL",
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
                    status: ClaudeTaskStatus::Active,
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
            prompt_message,
            permission_mode,
            action_tool_name,
            action_description,
        } => {
            // Update in-memory state
            if let Some(mut entry) = state.agentic_loops.get_mut(&loop_id) {
                entry.status = status;
                if task_name.is_some() {
                    entry.task_name = task_name.clone();
                }
                if permission_mode.is_some() {
                    entry.permission_mode.clone_from(&permission_mode);
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
            if let Some(mut loop_info) = fetch_loop_info(state, &loop_id_str).await {
                // Overlay transient fields (not stored in DB)
                loop_info.prompt_message = prompt_message;
                loop_info.permission_mode = permission_mode.or_else(|| {
                    state
                        .agentic_loops
                        .get(&loop_id)
                        .and_then(|e| e.permission_mode.clone())
                });
                loop_info.action_tool_name = action_tool_name;
                loop_info.action_description = action_description;
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
            if let Ok(Some((task_id, cs_sid, cs_pp, cs_tn))) =
                sqlx::query_as::<_, (String, String, Option<String>, Option<String>)>(
                    "SELECT id, session_id, project_path, task_name FROM claude_sessions WHERE loop_id = ?",
                )
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
                    status: ClaudeTaskStatus::Completed,
                    summary: None,
                    session_id: Some(cs_sid),
                    host_id: Some(host_id.to_string()),
                    project_path: cs_pp,
                    task_name: cs_tn,
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
        AgenticAgentMessage::LoopMetricsUpdate {
            loop_id,
            input_tokens,
            output_tokens,
            cost_usd,
        } => {
            let loop_id_str = loop_id.to_string();

            // Update DB
            if let Err(e) = sqlx::query(
                "UPDATE agentic_loops SET input_tokens = ?1, output_tokens = ?2, cost_usd = ?3 WHERE id = ?4",
            )
            .bind(input_tokens.cast_signed())
            .bind(output_tokens.cast_signed())
            .bind(cost_usd)
            .bind(&loop_id_str)
            .execute(&state.db)
            .await
            {
                tracing::warn!(loop_id = %loop_id, error = %e, "failed to update loop metrics in DB");
            }

            // Update in-memory state
            if let Some(mut entry) = state.agentic_loops.get_mut(&loop_id) {
                entry.input_tokens = input_tokens;
                entry.output_tokens = output_tokens;
                entry.cost_usd = cost_usd;
                entry.last_updated = Instant::now();
            }

            // Broadcast event with full loop info
            let hostname = state
                .connections
                .get_hostname(&host_id)
                .await
                .unwrap_or_default();
            if let Some(loop_info) = fetch_loop_info(state, &loop_id_str).await {
                let _ = state.events.send(ServerEvent::LoopMetricsUpdated {
                    loop_info,
                    host_id: host_id.to_string(),
                    hostname,
                });
            }
        }
        AgenticAgentMessage::ExecutionNode {
            session_id,
            loop_id,
            timestamp,
            kind,
            input,
            output_summary,
            exit_code,
            working_dir,
            duration_ms,
        } => {
            let session_id_str = session_id.to_string();
            let loop_id_str = loop_id.map(|id| id.to_string());

            let node_id = match zremote_core::queries::execution_nodes::insert_execution_node(
                &state.db,
                &session_id_str,
                loop_id_str.as_deref(),
                timestamp,
                &kind,
                input.as_deref(),
                output_summary.as_deref(),
                exit_code,
                &working_dir,
                duration_ms,
            )
            .await
            {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to insert execution node");
                    return Ok(());
                }
            };

            // Enforce cap
            let _ = zremote_core::queries::execution_nodes::enforce_session_node_cap(
                &state.db,
                &session_id_str,
                10_000,
            )
            .await;

            let _ = state.events.send(ServerEvent::ExecutionNodeCreated {
                session_id: session_id_str,
                host_id: host_id.to_string(),
                node_id,
                loop_id: loop_id_str,
                timestamp,
                kind,
                input,
                output_summary,
                exit_code,
                working_dir,
                duration_ms,
            });
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

            // Fetch context before marking as error.
            let ctx: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
                "SELECT session_id, project_path, task_name FROM claude_sessions WHERE id = ?",
            )
            .bind(&task_id_str)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

            if let Err(e) = sqlx::query(
                "UPDATE claude_sessions SET status = 'error', ended_at = ?, error_message = ? WHERE id = ?",
            )
            .bind(&now)
            .bind(&error)
            .bind(&task_id_str)
            .execute(&state.db)
            .await
            {
                tracing::error!(task_id = %task_id_str, error = %e, "failed to update claude task status");
            }

            let (cs_sid, cs_pp, cs_tn) =
                ctx.map_or((None, None, None), |(s, p, t)| (Some(s), p, t));
            let _ = state.events.send(ServerEvent::ClaudeTaskEnded {
                task_id: task_id_str,
                status: ClaudeTaskStatus::Error,
                summary: Some(error),
                session_id: cs_sid,
                host_id: Some(host_id.to_string()),
                project_path: cs_pp,
                task_name: cs_tn,
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
            if let Some((_, pending)) = state.claude_discover_requests.remove(&request_key) {
                let _ = pending.sender.send(sessions);
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
            permission_mode,
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
                        permission_mode,
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
            if let Some((_, pending)) = state.knowledge_requests.remove(&request_id) {
                let _ = pending.sender.send(KnowledgeAgentMessage::SearchResults {
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
                                tracing::debug!(key = %memory.key, "skipping memory with lower confidence");
                            }
                            None => {
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

                    if let Err(e) = sqlx::query(
                        "UPDATE knowledge_bases SET memories_since_regen = memories_since_regen + ? WHERE host_id = ?"
                    )
                    .bind(i64::from(memory_count))
                    .bind(&host_id_str)
                    .execute(&state.db)
                    .await {
                        tracing::warn!(error = %e, "failed to increment memories_since_regen");
                    }

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
                        let _ = sender.send(zremote_protocol::ServerMessage::KnowledgeAction(
                            zremote_protocol::knowledge::KnowledgeServerMessage::WriteClaudeMd {
                                project_path: path.clone(),
                                content: String::new(),
                                mode: zremote_protocol::knowledge::WriteMdMode::Section,
                            }
                        )).await;

                        let _ = sender.send(zremote_protocol::ServerMessage::KnowledgeAction(
                            zremote_protocol::knowledge::KnowledgeServerMessage::GenerateSkills {
                                project_path: path.clone(),
                            }
                        )).await;

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
            let request_id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("instructions:{host_id_str}:{project_path}").as_bytes(),
            );

            if let Some((_, pending)) = state.knowledge_requests.remove(&request_id) {
                let _ = pending
                    .sender
                    .send(KnowledgeAgentMessage::InstructionsGenerated {
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

            if let Some((_, pending)) = state.knowledge_requests.remove(&request_id) {
                let _ = pending.sender.send(KnowledgeAgentMessage::ClaudeMdWritten {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use uuid::Uuid;
    use zremote_protocol::agentic::{AgenticAgentMessage, AgenticStatus};
    use zremote_protocol::status::SessionStatus;

    use crate::state::{AppState, ConnectionManager};

    use super::super::lifecycle::{cleanup_agent, upsert_host};

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
            branch_list_requests: Arc::new(dashmap::DashMap::new()),
            worktree_create_requests: Arc::new(dashmap::DashMap::new()),
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
             VALUES (?, ?, '/tmp/project', 'bash', 'working')",
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
        assert_eq!(info.status, zremote_protocol::AgenticStatus::Working);
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
            prompt_message: None,
            permission_mode: None,
            action_tool_name: None,
            action_description: None,
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
            sessions.get_mut(&session_id).unwrap().status = SessionStatus::Active;
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
            sessions.get_mut(&session_id).unwrap().status = SessionStatus::Active;
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
            assert_eq!(session.status, SessionStatus::Suspended);
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

    // ── handle_sessions_recovered ──

    #[tokio::test]
    async fn sessions_recovered_suspends_unrecovered_sessions() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let recovered_sid = Uuid::new_v4();
        let unrecovered_sid = Uuid::new_v4();

        insert_test_host(&state, &host_id.to_string(), "recover-host").await;
        insert_test_session(
            &state,
            &recovered_sid.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;
        insert_test_session(
            &state,
            &unrecovered_sid.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        // Add both sessions to in-memory store
        {
            let mut sessions = state.sessions.write().await;
            let mut s1 = zremote_core::state::SessionState::new(recovered_sid, host_id);
            s1.status = SessionStatus::Active;
            sessions.insert(recovered_sid, s1);
            let mut s2 = zremote_core::state::SessionState::new(unrecovered_sid, host_id);
            s2.status = SessionStatus::Active;
            sessions.insert(unrecovered_sid, s2);
        }

        // Subscribe to events before calling the handler
        let mut events_rx = state.events.subscribe();

        // Only recover one session
        let recovered = vec![zremote_protocol::RecoveredSession {
            session_id: recovered_sid,
            shell: "/bin/bash".to_string(),
            pid: 1234,
        }];

        super::handle_sessions_recovered(&state, host_id, recovered).await;

        // Unrecovered session should be suspended in DB
        let row: (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(unrecovered_sid.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(
            row.0, "suspended",
            "unrecovered session should be suspended in DB"
        );

        // Unrecovered session should still be in memory with Suspended status
        {
            let sessions = state.sessions.read().await;
            let session = sessions
                .get(&unrecovered_sid)
                .expect("unrecovered session should still exist in memory (not removed)");
            assert_eq!(session.status, SessionStatus::Suspended);
        }

        // Recovered session should be active in DB
        let row: (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(recovered_sid.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(row.0, "active", "recovered session should be active in DB");

        // Recovered session should be active in memory
        {
            let sessions = state.sessions.read().await;
            let session = sessions
                .get(&recovered_sid)
                .expect("recovered session should exist");
            assert_eq!(session.status, SessionStatus::Active);
        }

        // Verify events: should see SessionResumed and SessionSuspended, but NOT SessionClosed
        let mut saw_resumed = false;
        let mut saw_suspended = false;
        let mut saw_closed = false;
        // Drain all available events
        while let Ok(event) = events_rx.try_recv() {
            match &event {
                crate::state::ServerEvent::SessionResumed { session_id }
                    if *session_id == recovered_sid.to_string() =>
                {
                    saw_resumed = true;
                }
                crate::state::ServerEvent::SessionSuspended { session_id }
                    if *session_id == unrecovered_sid.to_string() =>
                {
                    saw_suspended = true;
                }
                crate::state::ServerEvent::SessionClosed { .. } => {
                    saw_closed = true;
                }
                _ => {}
            }
        }
        assert!(
            saw_resumed,
            "should emit SessionResumed for recovered session"
        );
        assert!(
            saw_suspended,
            "should emit SessionSuspended for unrecovered session"
        );
        assert!(
            !saw_closed,
            "should NOT emit SessionClosed for unrecovered session"
        );
    }

    #[tokio::test]
    async fn sessions_recovered_then_resumed_on_second_recovery() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();

        insert_test_host(&state, &host_id.to_string(), "resume-host").await;
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
            let mut s = zremote_core::state::SessionState::new(session_id, host_id);
            s.status = SessionStatus::Active;
            sessions.insert(session_id, s);
        }

        // First recovery: session is NOT in the recovered list -> should be suspended
        super::handle_sessions_recovered(&state, host_id, vec![]).await;

        // Verify suspended state
        {
            let sessions = state.sessions.read().await;
            let session = sessions.get(&session_id).expect("session should exist");
            assert_eq!(
                session.status,
                SessionStatus::Suspended,
                "session should be suspended after first recovery"
            );
        }
        let row: (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(row.0, "suspended");

        // Second recovery: session IS in the recovered list -> should be resumed
        let recovered = vec![zremote_protocol::RecoveredSession {
            session_id,
            shell: "/bin/zsh".to_string(),
            pid: 5678,
        }];

        super::handle_sessions_recovered(&state, host_id, recovered).await;

        // Verify active state
        {
            let sessions = state.sessions.read().await;
            let session = sessions.get(&session_id).expect("session should exist");
            assert_eq!(
                session.status,
                SessionStatus::Active,
                "session should be active after second recovery"
            );
        }
        let row: (String,) = sqlx::query_as("SELECT status FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(row.0, "active");
    }

    // ── claude_sessions resilience ──

    async fn insert_test_claude_session(
        state: &AppState,
        id: &str,
        session_id: &str,
        host_id: &str,
        status: &str,
    ) {
        sqlx::query(
            "INSERT INTO claude_sessions (id, session_id, host_id, project_path, status) VALUES (?, ?, ?, '/tmp/project', ?)",
        )
        .bind(id)
        .bind(session_id)
        .bind(host_id)
        .bind(status)
        .execute(&state.db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cleanup_persistent_agent_suspends_claude_tasks() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();

        insert_test_host(&state, &host_id.to_string(), "persist-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;
        insert_test_claude_session(
            &state,
            &task_id.to_string(),
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        // Add session to in-memory store
        {
            let mut sessions = state.sessions.write().await;
            let mut s = zremote_core::state::SessionState::new(session_id, host_id);
            s.status = SessionStatus::Active;
            sessions.insert(session_id, s);
        }

        // Register as persistent
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (_, generation) = state
            .connections
            .register(host_id, "persist-host".to_string(), tx, true)
            .await;

        cleanup_agent(&state, &host_id, generation).await;

        // Claude task should be suspended (not error)
        let row: (String, Option<String>) =
            sqlx::query_as("SELECT status, disconnect_reason FROM claude_sessions WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(
            row.0, "suspended",
            "persistent agent should suspend claude tasks"
        );
        assert_eq!(row.1.as_deref(), Some("agent_disconnected"));
    }

    #[tokio::test]
    async fn cleanup_non_persistent_agent_errors_claude_tasks() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();

        insert_test_host(&state, &host_id.to_string(), "non-persist-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;
        insert_test_claude_session(
            &state,
            &task_id.to_string(),
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        // Add session to in-memory store
        {
            let mut sessions = state.sessions.write().await;
            let mut s = zremote_core::state::SessionState::new(session_id, host_id);
            s.status = SessionStatus::Active;
            sessions.insert(session_id, s);
        }

        // Register as non-persistent
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (_, generation) = state
            .connections
            .register(host_id, "non-persist-host".to_string(), tx, false)
            .await;

        cleanup_agent(&state, &host_id, generation).await;

        // Claude task should be error (not suspended)
        let row: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status, error_message, disconnect_reason FROM claude_sessions WHERE id = ?",
        )
        .bind(task_id.to_string())
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(
            row.0, "error",
            "non-persistent agent should mark claude tasks as error"
        );
        assert_eq!(
            row.1.as_deref(),
            Some("agent disconnected while task was running")
        );
        assert_eq!(row.2.as_deref(), Some("agent_disconnected"));
    }

    #[tokio::test]
    async fn loop_detected_links_to_suspended_task() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();

        insert_test_host(&state, &host_id.to_string(), "test-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "active",
        )
        .await;

        // Insert a suspended claude_session (simulating post-disconnect state)
        sqlx::query(
            "INSERT INTO claude_sessions (id, session_id, host_id, project_path, status, disconnect_reason) \
             VALUES (?, ?, ?, '/tmp/project', 'suspended', 'agent_disconnected')",
        )
        .bind(task_id.to_string())
        .bind(session_id.to_string())
        .bind(host_id.to_string())
        .execute(&state.db)
        .await
        .unwrap();

        // Register connection so get_hostname works
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        state
            .connections
            .register(host_id, "test-host".to_string(), tx, true)
            .await;

        let msg = AgenticAgentMessage::LoopDetected {
            loop_id,
            session_id,
            project_path: "/tmp/project".to_string(),
            tool_name: "bash".to_string(),
        };

        handle_agentic_message(&state, host_id, msg).await.unwrap();

        // Claude task should now be active with loop linked and disconnect_reason cleared
        let row: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status, loop_id, disconnect_reason FROM claude_sessions WHERE id = ?",
        )
        .bind(task_id.to_string())
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(
            row.0, "active",
            "suspended task should become active on LoopDetected"
        );
        assert_eq!(
            row.1.as_deref(),
            Some(&loop_id.to_string()[..]),
            "loop_id should be linked"
        );
        assert_eq!(row.2, None, "disconnect_reason should be cleared");
    }

    #[tokio::test]
    async fn session_recovery_resumes_suspended_claude_tasks() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();

        insert_test_host(&state, &host_id.to_string(), "recover-host").await;
        insert_test_session(
            &state,
            &session_id.to_string(),
            &host_id.to_string(),
            "suspended",
        )
        .await;

        // Insert a suspended claude task linked to the session
        sqlx::query(
            "INSERT INTO claude_sessions (id, session_id, host_id, project_path, status, disconnect_reason) \
             VALUES (?, ?, ?, '/tmp/project', 'suspended', 'agent_disconnected')",
        )
        .bind(task_id.to_string())
        .bind(session_id.to_string())
        .bind(host_id.to_string())
        .execute(&state.db)
        .await
        .unwrap();

        // Add session to in-memory store as suspended
        {
            let mut sessions = state.sessions.write().await;
            let mut s = zremote_core::state::SessionState::new(session_id, host_id);
            s.status = SessionStatus::Suspended;
            sessions.insert(session_id, s);
        }

        // Recover the session
        let recovered = vec![zremote_protocol::RecoveredSession {
            session_id,
            shell: "/bin/bash".to_string(),
            pid: 1234,
        }];

        super::handle_sessions_recovered(&state, host_id, recovered).await;

        // Claude task should now be active with disconnect_reason cleared
        let row: (String, Option<String>) =
            sqlx::query_as("SELECT status, disconnect_reason FROM claude_sessions WHERE id = ?")
                .bind(task_id.to_string())
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(
            row.0, "active",
            "suspended claude task should be resumed on session recovery"
        );
        assert_eq!(
            row.1, None,
            "disconnect_reason should be cleared on recovery"
        );
    }

    // ── apply_project_list (T7: worktree parent linking) ──

    fn make_project_info(
        path: &str,
        name: &str,
        main_repo_path: Option<&str>,
    ) -> zremote_protocol::ProjectInfo {
        zremote_protocol::ProjectInfo {
            path: path.to_string(),
            name: name.to_string(),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "rust".to_string(),
            git_info: None,
            worktrees: vec![],
            frameworks: vec![],
            architecture: None,
            conventions: vec![],
            package_manager: None,
            main_repo_path: main_repo_path.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn server_add_worktree_links_to_existing_parent() {
        let state = test_state().await;
        let host_uuid = Uuid::new_v4();
        let host_id_str = host_uuid.to_string();
        insert_test_host(&state, &host_id_str, "testhost").await;

        // Pre-register the parent main repo.
        let parent_id = Uuid::new_v4().to_string();
        zremote_core::queries::projects::insert_project(
            &state.db,
            &parent_id,
            &host_id_str,
            "/home/user/repo",
            "repo",
        )
        .await
        .unwrap();

        // Send a ProjectList with the main repo and a worktree that points to it.
        let projects = vec![
            make_project_info("/home/user/repo", "repo", None),
            make_project_info("/home/user/repo-wt", "repo-wt", Some("/home/user/repo")),
        ];
        super::apply_project_list(&state, host_uuid, &projects).await;

        let parent = zremote_core::queries::projects::get_project_by_host_and_path(
            &state.db,
            &host_id_str,
            "/home/user/repo",
        )
        .await
        .unwrap();
        let wt = zremote_core::queries::projects::get_project_by_host_and_path(
            &state.db,
            &host_id_str,
            "/home/user/repo-wt",
        )
        .await
        .unwrap();

        assert_eq!(wt.parent_project_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(wt.project_type, "worktree");
    }

    #[tokio::test]
    async fn server_add_worktree_auto_registers_parent() {
        let state = test_state().await;
        let host_uuid = Uuid::new_v4();
        let host_id_str = host_uuid.to_string();
        insert_test_host(&state, &host_id_str, "testhost").await;

        // ProjectList contains ONLY the worktree — parent isn't reported in
        // this batch. The server should stub-register the parent so the
        // worktree is still linkable.
        let projects = vec![make_project_info(
            "/home/user/repo-wt",
            "repo-wt",
            Some("/home/user/repo"),
        )];
        super::apply_project_list(&state, host_uuid, &projects).await;

        let parent = zremote_core::queries::projects::get_project_by_host_and_path(
            &state.db,
            &host_id_str,
            "/home/user/repo",
        )
        .await
        .unwrap();
        let wt = zremote_core::queries::projects::get_project_by_host_and_path(
            &state.db,
            &host_id_str,
            "/home/user/repo-wt",
        )
        .await
        .unwrap();

        assert_eq!(parent.name, "repo");
        assert_eq!(wt.parent_project_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(wt.project_type, "worktree");
    }

    #[tokio::test]
    async fn server_scan_links_worktrees_to_main_repos() {
        let state = test_state().await;
        let host_uuid = Uuid::new_v4();
        let host_id_str = host_uuid.to_string();
        insert_test_host(&state, &host_id_str, "testhost").await;

        // Simulate a scan batch: one main repo + two worktrees referencing it.
        let projects = vec![
            make_project_info("/home/user/proj", "proj", None),
            make_project_info(
                "/home/user/proj-feature-a",
                "feature-a",
                Some("/home/user/proj"),
            ),
            make_project_info(
                "/home/user/proj-feature-b",
                "feature-b",
                Some("/home/user/proj"),
            ),
        ];
        super::apply_project_list(&state, host_uuid, &projects).await;

        let parent = zremote_core::queries::projects::get_project_by_host_and_path(
            &state.db,
            &host_id_str,
            "/home/user/proj",
        )
        .await
        .unwrap();
        let children = zremote_core::queries::projects::list_worktrees(&state.db, &parent.id)
            .await
            .unwrap();
        assert_eq!(children.len(), 2, "both worktrees should link to parent");

        for child in &children {
            assert_eq!(child.parent_project_id.as_deref(), Some(parent.id.as_str()));
            assert_eq!(child.project_type, "worktree");
        }

        // project_type on main repo should reflect detected language, not worktree.
        assert_eq!(parent.project_type, "rust");
    }

    // ── RFC-009 P3: handle_branch_list_response / handle_worktree_create_response ──

    async fn insert_test_project(
        state: &AppState,
        id: &str,
        host_id: &str,
        path: &str,
        name: &str,
    ) {
        sqlx::query(
            "INSERT INTO projects (id, host_id, path, name, project_type) \
             VALUES (?, ?, ?, ?, 'rust')",
        )
        .bind(id)
        .bind(host_id)
        .bind(path)
        .bind(name)
        .execute(&state.db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn handle_branch_list_response_resolves_pending_oneshot() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();

        let (tx, rx) = tokio::sync::oneshot::channel::<crate::state::BranchListResponse>();
        state
            .branch_list_requests
            .insert(request_id, crate::state::PendingRequest::new(tx));

        let branches = zremote_protocol::project::BranchList {
            local: vec![zremote_protocol::project::Branch {
                name: "main".to_string(),
                is_current: true,
                ahead: 0,
                behind: 0,
            }],
            remote: vec![],
            current: "main".to_string(),
            remote_truncated: false,
        };

        handle_branch_list_response(&state, host_id, request_id, Some(branches.clone()), None);

        let resolved = tokio::time::timeout(std::time::Duration::from_secs(1), rx)
            .await
            .expect("oneshot should be resolved")
            .expect("sender should not have been dropped");
        assert_eq!(resolved.branches.as_ref(), Some(&branches));
        assert!(resolved.error.is_none());
        assert!(
            state.branch_list_requests.get(&request_id).is_none(),
            "pending entry should be consumed"
        );
    }

    #[tokio::test]
    async fn handle_branch_list_response_unknown_request_id_is_noop() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        // No pending entry registered.
        handle_branch_list_response(&state, host_id, request_id, None, None);
        // No panic, no entry added.
        assert!(state.branch_list_requests.is_empty());
    }

    #[tokio::test]
    async fn handle_worktree_create_response_success_upserts_and_broadcasts() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let host_id_str = host_id.to_string();
        let parent_path = "/srv/repos/acme";

        insert_test_host(&state, &host_id_str, "test-host").await;
        let parent_id = Uuid::new_v4().to_string();
        insert_test_project(&state, &parent_id, &host_id_str, parent_path, "acme").await;

        let mut events_rx = state.events.subscribe();

        let request_id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel::<crate::state::WorktreeCreateResponse>();
        state.worktree_create_requests.insert(
            request_id,
            crate::state::PendingWorktreeCreate::new(tx, parent_path.to_string()),
        );

        let payload = zremote_protocol::WorktreeCreateSuccessPayload {
            path: "/srv/repos/acme-wt/feature".to_string(),
            branch: Some("feature/x".to_string()),
            commit_hash: Some("abc1234".to_string()),
            hook_result: None,
        };

        handle_worktree_create_response(&state, host_id, request_id, Some(payload.clone()), None)
            .await;

        let resolved = tokio::time::timeout(std::time::Duration::from_secs(1), rx)
            .await
            .expect("oneshot resolved")
            .expect("sender not dropped");
        assert_eq!(resolved.worktree.as_ref(), Some(&payload));
        assert!(resolved.error.is_none());
        assert!(
            resolved.project_id.is_some(),
            "upsert should have returned a project_id"
        );
        assert!(state.worktree_create_requests.is_empty());

        // DB row inserted
        let row: (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ) = sqlx::query_as(
            "SELECT id, path, git_branch, git_commit_hash, parent_project_id, project_type \
                 FROM projects WHERE host_id = ? AND path = ?",
        )
        .bind(&host_id_str)
        .bind(&payload.path)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(row.1, payload.path);
        assert_eq!(row.2.as_deref(), Some("feature/x"));
        assert_eq!(row.3.as_deref(), Some("abc1234"));
        assert_eq!(row.4.as_deref(), Some(parent_id.as_str()));
        assert_eq!(row.5, "worktree");
        assert_eq!(
            resolved.project_id.as_deref(),
            Some(row.0.as_str()),
            "returned project_id matches DB row id"
        );

        // ProjectsUpdated broadcast
        let evt = tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv())
            .await
            .expect("event received in time")
            .expect("event channel open");
        match evt {
            ServerEvent::ProjectsUpdated { host_id: h } => assert_eq!(h, host_id_str),
            other => panic!("expected ProjectsUpdated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_worktree_create_response_error_resolves_without_db_write() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let host_id_str = host_id.to_string();
        let parent_path = "/srv/repos/acme";

        insert_test_host(&state, &host_id_str, "test-host").await;
        insert_test_project(
            &state,
            &Uuid::new_v4().to_string(),
            &host_id_str,
            parent_path,
            "acme",
        )
        .await;

        let request_id = Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel::<crate::state::WorktreeCreateResponse>();
        state.worktree_create_requests.insert(
            request_id,
            crate::state::PendingWorktreeCreate::new(tx, parent_path.to_string()),
        );

        let err = zremote_protocol::project::WorktreeError::new(
            zremote_protocol::project::WorktreeErrorCode::BranchExists,
            "Pick a different branch name",
            "branch already exists",
        );
        handle_worktree_create_response(&state, host_id, request_id, None, Some(err.clone())).await;

        let resolved = tokio::time::timeout(std::time::Duration::from_secs(1), rx)
            .await
            .expect("oneshot resolved")
            .expect("sender not dropped");
        assert!(resolved.worktree.is_none());
        assert_eq!(resolved.error.as_ref(), Some(&err));
        assert!(resolved.project_id.is_none());

        // No worktree child inserted
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM projects WHERE parent_project_id IS NOT NULL")
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn handle_worktree_create_response_unknown_request_with_success_still_broadcasts() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let host_id_str = host_id.to_string();

        insert_test_host(&state, &host_id_str, "test-host").await;
        let mut events_rx = state.events.subscribe();

        // No pending entry registered -- simulates a late reply after HTTP timeout.
        let payload = zremote_protocol::WorktreeCreateSuccessPayload {
            path: "/srv/repos/acme-wt/feature".to_string(),
            branch: Some("feature/x".to_string()),
            commit_hash: Some("abc1234".to_string()),
            hook_result: None,
        };
        handle_worktree_create_response(
            &state,
            host_id,
            Uuid::new_v4(),
            Some(payload.clone()),
            None,
        )
        .await;

        // DB should NOT have the worktree row (no parent context available).
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM projects WHERE host_id = ? AND path = ?")
                .bind(&host_id_str)
                .bind(&payload.path)
                .fetch_one(&state.db)
                .await
                .unwrap();
        assert_eq!(count.0, 0, "no upsert when parent context is lost");

        // ProjectsUpdated should still fire so GUIs refresh.
        let evt = tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv())
            .await
            .expect("event received")
            .expect("channel open");
        assert!(matches!(evt, ServerEvent::ProjectsUpdated { .. }));
    }

    #[tokio::test]
    async fn handle_worktree_create_response_unknown_request_with_error_is_silent() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let host_id_str = host_id.to_string();
        insert_test_host(&state, &host_id_str, "test-host").await;

        let mut events_rx = state.events.subscribe();

        let err = zremote_protocol::project::WorktreeError::new(
            zremote_protocol::project::WorktreeErrorCode::Internal,
            "try again",
            "boom",
        );
        handle_worktree_create_response(&state, host_id, Uuid::new_v4(), None, Some(err)).await;

        // No event, no DB change.
        let got =
            tokio::time::timeout(std::time::Duration::from_millis(50), events_rx.recv()).await;
        assert!(
            got.is_err(),
            "no event should fire for late error-only replies"
        );
    }

    // Regression: the legacy WorktreeCreated handler still works after the
    // SQL was extracted into `upsert_worktree_row`.
    #[tokio::test]
    async fn legacy_worktree_created_still_upserts_via_shared_helper() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let host_id_str = host_id.to_string();
        let parent_path = "/srv/repos/acme";

        insert_test_host(&state, &host_id_str, "test-host").await;
        let parent_id = Uuid::new_v4().to_string();
        insert_test_project(&state, &parent_id, &host_id_str, parent_path, "acme").await;

        let wt_path = "/srv/repos/acme-wt/legacy";
        let project_id = upsert_worktree_row(
            &state,
            &host_id_str,
            parent_path,
            wt_path,
            Some("legacy-branch"),
            Some("deadbee"),
        )
        .await;
        assert!(project_id.is_some());

        let row: (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ) = sqlx::query_as(
            "SELECT id, git_branch, git_commit_hash, parent_project_id, project_type \
                 FROM projects WHERE host_id = ? AND path = ?",
        )
        .bind(&host_id_str)
        .bind(wt_path)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(row.1.as_deref(), Some("legacy-branch"));
        assert_eq!(row.2.as_deref(), Some("deadbee"));
        assert_eq!(row.3.as_deref(), Some(parent_id.as_str()));
        assert_eq!(row.4, "worktree");
    }

    #[tokio::test]
    async fn upsert_worktree_row_without_parent_returns_none() {
        let state = test_state().await;
        let host_id_str = Uuid::new_v4().to_string();

        // No parent project row in DB — helper should skip and return None.
        let result = upsert_worktree_row(
            &state,
            &host_id_str,
            "/nonexistent/parent",
            "/nonexistent/parent-wt/x",
            Some("b"),
            Some("c"),
        )
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn upsert_worktree_row_conflict_updates_existing_branch_and_hash() {
        let state = test_state().await;
        let host_id = Uuid::new_v4();
        let host_id_str = host_id.to_string();
        let parent_path = "/srv/repos/acme";

        insert_test_host(&state, &host_id_str, "test-host").await;
        let parent_id = Uuid::new_v4().to_string();
        insert_test_project(&state, &parent_id, &host_id_str, parent_path, "acme").await;

        let wt_path = "/srv/repos/acme-wt/a";

        let first_id = upsert_worktree_row(
            &state,
            &host_id_str,
            parent_path,
            wt_path,
            Some("v1"),
            Some("h1"),
        )
        .await
        .unwrap();
        let second_id = upsert_worktree_row(
            &state,
            &host_id_str,
            parent_path,
            wt_path,
            Some("v2"),
            Some("h2"),
        )
        .await
        .unwrap();
        assert_eq!(first_id, second_id, "stable id across upserts");

        let row: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT git_branch, git_commit_hash FROM projects WHERE host_id = ? AND path = ?",
        )
        .bind(&host_id_str)
        .bind(wt_path)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some("v2"));
        assert_eq!(row.1.as_deref(), Some("h2"));
    }
}
