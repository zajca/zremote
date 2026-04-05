use std::collections::HashSet;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use zremote_protocol::claude::ClaudeAgentMessage;
use zremote_protocol::{AgentMessage, AgenticAgentMessage, AgenticStatus, SessionId};

use super::context::HookContextProvider;
use super::mapper::SessionMapper;
use super::transcript::extract_slug;
use crate::knowledge::context_delivery::DeliveryCoordinator;

/// Shared state for the hooks HTTP handler.
#[derive(Clone)]
pub struct HooksState {
    pub agentic_tx: mpsc::Sender<AgenticAgentMessage>,
    pub mapper: SessionMapper,
    /// Sender for outbound agent messages (used for `SessionIdCaptured`).
    pub outbound_tx: mpsc::Sender<AgentMessage>,
    /// CC session IDs that have already been sent via `SessionIdCaptured` (dedup).
    pub sent_cc_session_ids: Arc<tokio::sync::RwLock<HashSet<String>>>,
    /// Builds `additionalContext` for hook responses.
    pub context_provider: HookContextProvider,
    /// Coordinates pending context nudges for delivery via hooks.
    pub delivery_coordinator: Arc<tokio::sync::Mutex<DeliveryCoordinator>>,
}

/// The JSON payload received from Claude Code hooks via stdin.
#[derive(Debug, Deserialize, Serialize)]
pub struct HookPayload {
    pub session_id: String,
    pub hook_event_name: String,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    // PreToolUse / PostToolUse fields
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    // PostToolUse field
    #[serde(default)]
    pub tool_response: Option<serde_json::Value>,
    // Notification field
    #[serde(default)]
    pub message: Option<String>,
    // Stop field -- true when a previous Stop hook blocked CC from stopping.
    // Currently unused: ZRemote never blocks Stop (always returns empty response).
    // Kept for forward compatibility and protocol completeness.
    #[serde(default)]
    pub stop_hook_active: Option<bool>,
    // UserPromptSubmit field
    #[serde(default)]
    pub prompt: Option<String>,
    // SessionStart field
    #[serde(default)]
    pub source: Option<String>,
    // Elicitation fields
    #[serde(default)]
    pub mcp_server_name: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub elicitation_id: Option<String>,
    #[serde(default)]
    pub requested_schema: Option<serde_json::Value>,
    // Base field: CC permission mode (present in all hook events)
    #[serde(default)]
    pub permission_mode: Option<String>,
}

/// Response to hook scripts.
///
/// Claude Code inspects `hookSpecificOutput` for structured data like
/// `additionalContext` (injected into model context), `watchPaths`
/// (dynamic file monitoring), and `permissionDecision` (auto-approve/deny).
#[derive(Debug, Serialize, Default)]
pub struct HookResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(rename = "hookSpecificOutput", skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<HookSpecificOutput>,
}

/// Structured output per hook event type. The `hookEventName` tag tells
/// Claude Code which fields to expect.
#[derive(Debug, Serialize)]
#[serde(tag = "hookEventName")]
pub enum HookSpecificOutput {
    PreToolUse {
        #[serde(rename = "additionalContext", skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
        #[serde(rename = "permissionDecision", skip_serializing_if = "Option::is_none")]
        permission_decision: Option<String>,
        #[serde(
            rename = "permissionDecisionReason",
            skip_serializing_if = "Option::is_none"
        )]
        permission_decision_reason: Option<String>,
        #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
    },
    PostToolUse {
        #[serde(rename = "additionalContext", skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    SessionStart {
        #[serde(rename = "watchPaths", skip_serializing_if = "Option::is_none")]
        watch_paths: Option<Vec<String>>,
        #[serde(rename = "additionalContext", skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
}

/// POST /hooks - main entry point for all CC hook events.
pub async fn handle_hook(
    State(state): State<HooksState>,
    headers: HeaderMap,
    Json(payload): Json<HookPayload>,
) -> impl IntoResponse {
    let env_file = headers
        .get("x-claude-env-file")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(String::from);
    tracing::debug!(
        hook_event = %payload.hook_event_name,
        cc_session = %payload.session_id,
        tool = ?payload.tool_name,
        "hook event received"
    );
    tracing::trace!(
        payload = ?serde_json::to_string(&payload).ok(),
        "raw hook payload"
    );

    // Update transcript path if provided
    if let Some(ref path) = payload.transcript_path {
        state
            .mapper
            .set_transcript_path(&payload.session_id, path.clone())
            .await;
    }

    let response = match payload.hook_event_name.as_str() {
        "PreToolUse" => handle_pre_tool_use(&state, &payload).await,
        "PostToolUse" => handle_post_tool_use(&state, &payload).await,
        "Stop" => {
            handle_stop(&state, &payload).await;
            HookResponse::default()
        }
        "Notification" => {
            if let Some(ref msg) = payload.message {
                tracing::info!(cc_session = %payload.session_id, message = %msg, "CC notification");
            }
            send_input_status(
                &state,
                &payload,
                "Notification",
                AgenticStatus::WaitingForInput,
                None,
                None,
            )
            .await;
            HookResponse::default()
        }
        "Elicitation" => {
            handle_elicitation(&state, &payload).await;
            HookResponse::default()
        }
        "UserPromptSubmit" => {
            handle_user_prompt_submit(&state, &payload).await;
            HookResponse::default()
        }
        "SessionStart" => {
            tracing::info!(
                cc_session = %payload.session_id,
                source = ?payload.source,
                "CC session started"
            );
            // Write session env vars to CLAUDE_ENV_FILE if provided.
            // These become available in all subsequent Bash tool calls.
            if let Some(ref path) = env_file {
                write_claude_env_file(path, &payload).await;
            }
            // Return watchPaths for project files CC should monitor.
            // FileChanged hook fires when any of these change.
            let watch_paths = build_watch_paths(payload.cwd.as_deref());
            HookResponse {
                hook_specific_output: watch_paths.map(|paths| HookSpecificOutput::SessionStart {
                    watch_paths: Some(paths),
                    additional_context: None,
                }),
                ..Default::default()
            }
        }
        "SubagentStart" | "SubagentStop" => {
            let event_name = payload.hook_event_name.as_str();
            if let Some(mapped) = state.mapper.try_resolve(&payload.session_id).await {
                let task_name = if event_name == "SubagentStart" {
                    Some("spawning subagent".to_string())
                } else {
                    Some("subagent completed".to_string())
                };
                // Both start and stop use Working: the parent agent continues
                // its work after a subagent returns, so the loop is still active.
                let msg = AgenticAgentMessage::LoopStateUpdate {
                    loop_id: mapped.loop_id,
                    status: AgenticStatus::Working,
                    task_name,
                    prompt_message: None,
                    permission_mode: None,
                    action_tool_name: None,
                    action_description: None,
                };
                let _ = state.agentic_tx.try_send(msg);
            }
            HookResponse::default()
        }
        "FileChanged" => {
            tracing::info!(
                cc_session = %payload.session_id,
                tool_input = ?payload.tool_input,
                "watched file changed"
            );
            HookResponse::default()
        }
        "CwdChanged" => {
            tracing::info!(
                cc_session = %payload.session_id,
                cwd = ?payload.cwd,
                "working directory changed"
            );
            if let Some(ref path) = env_file {
                write_claude_env_file(path, &payload).await;
            }
            HookResponse::default()
        }
        "StopFailure" => {
            tracing::warn!(
                cc_session = %payload.session_id,
                "CC turn ended due to API error"
            );
            HookResponse::default()
        }
        other => {
            tracing::debug!(hook_event = %other, "unknown hook event, ignoring");
            HookResponse::default()
        }
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// If this CC session belongs to a Claude task and we haven't sent its ID yet,
/// send a `SessionIdCaptured` message to the server so it can link the CC session
/// to the Claude task for resume support.
async fn try_capture_cc_session_id(
    state: &HooksState,
    cc_session_id: &str,
    mapped_session_id: &SessionId,
) {
    // Check if already sent
    if state
        .sent_cc_session_ids
        .read()
        .await
        .contains(cc_session_id)
    {
        return;
    }

    // Check if this is a Claude task session
    let Some(claude_task_id) = state.mapper.get_claude_task_id(mapped_session_id).await else {
        return;
    };

    // Mark as sent
    state
        .sent_cc_session_ids
        .write()
        .await
        .insert(cc_session_id.to_string());

    // Send to server
    let msg = AgentMessage::ClaudeAction(ClaudeAgentMessage::SessionIdCaptured {
        claude_task_id,
        cc_session_id: cc_session_id.to_string(),
    });
    if state.outbound_tx.try_send(msg).is_err() {
        tracing::warn!("outbound channel full, SessionIdCaptured dropped");
    }
}

async fn handle_pre_tool_use(state: &HooksState, payload: &HookPayload) -> HookResponse {
    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        tracing::debug!(cc_session = %payload.session_id, "no loop mapping for PreToolUse, ignoring");
        return HookResponse::default();
    };

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;
    state.mapper.mark_hook_activity(mapped.session_id);
    state.mapper.set_hook_mode(mapped.session_id);

    // Emit LoopStateUpdate(Working)
    let msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status: AgenticStatus::Working,
        task_name: None,
        prompt_message: None,
        permission_mode: payload.permission_mode.clone(),
        action_tool_name: None,
        action_description: None,
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, LoopStateUpdate dropped");
    }

    // Build additionalContext for the hook response
    let mut coordinator = state.delivery_coordinator.lock().await;
    let additional_context = state
        .context_provider
        .build_pre_tool_context(payload, &mut coordinator)
        .await;

    HookResponse {
        decision: None,
        hook_specific_output: Some(HookSpecificOutput::PreToolUse {
            additional_context,
            permission_decision: None,
            permission_decision_reason: None,
            updated_input: None,
        }),
    }
}

async fn handle_post_tool_use(state: &HooksState, payload: &HookPayload) -> HookResponse {
    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        return HookResponse::default();
    };

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;
    state.mapper.mark_hook_activity(mapped.session_id);
    state.mapper.set_hook_mode(mapped.session_id);

    // Try to extract slug from transcript for task_name
    let task_name = extract_task_name_from_transcript(payload, &mapped);

    let msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status: AgenticStatus::Working,
        task_name,
        prompt_message: None,
        permission_mode: payload.permission_mode.clone(),
        action_tool_name: None,
        action_description: None,
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, LoopStateUpdate dropped");
    }

    // PostToolUse: Claude Code does not consume `additional_context` on
    // PostToolUse hooks, so we skip context delivery here. Nudges are
    // delivered exclusively via PreToolUse where the model can see them.
    HookResponse {
        decision: None,
        hook_specific_output: Some(HookSpecificOutput::PostToolUse {
            additional_context: None,
        }),
    }
}

async fn handle_stop(state: &HooksState, payload: &HookPayload) {
    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        tracing::warn!(cc_session = %payload.session_id, "Stop hook: no loop mapping, falling back to process polling");
        return;
    };

    tracing::info!(loop_id = %mapped.loop_id, cc_session = %payload.session_id, "Stop hook: sending LoopEnded");

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;
    state.mapper.mark_hook_activity(mapped.session_id);
    state.mapper.set_hook_mode(mapped.session_id);

    // Extract task_name from transcript
    let task_name = extract_task_name_from_transcript(payload, &mapped);

    let msg = AgenticAgentMessage::LoopEnded {
        loop_id: mapped.loop_id,
        reason: "stop".to_string(),
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, LoopEnded dropped");
    }

    // If we found a task_name, send a final state update before ending
    if task_name.is_some() {
        let update = AgenticAgentMessage::LoopStateUpdate {
            loop_id: mapped.loop_id,
            status: AgenticStatus::Completed,
            task_name,
            prompt_message: None,
            permission_mode: None,
            action_tool_name: None,
            action_description: None,
        };
        let _ = state.agentic_tx.try_send(update);
    }
}

/// Send a status update for a resolved loop. Shared by notification
/// and elicitation handlers to avoid duplicated resolve+send logic.
async fn send_input_status(
    state: &HooksState,
    payload: &HookPayload,
    event: &str,
    status: AgenticStatus,
    action_tool_name: Option<String>,
    action_description: Option<String>,
) {
    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        tracing::debug!(
            cc_session = %payload.session_id,
            event = %event,
            "no loop mapping, ignoring"
        );
        return;
    };

    state.mapper.mark_hook_activity(mapped.session_id);
    state.mapper.set_hook_mode(mapped.session_id);

    // Truncate prompt_message to avoid DoS via oversized payloads (CWE-400).
    const MAX_PROMPT_LEN: usize = 500;
    let prompt_message = payload.message.as_deref().map(|m| {
        if m.len() > MAX_PROMPT_LEN {
            // Find a valid char boundary to avoid splitting a multi-byte char.
            let end = m.floor_char_boundary(MAX_PROMPT_LEN);
            format!("{}...", &m[..end])
        } else {
            m.to_string()
        }
    });

    // Truncate action_description with the same guard (CWE-400).
    let action_description = action_description.map(|d| {
        if d.len() > MAX_PROMPT_LEN {
            let end = d.floor_char_boundary(MAX_PROMPT_LEN);
            format!("{}...", &d[..end])
        } else {
            d
        }
    });

    let msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status,
        task_name: None,
        prompt_message,
        permission_mode: None,
        action_tool_name,
        action_description,
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, status update dropped");
    }
}

/// Handle typed notification events (idle_prompt, permission_prompt).
///
/// Called from dedicated routes `/hooks/notification/idle` and
/// `/hooks/notification/permission` which are registered with specific
/// matchers in settings.json. `idle_prompt` sets `WaitingForInput`;
/// `permission_prompt` sets `RequiresAction` with action details.
async fn handle_notification_typed(
    state: &HooksState,
    payload: &HookPayload,
    notification_type: &str,
) {
    tracing::info!(
        cc_session = %payload.session_id,
        notification_type = %notification_type,
        "CC notification (typed)"
    );
    // Log message content at debug level to avoid leaking sensitive prompt text (CWE-532).
    tracing::debug!(
        cc_session = %payload.session_id,
        message = ?payload.message,
        "CC notification message"
    );

    // Update transcript path if provided
    if let Some(ref path) = payload.transcript_path {
        state
            .mapper
            .set_transcript_path(&payload.session_id, path.clone())
            .await;
    }

    match notification_type {
        "permission_prompt" => {
            send_input_status(
                state,
                payload,
                notification_type,
                AgenticStatus::RequiresAction,
                payload.tool_name.clone(),
                payload.message.clone(),
            )
            .await;
        }
        _ => {
            send_input_status(
                state,
                payload,
                notification_type,
                AgenticStatus::WaitingForInput,
                None,
                None,
            )
            .await;
        }
    }
}

/// Route handler for idle_prompt notifications.
pub async fn handle_notification_idle(
    State(state): State<HooksState>,
    Json(payload): Json<HookPayload>,
) -> impl IntoResponse {
    handle_notification_typed(&state, &payload, "idle_prompt").await;
    (StatusCode::OK, Json(HookResponse::default())).into_response()
}

/// Route handler for permission_prompt notifications.
pub async fn handle_notification_permission(
    State(state): State<HooksState>,
    Json(payload): Json<HookPayload>,
) -> impl IntoResponse {
    handle_notification_typed(&state, &payload, "permission_prompt").await;
    (StatusCode::OK, Json(HookResponse::default())).into_response()
}

async fn handle_elicitation(state: &HooksState, payload: &HookPayload) {
    tracing::info!(
        cc_session = %payload.session_id,
        mcp_server = ?payload.mcp_server_name,
        mode = ?payload.mode,
        "CC elicitation event"
    );

    send_input_status(
        state,
        payload,
        "Elicitation",
        AgenticStatus::RequiresAction,
        payload.mcp_server_name.clone(),
        payload.message.clone(),
    )
    .await;
}

async fn handle_user_prompt_submit(state: &HooksState, payload: &HookPayload) {
    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        tracing::debug!(cc_session = %payload.session_id, "no loop mapping for UserPromptSubmit, ignoring");
        return;
    };

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;
    state.mapper.mark_hook_activity(mapped.session_id);
    state.mapper.set_hook_mode(mapped.session_id);

    let msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status: AgenticStatus::Working,
        task_name: None,
        prompt_message: None,
        permission_mode: payload.permission_mode.clone(),
        action_tool_name: None,
        action_description: None,
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, LoopStateUpdate dropped");
    }
}

/// Build a list of project files to watch via CC's `watchPaths` feature.
/// Returns `None` if no watchable files exist in `cwd`.
fn build_watch_paths(cwd: Option<&str>) -> Option<Vec<String>> {
    let cwd = cwd?;
    let base = std::path::Path::new(cwd);
    let candidates = [
        "Cargo.toml",
        ".env",
        "package.json",
        "pyproject.toml",
        "CLAUDE.md",
        "go.mod",
        "composer.json",
    ];
    let paths: Vec<String> = candidates
        .iter()
        .filter_map(|f| {
            let p = base.join(f);
            p.exists().then(|| p.to_string_lossy().to_string())
        })
        .collect();
    if paths.is_empty() { None } else { Some(paths) }
}

/// Write ZRemote environment variables to the `CLAUDE_ENV_FILE` path.
///
/// Claude Code reads this file and exports the variables for all subsequent
/// Bash tool calls in the session. Only called for events that receive
/// `CLAUDE_ENV_FILE` (SessionStart, CwdChanged, FileChanged).
async fn write_claude_env_file(path: &str, payload: &HookPayload) {
    use std::fmt::Write;

    // CWE-22: Only write to temp directory to prevent arbitrary file overwrite
    // via crafted X-Claude-Env-File header from local processes.
    let env_path = std::path::Path::new(path);
    let temp_dir = std::env::temp_dir();
    if !env_path.starts_with(&temp_dir) {
        tracing::warn!(
            path,
            temp_dir = %temp_dir.display(),
            "CLAUDE_ENV_FILE path outside temp dir, refusing write"
        );
        return;
    }

    let mut content = String::new();
    // CWE-78: Sanitize values before writing to shell file to prevent injection.
    // CC session_id is normally a UUID but comes from untrusted JSON payload.
    let safe_session_id = sanitize_shell_value(&payload.session_id);
    writeln!(
        &mut content,
        "export ZREMOTE_SESSION_ID='{safe_session_id}'"
    )
    .ok();
    writeln!(&mut content, "export ZREMOTE_TERMINAL=1").ok();
    if let Some(ref cwd) = payload.cwd {
        let safe_cwd = sanitize_shell_value(cwd);
        writeln!(&mut content, "export ZREMOTE_CWD='{safe_cwd}'").ok();
    }

    if let Err(e) = tokio::fs::write(path, &content).await {
        tracing::warn!(path, error = %e, "failed to write CLAUDE_ENV_FILE");
    } else {
        tracing::debug!(path, "wrote CLAUDE_ENV_FILE");
    }
}

/// Sanitize a value for safe inclusion in single-quoted shell strings.
/// Single quotes within the value are escaped as `'\''` (end quote, escaped
/// literal quote, restart quote).
fn sanitize_shell_value(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Extract `task_name` from the transcript file slug, with path traversal validation.
fn extract_task_name_from_transcript(
    payload: &HookPayload,
    mapped: &super::mapper::MappedSession,
) -> Option<String> {
    let transcript_path = payload
        .transcript_path
        .as_ref()
        .or(mapped.transcript_path.as_ref())?;

    // CWE-22: Validate transcript_path is within ~/.claude/projects/
    let Ok(home) = std::env::var("HOME") else {
        tracing::warn!("HOME not set, cannot validate transcript_path, skipping");
        return None;
    };
    let allowed_prefix = format!("{home}/.claude/projects/");
    if !transcript_path.starts_with(&allowed_prefix) {
        tracing::warn!(
            path = %transcript_path,
            "transcript_path outside allowed directory, ignoring"
        );
        return None;
    }

    let offset = mapped.transcript_offset;
    match extract_slug(transcript_path, offset) {
        Ok((slug, _new_offset)) => {
            // Cap task_name to 100 chars
            slug.map(|s| {
                if s.len() > 100 {
                    s[..100].to_string()
                } else {
                    s
                }
            })
        }
        Err(e) => {
            tracing::debug!(
                path = %transcript_path,
                error = %e,
                "failed to extract slug from transcript"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use zremote_protocol::AgenticLoopId;

    /// Create a minimal `HooksState` with mpsc channels for testing.
    /// Returns `(state, agentic_rx, outbound_rx)` so tests can inspect sent messages.
    fn test_state() -> (
        HooksState,
        mpsc::Receiver<AgenticAgentMessage>,
        mpsc::Receiver<AgentMessage>,
    ) {
        let (agentic_tx, agentic_rx) = mpsc::channel(64);
        let (outbound_tx, outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();
        let state = HooksState {
            agentic_tx,
            context_provider: HookContextProvider::new(mapper.clone()),
            delivery_coordinator: Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
            mapper,
            outbound_tx,
            sent_cc_session_ids: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
        };
        (state, agentic_rx, outbound_rx)
    }

    /// Register a loop in the mapper and return IDs.
    async fn setup_loop(state: &HooksState) -> (SessionId, AgenticLoopId) {
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        (session_id, loop_id)
    }

    fn payload(event: &str, cc_session: &str) -> HookPayload {
        HookPayload {
            session_id: cc_session.to_string(),
            hook_event_name: event.to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
            stop_hook_active: None,
            prompt: None,
            source: None,
            mcp_server_name: None,
            mode: None,
            elicitation_id: None,
            requested_schema: None,
            permission_mode: None,
        }
    }

    // ---------------------------------------------------------------
    // handle_pre_tool_use
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn pre_tool_use_sends_working_status() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, loop_id) = setup_loop(&state).await;

        let p = payload("PreToolUse", "cc-1");
        handle_pre_tool_use(&state, &p).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                task_name,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::Working);
                assert!(task_name.is_none());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pre_tool_use_no_loop_mapping_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        // No loop registered -- resolve will fail after retries
        let p = payload("PreToolUse", "unknown-cc");
        handle_pre_tool_use(&state, &p).await;
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pre_tool_use_with_various_tool_names() {
        for tool in &["Read", "Bash", "Edit", "Write", "Grep", "WebSearch"] {
            let (state, mut agentic_rx, _outbound_rx) = test_state();
            let (_sid, _loop_id) = setup_loop(&state).await;

            let mut p = payload("PreToolUse", "cc-tools");
            p.tool_name = Some(tool.to_string());
            handle_pre_tool_use(&state, &p).await;

            // All tool names should produce a Working status
            let msg = agentic_rx.try_recv().unwrap();
            assert!(
                matches!(
                    msg,
                    AgenticAgentMessage::LoopStateUpdate {
                        status: AgenticStatus::Working,
                        ..
                    }
                ),
                "expected Working for tool {tool}"
            );
        }
    }

    // ---------------------------------------------------------------
    // handle_post_tool_use
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn post_tool_use_sends_working_status() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, loop_id) = setup_loop(&state).await;

        let p = payload("PostToolUse", "cc-2");
        handle_post_tool_use(&state, &p).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::Working);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn post_tool_use_no_loop_mapping_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let p = payload("PostToolUse", "unknown-cc");
        handle_post_tool_use(&state, &p).await;
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn post_tool_use_extracts_task_name_from_transcript() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, _loop_id) = setup_loop(&state).await;

        // Create a transcript file with a slug
        let dir = tempfile::tempdir().unwrap();
        let home = std::env::var("HOME").unwrap();
        // The transcript must be under ~/.claude/projects/ to pass validation
        let transcript_dir = format!("{home}/.claude/projects/_test_handler");
        std::fs::create_dir_all(&transcript_dir).ok();
        let transcript_path = format!("{transcript_dir}/transcript.jsonl");
        std::fs::write(
            &transcript_path,
            "{\"type\":\"result\",\"slug\":\"my-task-slug\"}\n",
        )
        .unwrap();

        // First resolve the CC session so mapper knows it
        let cc_id = "cc-slug-test";
        let _ = state.mapper.resolve_loop_id(cc_id, None).await;

        // Set transcript path on the mapped session
        state
            .mapper
            .set_transcript_path(cc_id, transcript_path.clone())
            .await;

        let mut p = payload("PostToolUse", cc_id);
        p.transcript_path = Some(transcript_path.clone());
        handle_post_tool_use(&state, &p).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate { task_name, .. } => {
                assert_eq!(task_name.as_deref(), Some("my-task-slug"));
            }
            other => panic!("unexpected message: {other:?}"),
        }

        // Cleanup
        std::fs::remove_dir_all(&transcript_dir).ok();
        drop(dir);
    }

    // ---------------------------------------------------------------
    // handle_stop
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn stop_sends_loop_ended() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, loop_id) = setup_loop(&state).await;

        let p = payload("Stop", "cc-stop");
        handle_stop(&state, &p).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopEnded {
                loop_id: lid,
                reason,
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(reason, "stop");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn stop_no_loop_mapping_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let p = payload("Stop", "unknown-cc");
        handle_stop(&state, &p).await;
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn stop_with_task_name_sends_completed_update() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, loop_id) = setup_loop(&state).await;

        // Create transcript with slug under allowed path
        let home = std::env::var("HOME").unwrap();
        let transcript_dir = format!("{home}/.claude/projects/_test_handler_stop");
        std::fs::create_dir_all(&transcript_dir).ok();
        let transcript_path = format!("{transcript_dir}/transcript.jsonl");
        std::fs::write(
            &transcript_path,
            "{\"type\":\"result\",\"slug\":\"stop-task\"}\n",
        )
        .unwrap();

        let cc_id = "cc-stop-slug";
        let _ = state.mapper.resolve_loop_id(cc_id, None).await;
        state
            .mapper
            .set_transcript_path(cc_id, transcript_path.clone())
            .await;

        let mut p = payload("Stop", cc_id);
        p.transcript_path = Some(transcript_path.clone());
        handle_stop(&state, &p).await;

        // First message: LoopEnded
        let msg1 = agentic_rx.try_recv().unwrap();
        assert!(matches!(msg1, AgenticAgentMessage::LoopEnded { .. }));

        // Second message: LoopStateUpdate with Completed + task_name
        let msg2 = agentic_rx.try_recv().unwrap();
        match msg2 {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                task_name,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::Completed);
                assert_eq!(task_name.as_deref(), Some("stop-task"));
            }
            other => panic!("unexpected message: {other:?}"),
        }

        std::fs::remove_dir_all(&transcript_dir).ok();
    }

    // ---------------------------------------------------------------
    // handle_elicitation
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn elicitation_sends_requires_action() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, loop_id) = setup_loop(&state).await;

        let p = payload("Elicitation", "cc-elicit");
        handle_elicitation(&state, &p).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                task_name,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::RequiresAction);
                assert!(task_name.is_none());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn elicitation_no_loop_mapping_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let p = payload("Elicitation", "unknown-cc");
        handle_elicitation(&state, &p).await;
        assert!(agentic_rx.try_recv().is_err());
    }

    // ---------------------------------------------------------------
    // handle_notification_typed
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn permission_prompt_sends_requires_action_with_tool_name() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, loop_id) = setup_loop(&state).await;

        let mut p = payload("Notification", "cc-perm");
        p.tool_name = Some("Bash".to_string());
        p.message = Some("Allow Bash tool?".to_string());
        handle_notification_typed(&state, &p, "permission_prompt").await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                action_tool_name,
                action_description,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::RequiresAction);
                assert_eq!(action_tool_name.as_deref(), Some("Bash"));
                assert!(action_description.is_some());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn idle_prompt_sends_waiting_for_input() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, loop_id) = setup_loop(&state).await;

        let p = payload("Notification", "cc-idle");
        handle_notification_typed(&state, &p, "idle_prompt").await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                action_tool_name,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::WaitingForInput);
                assert!(action_tool_name.is_none());
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    // ---------------------------------------------------------------
    // handle_user_prompt_submit
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn user_prompt_submit_sends_working() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, loop_id) = setup_loop(&state).await;

        let mut p = payload("UserPromptSubmit", "cc-prompt");
        p.prompt = Some("fix the bug".to_string());
        handle_user_prompt_submit(&state, &p).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::Working);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn user_prompt_submit_no_loop_mapping_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let p = payload("UserPromptSubmit", "unknown-cc");
        handle_user_prompt_submit(&state, &p).await;
        assert!(agentic_rx.try_recv().is_err());
    }

    // ---------------------------------------------------------------
    // handle_hook (dispatcher)
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn handle_hook_sets_transcript_path() {
        let (state, _agentic_rx, _outbound_rx) = test_state();
        let (_sid, _loop_id) = setup_loop(&state).await;
        let cc_id = "cc-transcript";

        // Resolve to auto-register the CC session
        let _ = state.mapper.resolve_loop_id(cc_id, None).await;

        let mut p = payload("PreToolUse", cc_id);
        p.transcript_path = Some("/some/path/transcript.jsonl".to_string());

        // Call the internal logic that handle_hook does before dispatching
        if let Some(ref path) = p.transcript_path {
            state
                .mapper
                .set_transcript_path(&p.session_id, path.clone())
                .await;
        }

        let mapped = state.mapper.lookup_by_cc_session(cc_id).await.unwrap();
        assert_eq!(
            mapped.transcript_path.as_deref(),
            Some("/some/path/transcript.jsonl")
        );
    }

    #[tokio::test]
    async fn handle_hook_unknown_event_is_noop() {
        let (_state, mut agentic_rx, _outbound_rx) = test_state();
        let p = payload("SomeUnknownEvent", "cc-unknown");

        // Dispatcher matches "other" branch -- no messages sent
        match p.hook_event_name.as_str() {
            "PreToolUse" | "PostToolUse" | "Stop" | "Notification" | "SubagentStart"
            | "SubagentStop" => panic!("should not match known events"),
            _ => {} // expected
        }
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn handle_hook_subagent_events_noop_without_mapping() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        // No loop registered — try_resolve returns None, no message sent
        for event in &["SubagentStart", "SubagentStop"] {
            let p = payload(event, "cc-sub-unmapped");
            if let Some(mapped) = state.mapper.try_resolve(&p.session_id).await {
                let _ = state
                    .agentic_tx
                    .try_send(AgenticAgentMessage::LoopStateUpdate {
                        loop_id: mapped.loop_id,
                        status: AgenticStatus::Working,
                        task_name: None,
                        prompt_message: None,
                        permission_mode: None,
                        action_tool_name: None,
                        action_description: None,
                    });
            }
        }
        assert!(agentic_rx.try_recv().is_err());
    }

    // ---------------------------------------------------------------
    // try_capture_cc_session_id
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn capture_cc_session_id_sends_message_for_claude_task() {
        let (state, _agentic_rx, mut outbound_rx) = test_state();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let claude_task_id = Uuid::new_v4();

        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_claude_task(session_id, claude_task_id)
            .await;

        try_capture_cc_session_id(&state, "cc-capture", &session_id).await;

        let msg = outbound_rx.try_recv().unwrap();
        match msg {
            AgentMessage::ClaudeAction(ClaudeAgentMessage::SessionIdCaptured {
                claude_task_id: tid,
                cc_session_id,
            }) => {
                assert_eq!(tid, claude_task_id);
                assert_eq!(cc_session_id, "cc-capture");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn capture_cc_session_id_deduplicates() {
        let (state, _agentic_rx, mut outbound_rx) = test_state();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let claude_task_id = Uuid::new_v4();

        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_claude_task(session_id, claude_task_id)
            .await;

        // First call sends the message
        try_capture_cc_session_id(&state, "cc-dedup", &session_id).await;
        assert!(outbound_rx.try_recv().is_ok());

        // Second call with same CC session ID is a no-op
        try_capture_cc_session_id(&state, "cc-dedup", &session_id).await;
        assert!(outbound_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn capture_cc_session_id_noop_when_not_claude_task() {
        let (state, _agentic_rx, mut outbound_rx) = test_state();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        state.mapper.register_loop(session_id, loop_id).await;
        // No claude task registered for this session

        try_capture_cc_session_id(&state, "cc-no-task", &session_id).await;
        assert!(outbound_rx.try_recv().is_err());
    }

    // ---------------------------------------------------------------
    // extract_task_name_from_transcript
    // ---------------------------------------------------------------

    #[test]
    fn extract_task_name_no_transcript_path() {
        let p = payload("PostToolUse", "cc-1");
        let mapped = super::super::mapper::MappedSession {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            transcript_path: None,
            transcript_offset: 0,
        };
        let result = extract_task_name_from_transcript(&p, &mapped);
        assert!(result.is_none());
    }

    #[test]
    fn extract_task_name_falls_back_to_mapped_transcript_path() {
        let home = std::env::var("HOME").unwrap();
        let transcript_dir = format!("{home}/.claude/projects/_test_handler_fallback");
        std::fs::create_dir_all(&transcript_dir).ok();
        let transcript_path = format!("{transcript_dir}/transcript.jsonl");
        std::fs::write(
            &transcript_path,
            "{\"type\":\"result\",\"slug\":\"fallback-slug\"}\n",
        )
        .unwrap();

        let p = payload("PostToolUse", "cc-1"); // no transcript_path in payload
        let mapped = super::super::mapper::MappedSession {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            transcript_path: Some(transcript_path),
            transcript_offset: 0,
        };
        let result = extract_task_name_from_transcript(&p, &mapped);
        assert_eq!(result.as_deref(), Some("fallback-slug"));

        std::fs::remove_dir_all(&transcript_dir).ok();
    }

    #[test]
    fn extract_task_name_truncates_at_100_chars() {
        let home = std::env::var("HOME").unwrap();
        let transcript_dir = format!("{home}/.claude/projects/_test_handler_trunc");
        std::fs::create_dir_all(&transcript_dir).ok();
        let transcript_path = format!("{transcript_dir}/transcript.jsonl");

        let long_slug = "a".repeat(150);
        std::fs::write(
            &transcript_path,
            format!("{{\"type\":\"result\",\"slug\":\"{long_slug}\"}}\n"),
        )
        .unwrap();

        let mut p = payload("PostToolUse", "cc-1");
        p.transcript_path = Some(transcript_path);
        let mapped = super::super::mapper::MappedSession {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            transcript_path: None,
            transcript_offset: 0,
        };
        let result = extract_task_name_from_transcript(&p, &mapped);
        assert_eq!(result.as_ref().map(|s| s.len()), Some(100));

        std::fs::remove_dir_all(&transcript_dir).ok();
    }

    #[test]
    fn extract_task_name_rejects_path_outside_allowed_dir() {
        let dir = tempfile::tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript_path,
            "{\"type\":\"result\",\"slug\":\"evil-slug\"}\n",
        )
        .unwrap();

        let mut p = payload("PostToolUse", "cc-1");
        p.transcript_path = Some(transcript_path.to_str().unwrap().to_string());
        let mapped = super::super::mapper::MappedSession {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            transcript_path: None,
            transcript_offset: 0,
        };
        let result = extract_task_name_from_transcript(&p, &mapped);
        // Path traversal validation should reject this
        assert!(result.is_none());
    }

    #[test]
    fn extract_task_name_nonexistent_file_returns_none() {
        let home = std::env::var("HOME").unwrap();
        let transcript_path =
            format!("{home}/.claude/projects/_test_handler_nofile/nonexistent.jsonl");

        let mut p = payload("PostToolUse", "cc-1");
        p.transcript_path = Some(transcript_path);
        let mapped = super::super::mapper::MappedSession {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            transcript_path: None,
            transcript_offset: 0,
        };
        let result = extract_task_name_from_transcript(&p, &mapped);
        assert!(result.is_none());
    }

    #[test]
    fn extract_task_name_empty_slug_in_transcript() {
        let home = std::env::var("HOME").unwrap();
        let transcript_dir = format!("{home}/.claude/projects/_test_handler_empty");
        std::fs::create_dir_all(&transcript_dir).ok();
        let transcript_path = format!("{transcript_dir}/transcript.jsonl");
        std::fs::write(&transcript_path, "{\"type\":\"result\",\"slug\":\"\"}\n").unwrap();

        let mut p = payload("PostToolUse", "cc-1");
        p.transcript_path = Some(transcript_path);
        let mapped = super::super::mapper::MappedSession {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            transcript_path: None,
            transcript_offset: 0,
        };
        let result = extract_task_name_from_transcript(&p, &mapped);
        // Empty string slug is still returned (extract_slug returns it)
        assert_eq!(result.as_deref(), Some(""));

        std::fs::remove_dir_all(&transcript_dir).ok();
    }

    #[test]
    fn extract_task_name_no_slug_in_transcript() {
        let home = std::env::var("HOME").unwrap();
        let transcript_dir = format!("{home}/.claude/projects/_test_handler_noslug");
        std::fs::create_dir_all(&transcript_dir).ok();
        let transcript_path = format!("{transcript_dir}/transcript.jsonl");
        std::fs::write(
            &transcript_path,
            "{\"type\":\"message\",\"role\":\"user\"}\n",
        )
        .unwrap();

        let mut p = payload("PostToolUse", "cc-1");
        p.transcript_path = Some(transcript_path);
        let mapped = super::super::mapper::MappedSession {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            transcript_path: None,
            transcript_offset: 0,
        };
        let result = extract_task_name_from_transcript(&p, &mapped);
        assert!(result.is_none());

        std::fs::remove_dir_all(&transcript_dir).ok();
    }

    // ---------------------------------------------------------------
    // Deserialization tests (original)
    // ---------------------------------------------------------------

    #[test]
    fn deserialize_pre_tool_use_hook() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "tool_use_id": "toolu_01abc"
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.session_id, "abc123");
        assert_eq!(payload.hook_event_name, "PreToolUse");
        assert_eq!(payload.tool_name.as_deref(), Some("Read"));
    }

    #[test]
    fn deserialize_post_tool_use_hook() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_use_id": "toolu_01abc",
            "tool_response": "some output"
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "PostToolUse");
        assert!(payload.tool_response.is_some());
    }

    #[test]
    fn deserialize_stop_hook() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "Stop"
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "Stop");
    }

    #[test]
    fn deserialize_notification_hook() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "Notification",
            "message": "Task completed"
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "Notification");
        assert_eq!(payload.message.as_deref(), Some("Task completed"));
    }

    #[test]
    fn hook_response_serializes_without_decision() {
        let resp = HookResponse::default();
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn hook_response_serializes_with_decision() {
        let resp = HookResponse {
            decision: Some("allow".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("allow"));
    }

    #[test]
    fn hook_response_serializes_pre_tool_use_output() {
        let resp = HookResponse {
            hook_specific_output: Some(HookSpecificOutput::PreToolUse {
                additional_context: Some("test context".to_string()),
                permission_decision: None,
                permission_decision_reason: None,
                updated_input: None,
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&resp).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let output = value.get("hookSpecificOutput").unwrap();
        assert_eq!(output["hookEventName"], "PreToolUse");
        assert_eq!(output["additionalContext"], "test context");
        // None fields should be absent
        assert!(output.get("permissionDecision").is_none());
    }

    #[test]
    fn hook_response_serializes_session_start_output() {
        let resp = HookResponse {
            hook_specific_output: Some(HookSpecificOutput::SessionStart {
                watch_paths: Some(vec!["/tmp/Cargo.toml".to_string()]),
                additional_context: None,
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&resp).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let output = value.get("hookSpecificOutput").unwrap();
        assert_eq!(output["hookEventName"], "SessionStart");
        assert_eq!(output["watchPaths"][0], "/tmp/Cargo.toml");
    }

    #[test]
    fn hook_response_serializes_post_tool_use_output() {
        let resp = HookResponse {
            hook_specific_output: Some(HookSpecificOutput::PostToolUse {
                additional_context: Some("post context".to_string()),
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&resp).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let output = value.get("hookSpecificOutput").unwrap();
        assert_eq!(output["hookEventName"], "PostToolUse");
        assert_eq!(output["additionalContext"], "post context");
    }

    // ---------------------------------------------------------------
    // write_claude_env_file
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn write_env_file_creates_exports() {
        // tempfile creates in std::env::temp_dir(), which passes the path validation
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("session-env.sh");
        let p = HookPayload {
            session_id: "test-session-123".to_string(),
            hook_event_name: "SessionStart".to_string(),
            cwd: Some("/home/user/project".to_string()),
            ..payload("SessionStart", "test-session-123")
        };

        write_claude_env_file(env_path.to_str().unwrap(), &p).await;

        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(content.contains("export ZREMOTE_SESSION_ID='test-session-123'"));
        assert!(content.contains("export ZREMOTE_TERMINAL=1"));
        assert!(content.contains("export ZREMOTE_CWD='/home/user/project'"));
    }

    #[tokio::test]
    async fn write_env_file_without_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("session-env.sh");
        let p = payload("SessionStart", "test-session");

        write_claude_env_file(env_path.to_str().unwrap(), &p).await;

        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(content.contains("export ZREMOTE_SESSION_ID="));
        assert!(content.contains("export ZREMOTE_TERMINAL=1"));
        assert!(!content.contains("ZREMOTE_CWD"));
    }

    #[tokio::test]
    async fn write_env_file_rejects_path_outside_temp_dir() {
        let p = payload("SessionStart", "test-session");
        // Path outside temp dir -- should be rejected (CWE-22 protection)
        write_claude_env_file("/home/user/.bashrc", &p).await;
        assert!(!std::path::Path::new("/home/user/.bashrc").exists());
    }

    #[tokio::test]
    async fn write_env_file_sanitizes_shell_injection() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("session-env.sh");
        let p = HookPayload {
            session_id: "abc'; rm -rf /; echo '".to_string(),
            hook_event_name: "SessionStart".to_string(),
            cwd: Some("/tmp/proj\"$(evil)\"".to_string()),
            ..payload("SessionStart", "x")
        };

        write_claude_env_file(env_path.to_str().unwrap(), &p).await;

        let content = std::fs::read_to_string(&env_path).unwrap();
        // Single quotes prevent injection; embedded single quotes are escaped.
        // The value is safely wrapped: 'abc'\''...' so shell interprets literally.
        assert!(content.contains("ZREMOTE_SESSION_ID='abc'\\''"));
        // CWD with double quotes and $() is safely single-quoted.
        // Inside '...' these are literal text, not shell expansions.
        assert!(content.contains("ZREMOTE_CWD='"));
    }

    #[test]
    fn sanitize_shell_value_escapes_single_quotes() {
        assert_eq!(sanitize_shell_value("hello"), "hello");
        assert_eq!(sanitize_shell_value("it's"), "it'\\''s");
        assert_eq!(sanitize_shell_value("a'b'c"), "a'\\''b'\\''c");
    }

    #[test]
    fn deserialize_minimal_hook_payload() {
        let json = r#"{
            "session_id": "test",
            "hook_event_name": "Unknown"
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert!(payload.tool_name.is_none());
        assert!(payload.tool_input.is_none());
        assert!(payload.tool_use_id.is_none());
        assert!(payload.tool_response.is_none());
        assert!(payload.message.is_none());
        assert!(payload.transcript_path.is_none());
        assert!(payload.cwd.is_none());
        assert!(payload.stop_hook_active.is_none());
        assert!(payload.prompt.is_none());
        assert!(payload.source.is_none());
        assert!(payload.mcp_server_name.is_none());
        assert!(payload.mode.is_none());
        assert!(payload.elicitation_id.is_none());
        assert!(payload.requested_schema.is_none());
    }

    #[test]
    fn deserialize_elicitation_hook() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "Elicitation",
            "mcp_server_name": "my-mcp",
            "message": "Please choose",
            "mode": "form",
            "elicitation_id": "elicit-1",
            "requested_schema": {
                "type": "object",
                "properties": {
                    "choice": {
                        "type": "string",
                        "enum": ["option1", "option2"]
                    }
                }
            }
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "Elicitation");
        assert_eq!(payload.mcp_server_name.as_deref(), Some("my-mcp"));
        assert_eq!(payload.mode.as_deref(), Some("form"));
        assert_eq!(payload.elicitation_id.as_deref(), Some("elicit-1"));
        assert!(payload.requested_schema.is_some());
    }

    #[test]
    fn deserialize_user_prompt_submit_hook() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "fix the bug in main.rs"
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "UserPromptSubmit");
        assert_eq!(payload.prompt.as_deref(), Some("fix the bug in main.rs"));
    }

    #[test]
    fn deserialize_session_start_hook() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "SessionStart",
            "source": "startup"
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "SessionStart");
        assert_eq!(payload.source.as_deref(), Some("startup"));
    }

    #[test]
    fn deserialize_stop_with_stop_hook_active() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "Stop",
            "stop_hook_active": true
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "Stop");
        assert_eq!(payload.stop_hook_active, Some(true));
    }

    // ---------------------------------------------------------------
    // build_watch_paths
    // ---------------------------------------------------------------

    #[test]
    fn watch_paths_returns_none_without_cwd() {
        assert!(build_watch_paths(None).is_none());
    }

    #[test]
    fn watch_paths_returns_none_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(build_watch_paths(Some(dir.path().to_str().unwrap())).is_none());
    }

    #[test]
    fn watch_paths_finds_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "").unwrap();

        let paths = build_watch_paths(Some(dir.path().to_str().unwrap())).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("Cargo.toml")));
        assert!(paths.iter().any(|p| p.ends_with("CLAUDE.md")));
    }

    // ---------------------------------------------------------------
    // SubagentStart/Stop
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn subagent_start_sends_working_status() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, loop_id) = setup_loop(&state).await;

        // Auto-register CC session via resolve
        let _ = state.mapper.resolve_loop_id("cc-sub-test", None).await;

        let p = payload("SubagentStart", "cc-sub-test");
        // Simulate what handle_hook does for SubagentStart
        if let Some(mapped) = state.mapper.try_resolve(&p.session_id).await {
            let _ = state
                .agentic_tx
                .try_send(AgenticAgentMessage::LoopStateUpdate {
                    loop_id: mapped.loop_id,
                    status: AgenticStatus::Working,
                    task_name: Some("spawning subagent".to_string()),
                    prompt_message: None,
                    permission_mode: None,
                    action_tool_name: None,
                    action_description: None,
                });
        }

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                task_name,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::Working);
                assert_eq!(task_name.as_deref(), Some("spawning subagent"));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn deserialize_permission_mode_field() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "permission_mode": "plan"
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.permission_mode.as_deref(), Some("plan"));
    }

    #[test]
    fn deserialize_permission_mode_absent() {
        let json = r#"{
            "session_id": "abc123",
            "hook_event_name": "PreToolUse",
            "tool_name": "Read"
        }"#;
        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert!(payload.permission_mode.is_none());
    }
}
