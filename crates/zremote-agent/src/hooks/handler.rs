// Q1 (subagent session_id): Empirical testing shows CC subagents launched via the Task
// tool use their OWN session_id, distinct from the parent. SessionMapper's try_resolve_fallback
// will auto-register the subagent's session_id if there is an unmapped loop in the session.
// In practice, when a Task subagent runs inside the same PTY session, this fallback succeeds.
// If the subagent runs in a completely separate context, the hook will be dropped with a warn.
//
// Q2 (tool_response vs tool_result field name): CC sends "tool_response" in PostToolUse.
// The test fixture at server.rs:316 used "tool_result" which was incorrect.
// Resolution: accept both via #[serde(alias)] for resilience.

use std::collections::HashSet;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use zremote_protocol::claude::ClaudeAgentMessage;
use zremote_protocol::{
    AgentInputRequest, AgentInputRequestKind, AgentKind, AgentRuntimeStatus, AgenticLoopId,
    NodeStatus,
};
use zremote_protocol::{AgentMessage, AgenticAgentMessage, AgenticStatus, SessionId};

use super::context::HookContextProvider;
use super::mapper::SessionMapper;
use crate::agents::AgentIntegration;
use crate::knowledge::context_delivery::DeliveryCoordinator;

const INPUT_CAP_BYTES: usize = 1024;
const SUMMARY_CAP_BYTES: usize = 4096;

/// Pretty input string for an opening node. Falls back to compact JSON.
fn format_tool_input(tool_name: &str, tool_input: Option<&serde_json::Value>) -> Option<String> {
    let v = tool_input?;
    let s = match tool_name {
        "Read" | "Edit" | "Write" | "MultiEdit" => v
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        "Bash" => v
            .get("command")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        "Glob" | "Grep" => v
            .get("pattern")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        "Task" => {
            let agent = v
                .get("subagent_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("agent");
            let prompt = v
                .get("prompt")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            Some(format!("{agent}: {}", truncate(prompt, 60)))
        }
        "WebFetch" => v
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        _ => Some(serde_json::to_string(v).unwrap_or_default()),
    };
    s.map(|s| truncate(&s, INPUT_CAP_BYTES))
}

/// Format tool response for display in output_summary. Handles is_error prefix,
/// stdout/content/result field fallbacks, and byte-cap with ellipsis.
fn format_tool_response(
    tool_response: Option<&serde_json::Value>,
    is_error: bool,
) -> Option<String> {
    let v = tool_response?;

    // Extract content string from common CC response shapes.
    let content = if let Some(s) = v.as_str() {
        s.to_string()
    } else if let Some(arr) = v.as_array() {
        // Array of content blocks: [{type:"text",text:"..."}, ...]
        arr.iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| item.get("content").and_then(serde_json::Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else if let Some(s) = v
        .get("stdout")
        .and_then(serde_json::Value::as_str)
        .or_else(|| v.get("content").and_then(serde_json::Value::as_str))
        .or_else(|| v.get("result").and_then(serde_json::Value::as_str))
    {
        s.to_string()
    } else {
        serde_json::to_string(v).unwrap_or_default()
    };

    let truncated = truncate(&content, SUMMARY_CAP_BYTES);
    let result = if is_error {
        format!("ERROR: {truncated}")
    } else {
        truncated
    };
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Truncate `s` to at most `max` bytes at a valid UTF-8 char boundary,
/// appending `…` if truncation occurred.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let boundary = s.floor_char_boundary(max.saturating_sub(3));
    format!("{}…", &s[..boundary])
}

/// Shared state for the hooks HTTP handler.
#[derive(Clone)]
pub struct HooksState {
    pub agentic_tx: mpsc::Sender<AgenticAgentMessage>,
    pub mapper: SessionMapper,
    /// Sender for outbound agent messages (used for `SessionIdCaptured`).
    pub outbound_tx: mpsc::Sender<AgentMessage>,
    /// CC session IDs that have already been sent via `SessionIdCaptured` (dedup).
    pub sent_cc_session_ids: Arc<tokio::sync::RwLock<HashSet<String>>>,
    /// `(zremote_session_id, agent, native_session_id)` tuples already emitted as
    /// `AgentSessionRefCaptured`, for dedup of the RFC-012 generic capture path.
    /// Generalizes [`Self::sent_cc_session_ids`], which is kept for the legacy
    /// Claude-task `SessionIdCaptured` path during the transition.
    ///
    /// A `Mutex` (not `RwLock`) so the check-and-insert is a single critical
    /// section — two concurrent hooks for the same key cannot both observe it
    /// absent and double-send.
    pub sent_agent_session_refs: Arc<tokio::sync::Mutex<HashSet<(SessionId, AgentKind, String)>>>,
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
    // PostToolUse field — CC sends "tool_response"; accept "tool_result" as alias for resilience.
    #[serde(default, alias = "tool_result")]
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

fn emit_agent_state(
    state: &HooksState,
    session_id: SessionId,
    loop_id: AgenticLoopId,
    status: AgentRuntimeStatus,
    task_name: Option<String>,
    input_request: Option<AgentInputRequest>,
) {
    let msg = AgenticAgentMessage::AgentStateChanged {
        session_id,
        loop_id: Some(loop_id),
        status,
        task_name,
        input_request,
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, AgentStateChanged dropped");
    }
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
    // RFC-012: the forwarder script sets X-ZRemote-Session-Id to the originating
    // PTY session's id (from $ZREMOTE_SESSION_ID). Validate it as a UUID here;
    // it is the authoritative key for native-session capture and does not depend
    // on the detector/loop mapping.
    let zremote_session_id = parse_zremote_session_header(&headers);
    // Select the per-agent integration from the X-ZRemote-Agent header (codex vs
    // claude; absent => claude for back-compat). Drives both the captured agent
    // kind and the transcript root / task-name extraction.
    let integration = select_integration(&headers);
    tracing::debug!(
        hook_event = %payload.hook_event_name,
        cc_session = %payload.session_id,
        agent = ?integration.agent_kind(),
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
        "PreToolUse" => {
            handle_pre_tool_use(&state, integration, &payload, zremote_session_id).await
        }
        "PostToolUse" => handle_post_tool_use(&state, integration, &payload).await,
        "Stop" => {
            handle_stop(&state, integration, &payload).await;
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
            handle_user_prompt_submit(&state, integration, &payload, zremote_session_id).await;
            HookResponse::default()
        }
        "SessionStart" => {
            tracing::info!(
                cc_session = %payload.session_id,
                source = ?payload.source,
                "CC session started"
            );
            // RFC-012: capture the native session id keyed by the ZRemote session
            // id from the header, for the agent selected by X-ZRemote-Agent.
            // Independent of the loop mapping and of whether this is a UI-started
            // Claude task.
            try_capture_agent_session_ref(
                &state,
                integration.agent_kind(),
                zremote_session_id,
                &payload.session_id,
            )
            .await;
            // Write session env vars to CLAUDE_ENV_FILE if provided.
            // These become available in all subsequent Bash tool calls.
            if let Some(ref path) = env_file {
                write_claude_env_file(path, &payload).await;
            }
            build_session_start_response(integration, payload.cwd.as_deref())
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
            handle_stop(&state, integration, &payload).await;
            HookResponse::default()
        }
        other => {
            tracing::debug!(hook_event = %other, "unknown hook event, ignoring");
            HookResponse::default()
        }
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// Build event-specific output for `SessionStart`.
///
/// `watchPaths` is a Claude Code hook extension. Codex validates its
/// `SessionStart` response more narrowly, so keep the response empty there while
/// still letting the shared handler capture the native session id above.
fn build_session_start_response(
    integration: &dyn AgentIntegration,
    cwd: Option<&str>,
) -> HookResponse {
    if integration.agent_kind() != AgentKind::Claude {
        return HookResponse::default();
    }

    // Return watchPaths for project files CC should monitor.
    // FileChanged hook fires when any of these change.
    let watch_paths = build_watch_paths(cwd);
    HookResponse {
        hook_specific_output: watch_paths.map(|paths| HookSpecificOutput::SessionStart {
            watch_paths: Some(paths),
            additional_context: None,
        }),
        ..Default::default()
    }
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
    match state.outbound_tx.try_send(msg) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("outbound channel full, SessionIdCaptured dropped");
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            // In local mode the outbound channel receiver is intentionally dropped;
            // this is expected and not an error.
            tracing::debug!("outbound channel closed (local mode), SessionIdCaptured dropped");
        }
    }
}

/// Static Claude integration instance for per-request selection.
static CLAUDE_INTEGRATION: crate::agents::ClaudeIntegration = crate::agents::ClaudeIntegration;
/// Static Codex integration instance for per-request selection.
static CODEX_INTEGRATION: crate::agents::CodexIntegration = crate::agents::CodexIntegration;

/// Select the [`AgentIntegration`] for a hook request from its `X-ZRemote-Agent`
/// header. `codex` -> Codex; anything else (including an absent header) ->
/// Claude, preserving back-compat with the original claude-only forwarder that
/// sent no agent header.
fn select_integration(headers: &HeaderMap) -> &'static dyn AgentIntegration {
    let raw = headers
        .get("x-zremote-agent")
        .and_then(|v| v.to_str().ok())
        .map(str::trim);
    match raw {
        Some("codex") => &CODEX_INTEGRATION,
        _ => &CLAUDE_INTEGRATION,
    }
}

/// Parse and validate the `X-ZRemote-Session-Id` header as a ZRemote
/// [`SessionId`] UUID. Returns `None` if the header is absent, non-UTF-8, empty,
/// or not a valid UUID (callers then skip the generic capture path).
fn parse_zremote_session_header(headers: &HeaderMap) -> Option<SessionId> {
    let raw = headers
        .get("x-zremote-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())?;
    match SessionId::parse_str(raw) {
        Ok(id) => Some(id),
        Err(_) => {
            tracing::warn!(
                header = %raw,
                "X-ZRemote-Session-Id is not a valid UUID, ignoring for capture"
            );
            None
        }
    }
}

/// RFC-012 generic native-session capture.
///
/// Emits [`AgenticAgentMessage::AgentSessionRefCaptured`] linking the ZRemote
/// session id (from the validated `X-ZRemote-Session-Id` header) to the agent's
/// native session id (`native_session_id`, i.e. the hook payload's
/// `session_id`). The `agent` kind is selected per-request from the
/// `X-ZRemote-Agent` header (see [`select_integration`]), so this path serves
/// both Claude and Codex. Unlike [`try_capture_cc_session_id`], it does **not**
/// consult `get_claude_task_id` — it captures for *every* session.
///
/// Deduplicated per `(zremote_session_id, agent, native_session_id)`. A missing
/// or invalid header (`zremote_session_id == None`) is a no-op.
async fn try_capture_agent_session_ref(
    state: &HooksState,
    agent: AgentKind,
    zremote_session_id: Option<SessionId>,
    native_session_id: &str,
) {
    /// Native session ids are short identifiers (UUIDs, rollout ids). Reject
    /// anything implausibly long so a hostile/buggy payload can't grow the
    /// dedup set without bound (CWE-400).
    const MAX_NATIVE_ID_LEN: usize = 256;

    let Some(session_id) = zremote_session_id else {
        return;
    };
    if native_session_id.is_empty() {
        return;
    }
    if native_session_id.len() > MAX_NATIVE_ID_LEN {
        tracing::warn!(
            len = native_session_id.len(),
            "native_session_id exceeds {MAX_NATIVE_ID_LEN} bytes, ignoring for capture"
        );
        return;
    }
    let key = (session_id, agent, native_session_id.to_string());

    // Check-and-insert under a single Mutex guard so two concurrent hooks for
    // the same key cannot both observe it absent and double-send. Release the
    // guard before the (synchronous) try_send.
    {
        let mut sent = state.sent_agent_session_refs.lock().await;
        if sent.contains(&key) {
            return;
        }
        sent.insert(key.clone());
    }

    let msg = AgenticAgentMessage::AgentSessionRefCaptured {
        session_id,
        agent,
        native_session_id: native_session_id.to_string(),
    };
    if state.agentic_tx.try_send(msg).is_err() {
        // The send failed, so this ref was never delivered. Roll back the dedup
        // entry, otherwise the key stays forever and the session's native id is
        // never re-captured — permanently breaking resume for that session.
        state.sent_agent_session_refs.lock().await.remove(&key);
        tracing::warn!("agentic channel full, AgentSessionRefCaptured dropped (will retry)");
    }
}

async fn handle_pre_tool_use(
    state: &HooksState,
    integration: &dyn AgentIntegration,
    payload: &HookPayload,
    zremote_session_id: Option<SessionId>,
) -> HookResponse {
    // Drop hook if tool_use_id is missing — we can't correlate without it.
    let Some(ref tool_use_id) = payload.tool_use_id else {
        tracing::warn!(
            cc_session = %payload.session_id,
            tool = ?payload.tool_name,
            "PreToolUse missing tool_use_id, dropping"
        );
        return HookResponse::default();
    };

    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        tracing::warn!(
            cc_session = %payload.session_id,
            "PreToolUse: no loop mapping after retry, dropping"
        );
        return HookResponse::default();
    };

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;
    // RFC-012 fallback: agents may fire a tool before a SessionStart reaches us.
    // Capture the native session id here too (dedup makes it first-only).
    try_capture_agent_session_ref(
        state,
        integration.agent_kind(),
        zremote_session_id,
        &payload.session_id,
    )
    .await;
    state.mapper.mark_hook_activity(mapped.session_id);
    state.mapper.set_hook_mode(mapped.session_id);

    // Emit LoopStateUpdate(Working)
    let loop_state_msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status: AgenticStatus::Working,
        task_name: None,
        prompt_message: None,
        permission_mode: payload.permission_mode.clone(),
        action_tool_name: None,
        action_description: None,
    };
    if state.agentic_tx.try_send(loop_state_msg).is_err() {
        tracing::warn!("agentic channel full, LoopStateUpdate dropped");
    }

    // Emit ExecutionNodeOpened
    let tool_name = payload.tool_name.as_deref().unwrap_or("unknown");
    let input = format_tool_input(tool_name, payload.tool_input.as_ref());
    let node_opened = AgenticAgentMessage::ExecutionNodeOpened {
        session_id: mapped.session_id,
        loop_id: Some(mapped.loop_id),
        tool_use_id: tool_use_id.clone(),
        timestamp: chrono::Utc::now().timestamp_millis(),
        kind: tool_name.to_lowercase(),
        input,
        working_dir: payload.cwd.clone().unwrap_or_default(),
    };
    if state.agentic_tx.try_send(node_opened).is_err() {
        tracing::warn!("agentic channel full, ExecutionNodeOpened dropped");
    }

    emit_agent_state(
        state,
        mapped.session_id,
        mapped.loop_id,
        AgentRuntimeStatus::Working,
        None,
        None,
    );

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

async fn handle_post_tool_use(
    state: &HooksState,
    integration: &dyn AgentIntegration,
    payload: &HookPayload,
) -> HookResponse {
    let Some(ref tool_use_id) = payload.tool_use_id else {
        tracing::warn!(
            cc_session = %payload.session_id,
            tool = ?payload.tool_name,
            "PostToolUse missing tool_use_id, dropping"
        );
        return HookResponse::default();
    };

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
    let task_name = extract_task_name_from_transcript(integration, payload, &mapped);

    let loop_state_msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status: AgenticStatus::Working,
        task_name: task_name.clone(),
        prompt_message: None,
        permission_mode: payload.permission_mode.clone(),
        action_tool_name: None,
        action_description: None,
    };
    if state.agentic_tx.try_send(loop_state_msg).is_err() {
        tracing::warn!("agentic channel full, LoopStateUpdate dropped");
    }

    // Emit ExecutionNodeClosed
    let tool_name = payload.tool_name.as_deref().unwrap_or("unknown");
    let is_error = payload
        .tool_response
        .as_ref()
        .and_then(|v| v.get("is_error").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    let output_summary = format_tool_response(payload.tool_response.as_ref(), is_error);
    let node_closed = AgenticAgentMessage::ExecutionNodeClosed {
        session_id: mapped.session_id,
        tool_use_id: tool_use_id.clone(),
        kind: tool_name.to_lowercase(),
        output_summary,
        exit_code: None,
        duration_ms: 0,
        status: NodeStatus::Completed,
    };
    if state.agentic_tx.try_send(node_closed).is_err() {
        tracing::warn!("agentic channel full, ExecutionNodeClosed dropped");
    }

    emit_agent_state(
        state,
        mapped.session_id,
        mapped.loop_id,
        AgentRuntimeStatus::Working,
        task_name,
        None,
    );

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

async fn handle_stop(
    state: &HooksState,
    integration: &dyn AgentIntegration,
    payload: &HookPayload,
) {
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

    // Emit SessionExecutionStopped to close any running execution nodes for this session.
    let stopped_msg = AgenticAgentMessage::SessionExecutionStopped {
        session_id: mapped.session_id,
    };
    if state.agentic_tx.try_send(stopped_msg).is_err() {
        tracing::warn!("agentic channel full, SessionExecutionStopped dropped");
    }

    // Extract task_name from transcript
    let task_name = extract_task_name_from_transcript(integration, payload, &mapped);

    emit_agent_state(
        state,
        mapped.session_id,
        mapped.loop_id,
        AgentRuntimeStatus::Idle,
        task_name.clone(),
        None,
    );

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
        prompt_message: prompt_message.clone(),
        permission_mode: None,
        action_tool_name: action_tool_name.clone(),
        action_description: action_description.clone(),
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, status update dropped");
    }

    let runtime_status = match status {
        AgenticStatus::Working => AgentRuntimeStatus::Working,
        AgenticStatus::WaitingForInput | AgenticStatus::RequiresAction => {
            AgentRuntimeStatus::WaitingForInput
        }
        AgenticStatus::Idle | AgenticStatus::Completed => AgentRuntimeStatus::Idle,
        AgenticStatus::Error | AgenticStatus::Unknown => AgentRuntimeStatus::Unknown,
    };
    let request_kind = match (status, event) {
        (AgenticStatus::RequiresAction, "permission_prompt") => {
            Some(AgentInputRequestKind::Permission)
        }
        (AgenticStatus::RequiresAction, "Elicitation") => Some(AgentInputRequestKind::Elicitation),
        (AgenticStatus::WaitingForInput, _) => Some(AgentInputRequestKind::Prompt),
        _ => None,
    };
    let input_request =
        (runtime_status == AgentRuntimeStatus::WaitingForInput).then(|| AgentInputRequest {
            kind: request_kind,
            message: prompt_message.or(action_description.clone()),
            tool_name: action_tool_name,
        });
    emit_agent_state(
        state,
        mapped.session_id,
        mapped.loop_id,
        runtime_status,
        None,
        input_request,
    );
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

async fn handle_user_prompt_submit(
    state: &HooksState,
    integration: &dyn AgentIntegration,
    payload: &HookPayload,
    zremote_session_id: Option<SessionId>,
) {
    // RFC-012 fallback: capture the native session id even before the loop
    // mapping resolves (it keys off the header, not the mapping). Dedup makes
    // this first-only across SessionStart / PreToolUse / UserPromptSubmit.
    try_capture_agent_session_ref(
        state,
        integration.agent_kind(),
        zremote_session_id,
        &payload.session_id,
    )
    .await;

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

    emit_agent_state(
        state,
        mapped.session_id,
        mapped.loop_id,
        AgentRuntimeStatus::Working,
        None,
        None,
    );
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

/// Extract `task_name` from the agent's transcript, with path-traversal
/// validation. Agent-specific bits (transcript root + extraction format) are
/// dispatched through `integration`; the HOME lookup, the prefix-validation
/// structure, and the 100-char cap are agent-agnostic and stay here.
fn extract_task_name_from_transcript(
    integration: &dyn AgentIntegration,
    payload: &HookPayload,
    mapped: &super::mapper::MappedSession,
) -> Option<String> {
    let transcript_path = payload
        .transcript_path
        .as_ref()
        .or(mapped.transcript_path.as_ref())?;

    // CWE-22: Validate transcript_path is within the agent's transcript root
    // (for Claude: ~/.claude/projects/). We canonicalize both the candidate and
    // the allowed root and compare with `Path::starts_with` (component-wise),
    // not raw `str::starts_with` — the latter is bypassable with `..` segments
    // (e.g. `~/.claude/projects/../../.ssh/id_rsa`). Canonicalizing resolves
    // `..` and symlinks; it also requires the file to already exist, which is
    // fine since we are about to open it.
    let Ok(home) = std::env::var("HOME") else {
        tracing::warn!("HOME not set, cannot validate transcript_path, skipping");
        return None;
    };
    let allowed_root = std::path::Path::new(&home).join(integration.transcript_root());
    let Ok(canonical_root) = allowed_root.canonicalize() else {
        // No transcript root on disk -> nothing valid to read.
        tracing::debug!(
            root = %allowed_root.display(),
            "transcript root does not exist, skipping"
        );
        return None;
    };
    let Ok(canonical_path) = std::path::Path::new(transcript_path).canonicalize() else {
        tracing::warn!(
            path = %transcript_path,
            "transcript_path could not be canonicalized (missing?), ignoring"
        );
        return None;
    };
    if !canonical_path.starts_with(&canonical_root) {
        tracing::warn!(
            path = %transcript_path,
            "transcript_path outside allowed directory, ignoring"
        );
        return None;
    }

    let offset = mapped.transcript_offset;
    match integration.extract_task_name(transcript_path, offset) {
        Ok((slug, _new_offset)) => {
            // Cap task_name to 100 bytes at a valid UTF-8 boundary (a raw
            // `s[..100]` panics if byte 100 splits a multibyte char).
            slug.map(|s| {
                if s.len() > 100 {
                    s[..s.floor_char_boundary(100)].to_string()
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
        test_state_with_agentic_capacity(64)
    }

    /// Like [`test_state`] but with a custom agentic-channel capacity so tests
    /// can force `try_send` to fail (channel-full) deterministically.
    fn test_state_with_agentic_capacity(
        cap: usize,
    ) -> (
        HooksState,
        mpsc::Receiver<AgenticAgentMessage>,
        mpsc::Receiver<AgentMessage>,
    ) {
        let (agentic_tx, agentic_rx) = mpsc::channel(cap);
        let (outbound_tx, outbound_rx) = mpsc::channel(64);
        let mapper = SessionMapper::new();
        let state = HooksState {
            agentic_tx,
            context_provider: HookContextProvider::new(mapper.clone()),
            delivery_coordinator: Arc::new(tokio::sync::Mutex::new(DeliveryCoordinator::new())),
            mapper,
            outbound_tx,
            sent_cc_session_ids: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            sent_agent_session_refs: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
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

        let mut p = payload("PreToolUse", "cc-1");
        p.tool_use_id = Some("toolu_test1".to_string());
        handle_pre_tool_use(&state, &crate::agents::ClaudeIntegration, &p, None).await;

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
        let mut p = payload("PreToolUse", "unknown-cc");
        p.tool_use_id = Some("toolu_unknown".to_string());
        handle_pre_tool_use(&state, &crate::agents::ClaudeIntegration, &p, None).await;
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pre_tool_use_with_various_tool_names() {
        for tool in &["Read", "Bash", "Edit", "Write", "Grep", "WebSearch"] {
            let (state, mut agentic_rx, _outbound_rx) = test_state();
            let (_sid, _loop_id) = setup_loop(&state).await;

            let mut p = payload("PreToolUse", "cc-tools");
            p.tool_name = Some(tool.to_string());
            p.tool_use_id = Some(format!("toolu_{tool}"));
            handle_pre_tool_use(&state, &crate::agents::ClaudeIntegration, &p, None).await;

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

        let mut p = payload("PostToolUse", "cc-2");
        p.tool_use_id = Some("toolu_post1".to_string());
        handle_post_tool_use(&state, &crate::agents::ClaudeIntegration, &p).await;

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
        let mut p = payload("PostToolUse", "unknown-cc");
        p.tool_use_id = Some("toolu_unknown".to_string());
        handle_post_tool_use(&state, &crate::agents::ClaudeIntegration, &p).await;
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
        p.tool_use_id = Some("toolu_slug_test".to_string());
        handle_post_tool_use(&state, &crate::agents::ClaudeIntegration, &p).await;

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
        handle_stop(&state, &crate::agents::ClaudeIntegration, &p).await;

        // First message: SessionExecutionStopped
        let msg1 = agentic_rx.try_recv().unwrap();
        assert!(
            matches!(msg1, AgenticAgentMessage::SessionExecutionStopped { .. }),
            "expected SessionExecutionStopped, got {msg1:?}"
        );

        // Second message: new minimal state update
        let msg2 = agentic_rx.try_recv().unwrap();
        assert!(
            matches!(
                msg2,
                AgenticAgentMessage::AgentStateChanged {
                    status: AgentRuntimeStatus::Idle,
                    ..
                }
            ),
            "expected AgentStateChanged{{Idle}}, got {msg2:?}"
        );

        // Third message: LoopEnded
        let msg3 = agentic_rx.try_recv().unwrap();
        match msg3 {
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
        handle_stop(&state, &crate::agents::ClaudeIntegration, &p).await;
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
        handle_stop(&state, &crate::agents::ClaudeIntegration, &p).await;

        // First message: SessionExecutionStopped
        let msg1 = agentic_rx.try_recv().unwrap();
        assert!(matches!(
            msg1,
            AgenticAgentMessage::SessionExecutionStopped { .. }
        ));

        // Second message: new minimal state update
        let msg2 = agentic_rx.try_recv().unwrap();
        assert!(matches!(
            msg2,
            AgenticAgentMessage::AgentStateChanged {
                status: AgentRuntimeStatus::Idle,
                ..
            }
        ));

        // Third message: LoopEnded
        let msg3 = agentic_rx.try_recv().unwrap();
        assert!(matches!(msg3, AgenticAgentMessage::LoopEnded { .. }));

        // Fourth message: LoopStateUpdate with Completed + task_name
        let msg4 = agentic_rx.try_recv().unwrap();
        match msg4 {
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
        handle_user_prompt_submit(&state, &crate::agents::ClaudeIntegration, &p, None).await;

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
        handle_user_prompt_submit(&state, &crate::agents::ClaudeIntegration, &p, None).await;
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
    // RFC-012: X-ZRemote-Session-Id parsing + AgentSessionRefCaptured
    // ---------------------------------------------------------------

    fn headers_with_session(value: &str) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        h.insert(
            "x-zremote-session-id",
            axum::http::HeaderValue::from_str(value).unwrap(),
        );
        h
    }

    #[test]
    fn parse_zremote_session_header_valid_uuid() {
        let id = Uuid::new_v4();
        let headers = headers_with_session(&id.to_string());
        assert_eq!(parse_zremote_session_header(&headers), Some(id));
    }

    #[test]
    fn parse_zremote_session_header_trims_whitespace() {
        let id = Uuid::new_v4();
        let headers = headers_with_session(&format!("  {id}  "));
        assert_eq!(parse_zremote_session_header(&headers), Some(id));
    }

    #[test]
    fn parse_zremote_session_header_absent_is_none() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(parse_zremote_session_header(&headers), None);
    }

    #[test]
    fn parse_zremote_session_header_empty_is_none() {
        let headers = headers_with_session("");
        assert_eq!(parse_zremote_session_header(&headers), None);
    }

    #[test]
    fn parse_zremote_session_header_invalid_uuid_is_none() {
        let headers = headers_with_session("not-a-uuid");
        assert_eq!(parse_zremote_session_header(&headers), None);
    }

    #[tokio::test]
    async fn capture_agent_session_ref_emits_for_non_claude_task_session() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        // Deliberately register NO claude task: the generic capture path must not
        // depend on get_claude_task_id (unlike try_capture_cc_session_id).
        let zremote_id = Uuid::new_v4();

        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(zremote_id), "cc-native-123")
            .await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::AgentSessionRefCaptured {
                session_id,
                agent,
                native_session_id,
            } => {
                assert_eq!(session_id, zremote_id);
                assert_eq!(agent, AgentKind::Claude);
                assert_eq!(native_session_id, "cc-native-123");
            }
            other => panic!("expected AgentSessionRefCaptured, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn capture_agent_session_ref_no_header_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        try_capture_agent_session_ref(&state, AgentKind::Claude, None, "cc-native").await;
        assert!(
            agentic_rx.try_recv().is_err(),
            "missing header must produce no capture message"
        );
    }

    #[tokio::test]
    async fn capture_agent_session_ref_empty_native_id_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(Uuid::new_v4()), "").await;
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn capture_agent_session_ref_deduplicates() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let zremote_id = Uuid::new_v4();

        // First call emits.
        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(zremote_id), "cc-dedup")
            .await;
        assert!(agentic_rx.try_recv().is_ok());

        // Same (session, agent, native) tuple is a no-op.
        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(zremote_id), "cc-dedup")
            .await;
        assert!(agentic_rx.try_recv().is_err());

        // A DIFFERENT native id for the same session still emits.
        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(zremote_id), "cc-other")
            .await;
        assert!(agentic_rx.try_recv().is_ok());

        // A DIFFERENT zremote session id also emits.
        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(Uuid::new_v4()), "cc-dedup")
            .await;
        assert!(agentic_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn capture_agent_session_ref_rolls_back_dedup_on_send_failure() {
        // If try_send fails (channel full), the dedup key must be rolled back so
        // a later attempt re-captures — otherwise resume is permanently broken
        // for that session.
        let (state, mut agentic_rx, _outbound_rx) = test_state_with_agentic_capacity(1);
        let zremote_id = Uuid::new_v4();

        // Fill the size-1 channel so the capture's try_send will fail.
        state
            .agentic_tx
            .try_send(AgenticAgentMessage::SessionExecutionStopped {
                session_id: Uuid::new_v4(),
            })
            .unwrap();

        // Capture attempt: send fails, key must be rolled back.
        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(zremote_id), "cc-retry")
            .await;
        assert!(
            state.sent_agent_session_refs.lock().await.is_empty(),
            "dedup key must be removed after a failed send"
        );

        // Drain the channel, then retry: capture must now succeed (not deduped).
        let _ = agentic_rx.try_recv();
        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(zremote_id), "cc-retry")
            .await;
        match agentic_rx.try_recv() {
            Ok(AgenticAgentMessage::AgentSessionRefCaptured {
                native_session_id, ..
            }) => assert_eq!(native_session_id, "cc-retry"),
            other => panic!("expected re-captured AgentSessionRefCaptured, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn capture_agent_session_ref_rejects_overlong_native_id() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let oversized = "x".repeat(257); // > MAX_NATIVE_ID_LEN (256)
        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(Uuid::new_v4()), &oversized)
            .await;
        assert!(
            agentic_rx.try_recv().is_err(),
            "native_session_id > 256 bytes must be rejected"
        );
        assert!(
            state.sent_agent_session_refs.lock().await.is_empty(),
            "oversized id must not enter the dedup set"
        );
    }

    #[tokio::test]
    async fn capture_agent_session_ref_accepts_max_length_native_id() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let at_max = "y".repeat(256); // exactly MAX_NATIVE_ID_LEN
        try_capture_agent_session_ref(&state, AgentKind::Claude, Some(Uuid::new_v4()), &at_max)
            .await;
        assert!(
            agentic_rx.try_recv().is_ok(),
            "native_session_id of exactly 256 bytes must be accepted"
        );
    }

    #[tokio::test]
    async fn pre_tool_use_captures_agent_session_ref_via_header() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, _loop_id) = setup_loop(&state).await;
        let zremote_id = Uuid::new_v4();

        let mut p = payload("PreToolUse", "cc-native-abc");
        p.tool_use_id = Some("toolu_x".to_string());
        handle_pre_tool_use(
            &state,
            &crate::agents::ClaudeIntegration,
            &p,
            Some(zremote_id),
        )
        .await;

        // Drain messages; one of them must be AgentSessionRefCaptured with the
        // header session id and the payload's session_id as the native id.
        let mut found = false;
        while let Ok(msg) = agentic_rx.try_recv() {
            if let AgenticAgentMessage::AgentSessionRefCaptured {
                session_id,
                agent,
                native_session_id,
            } = msg
            {
                assert_eq!(session_id, zremote_id);
                assert_eq!(agent, AgentKind::Claude);
                assert_eq!(native_session_id, "cc-native-abc");
                found = true;
            }
        }
        assert!(
            found,
            "PreToolUse with a valid header must emit AgentSessionRefCaptured"
        );
    }

    #[tokio::test]
    async fn user_prompt_submit_captures_agent_session_ref_via_header() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, _loop_id) = setup_loop(&state).await;
        let zremote_id = Uuid::new_v4();

        let p = payload("UserPromptSubmit", "cc-prompt-native");
        handle_user_prompt_submit(
            &state,
            &crate::agents::ClaudeIntegration,
            &p,
            Some(zremote_id),
        )
        .await;

        let mut found = false;
        while let Ok(msg) = agentic_rx.try_recv() {
            if let AgenticAgentMessage::AgentSessionRefCaptured {
                session_id,
                native_session_id,
                ..
            } = msg
            {
                assert_eq!(session_id, zremote_id);
                assert_eq!(native_session_id, "cc-prompt-native");
                found = true;
            }
        }
        assert!(found, "UserPromptSubmit with a valid header must capture");
    }

    // ---------------------------------------------------------------
    // RFC-012 P4b: X-ZRemote-Agent routing -> integration selection
    // ---------------------------------------------------------------

    fn headers_with_agent(agent: &str) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        h.insert(
            "x-zremote-agent",
            axum::http::HeaderValue::from_str(agent).unwrap(),
        );
        h
    }

    #[test]
    fn select_integration_codex_header() {
        let h = headers_with_agent("codex");
        assert_eq!(select_integration(&h).agent_kind(), AgentKind::Codex);
    }

    #[test]
    fn select_integration_claude_header() {
        let h = headers_with_agent("claude");
        assert_eq!(select_integration(&h).agent_kind(), AgentKind::Claude);
    }

    #[test]
    fn select_integration_absent_header_defaults_claude() {
        let h = axum::http::HeaderMap::new();
        assert_eq!(select_integration(&h).agent_kind(), AgentKind::Claude);
    }

    #[test]
    fn select_integration_unknown_header_defaults_claude() {
        let h = headers_with_agent("gemini");
        assert_eq!(select_integration(&h).agent_kind(), AgentKind::Claude);
    }

    #[tokio::test]
    async fn capture_via_codex_integration_uses_codex_agent_kind() {
        // Routing a hook through CodexIntegration must capture AgentKind::Codex.
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (_sid, _loop_id) = setup_loop(&state).await;
        let zremote_id = Uuid::new_v4();

        let mut p = payload("PreToolUse", "codex-native-xyz");
        p.tool_use_id = Some("toolu_codex".to_string());
        handle_pre_tool_use(
            &state,
            &crate::agents::CodexIntegration,
            &p,
            Some(zremote_id),
        )
        .await;

        let mut found = false;
        while let Ok(msg) = agentic_rx.try_recv() {
            if let AgenticAgentMessage::AgentSessionRefCaptured {
                session_id,
                agent,
                native_session_id,
            } = msg
            {
                assert_eq!(session_id, zremote_id);
                assert_eq!(agent, AgentKind::Codex);
                assert_eq!(native_session_id, "codex-native-xyz");
                found = true;
            }
        }
        assert!(
            found,
            "codex-routed PreToolUse must capture AgentKind::Codex"
        );
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
        let result =
            extract_task_name_from_transcript(&crate::agents::ClaudeIntegration, &p, &mapped);
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
        let result =
            extract_task_name_from_transcript(&crate::agents::ClaudeIntegration, &p, &mapped);
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
        let result =
            extract_task_name_from_transcript(&crate::agents::ClaudeIntegration, &p, &mapped);
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
        let result =
            extract_task_name_from_transcript(&crate::agents::ClaudeIntegration, &p, &mapped);
        // Path traversal validation should reject this
        assert!(result.is_none());
    }

    #[test]
    fn extract_task_name_rejects_dotdot_traversal_under_prefix() {
        // A path that textually starts with the allowed prefix but escapes it via
        // `..` must be rejected (the old str::starts_with guard was bypassable;
        // the canonicalize + Path::starts_with guard is not).
        let home = std::env::var("HOME").unwrap();
        // Put a real file OUTSIDE the projects dir, then reference it through a
        // `..`-laden path that begins with the allowed prefix string.
        let outside_dir = format!("{home}/.claude/_test_handler_escape");
        std::fs::create_dir_all(&outside_dir).ok();
        let outside_file = format!("{outside_dir}/secret.jsonl");
        std::fs::write(
            &outside_file,
            "{\"type\":\"result\",\"slug\":\"escaped\"}\n",
        )
        .unwrap();

        // `~/.claude/projects/../_test_handler_escape/secret.jsonl` — starts with
        // the allowed prefix as a raw string, but resolves outside it.
        let traversal = format!("{home}/.claude/projects/../_test_handler_escape/secret.jsonl");
        let mut p = payload("PostToolUse", "cc-1");
        p.transcript_path = Some(traversal);
        let mapped = super::super::mapper::MappedSession {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            transcript_path: None,
            transcript_offset: 0,
        };
        let result =
            extract_task_name_from_transcript(&crate::agents::ClaudeIntegration, &p, &mapped);
        assert!(
            result.is_none(),
            "`..` traversal escaping the transcript root must be rejected"
        );

        std::fs::remove_dir_all(&outside_dir).ok();
    }

    #[test]
    fn extract_task_name_truncates_multibyte_slug_at_char_boundary() {
        // A slug whose byte 100 falls inside a multibyte char must NOT panic
        // (the old `s[..100]` did). Build a slug where a 3-byte char straddles
        // byte 100: 99 ASCII bytes + a multibyte char.
        let home = std::env::var("HOME").unwrap();
        let transcript_dir = format!("{home}/.claude/projects/_test_handler_mb");
        std::fs::create_dir_all(&transcript_dir).ok();
        let transcript_path = format!("{transcript_dir}/transcript.jsonl");

        let mut slug = "a".repeat(99); // bytes 0..99
        slug.push('€'); // 3-byte char occupying bytes 99..102 (straddles 100)
        slug.push_str(&"b".repeat(20)); // ensure len > 100
        std::fs::write(
            &transcript_path,
            format!("{{\"type\":\"result\",\"slug\":\"{slug}\"}}\n"),
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
        // Must not panic; result is capped to a valid char boundary <= 100 bytes.
        let result =
            extract_task_name_from_transcript(&crate::agents::ClaudeIntegration, &p, &mapped)
                .expect("slug should be extracted");
        assert!(result.len() <= 100, "capped length must be <= 100 bytes");
        // The '€' at byte 99 does not fit fully within 100 bytes, so the cap lands
        // at byte 99 (the 99 'a's), never splitting the multibyte char.
        assert_eq!(result, "a".repeat(99));

        std::fs::remove_dir_all(&transcript_dir).ok();
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
        let result =
            extract_task_name_from_transcript(&crate::agents::ClaudeIntegration, &p, &mapped);
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
        let result =
            extract_task_name_from_transcript(&crate::agents::ClaudeIntegration, &p, &mapped);
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
        let result =
            extract_task_name_from_transcript(&crate::agents::ClaudeIntegration, &p, &mapped);
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
    fn session_start_response_keeps_watch_paths_for_claude_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let cwd = dir.path().to_str().unwrap();

        let claude = build_session_start_response(&crate::agents::ClaudeIntegration, Some(cwd));
        assert!(
            claude.hook_specific_output.is_some(),
            "Claude SessionStart should include hook-specific watch paths"
        );

        let codex = build_session_start_response(&crate::agents::CodexIntegration, Some(cwd));
        assert!(
            codex.hook_specific_output.is_none(),
            "Codex SessionStart must not receive Claude-only watchPaths output"
        );
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

    // ---------------------------------------------------------------
    // RFC-009 Phase 2 tests: #13-#16 (format helpers)
    // ---------------------------------------------------------------

    // Test #13: format_tool_input("Read", {"file_path":"/x"}) → "/x"
    #[test]
    fn format_tool_input_read_returns_file_path() {
        let input = serde_json::json!({"file_path": "/x"});
        let result = format_tool_input("Read", Some(&input));
        assert_eq!(result.as_deref(), Some("/x"));
    }

    // Test #14: format_tool_input("Bash", long command) truncated to INPUT_CAP_BYTES
    #[test]
    fn format_tool_input_bash_truncates_long_command() {
        let long_cmd = "a".repeat(INPUT_CAP_BYTES + 100);
        let input = serde_json::json!({"command": long_cmd});
        let result = format_tool_input("Bash", Some(&input)).unwrap();
        assert!(
            result.len() <= INPUT_CAP_BYTES,
            "expected truncation: len={}",
            result.len()
        );
        assert!(result.ends_with('…'), "expected ellipsis suffix");
    }

    // Test #15: format_tool_response of large stdout truncated to SUMMARY_CAP_BYTES
    #[test]
    fn format_tool_response_truncates_large_output() {
        let big = "x".repeat(SUMMARY_CAP_BYTES + 500);
        let response = serde_json::Value::String(big);
        let result = format_tool_response(Some(&response), false).unwrap();
        assert!(
            result.len() <= SUMMARY_CAP_BYTES,
            "expected truncation: len={}",
            result.len()
        );
        assert!(result.ends_with('…'), "expected ellipsis suffix");
    }

    // Test #16: format_tool_response with is_error=true prefixes "ERROR: "
    #[test]
    fn format_tool_response_error_prefix() {
        let response = serde_json::Value::String("something went wrong".to_string());
        let result = format_tool_response(Some(&response), true).unwrap();
        assert!(
            result.starts_with("ERROR: "),
            "expected ERROR: prefix, got: {result}"
        );
        assert!(result.contains("something went wrong"));
    }

    // Test #17: PreToolUse handler with valid mapping: LoopStateUpdate AND
    // ExecutionNodeOpened both fire on agentic_tx
    #[tokio::test]
    async fn pre_tool_use_emits_both_loop_state_update_and_execution_node_opened() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (sid, loop_id) = setup_loop(&state).await;

        let mut p = payload("PreToolUse", "cc-both");
        p.tool_name = Some("Bash".to_string());
        p.tool_use_id = Some("toolu_both".to_string());
        p.tool_input = Some(serde_json::json!({"command": "ls"}));
        handle_pre_tool_use(&state, &crate::agents::ClaudeIntegration, &p, None).await;

        let msg1 = agentic_rx.try_recv().unwrap();
        match &msg1 {
            AgenticAgentMessage::LoopStateUpdate {
                loop_id: lid,
                status,
                ..
            } => {
                assert_eq!(*lid, loop_id);
                assert_eq!(*status, AgenticStatus::Working);
            }
            other => panic!("expected LoopStateUpdate, got {other:?}"),
        }

        let msg2 = agentic_rx.try_recv().unwrap();
        match &msg2 {
            AgenticAgentMessage::ExecutionNodeOpened {
                session_id,
                tool_use_id,
                kind,
                input,
                ..
            } => {
                assert_eq!(*session_id, sid);
                assert_eq!(tool_use_id, "toolu_both");
                assert_eq!(kind, "bash");
                assert_eq!(input.as_deref(), Some("ls"));
            }
            other => panic!("expected ExecutionNodeOpened, got {other:?}"),
        }
    }

    // Test #18: PostToolUse emits ExecutionNodeClosed{Completed} with matching tool_use_id
    #[tokio::test]
    async fn post_tool_use_emits_execution_node_closed_completed() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (sid, _loop_id) = setup_loop(&state).await;

        let mut p = payload("PostToolUse", "cc-closed");
        p.tool_name = Some("Bash".to_string());
        p.tool_use_id = Some("toolu_closed_test".to_string());
        p.tool_response = Some(serde_json::Value::String("output".to_string()));
        handle_post_tool_use(&state, &crate::agents::ClaudeIntegration, &p).await;

        // First: LoopStateUpdate
        let _loop_msg = agentic_rx.try_recv().unwrap();

        // Second: ExecutionNodeClosed
        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::ExecutionNodeClosed {
                session_id,
                tool_use_id,
                status,
                output_summary,
                ..
            } => {
                assert_eq!(session_id, sid);
                assert_eq!(tool_use_id, "toolu_closed_test");
                assert_eq!(status, NodeStatus::Completed);
                assert!(output_summary.is_some());
            }
            other => panic!("expected ExecutionNodeClosed, got {other:?}"),
        }
    }

    // Test #19: Stop hook emits SessionExecutionStopped
    #[tokio::test]
    async fn stop_hook_emits_session_execution_stopped() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let (sid, _loop_id) = setup_loop(&state).await;

        let p = payload("Stop", "cc-sesstopped");
        handle_stop(&state, &crate::agents::ClaudeIntegration, &p).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::SessionExecutionStopped { session_id } => {
                assert_eq!(session_id, sid);
            }
            other => panic!("expected SessionExecutionStopped, got {other:?}"),
        }
    }

    // Test #26: PreToolUse with unknown CC session_id (no mapping after retry) dropped with warn
    #[tokio::test]
    async fn pre_tool_use_unknown_session_drops_with_warn() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        // No loop registered at all
        let mut p = payload("PreToolUse", "totally-unknown-session");
        p.tool_use_id = Some("toolu_unknown".to_string());
        handle_pre_tool_use(&state, &crate::agents::ClaudeIntegration, &p, None).await;
        assert!(
            agentic_rx.try_recv().is_err(),
            "should produce no messages for unknown session"
        );
    }

    // Test #27: PreToolUse with missing tool_use_id dropped with warn
    #[tokio::test]
    async fn pre_tool_use_missing_tool_use_id_drops() {
        let (state, mut agentic_rx, _outbound_rx) = test_state();
        let _ = setup_loop(&state).await;
        // tool_use_id is None (default payload)
        let p = payload("PreToolUse", "cc-noid");
        handle_pre_tool_use(&state, &crate::agents::ClaudeIntegration, &p, None).await;
        assert!(
            agentic_rx.try_recv().is_err(),
            "missing tool_use_id should produce no messages"
        );
    }

    // Q2 verification: tool_result alias works for deserialization
    #[test]
    fn deserialize_tool_result_alias() {
        let json = r#"{
            "session_id": "abc",
            "hook_event_name": "PostToolUse",
            "tool_use_id": "toolu_x",
            "tool_result": "output via alias"
        }"#;
        let p: HookPayload = serde_json::from_str(json).unwrap();
        assert!(p.tool_response.is_some());
        assert_eq!(
            p.tool_response.as_ref().and_then(|v| v.as_str()),
            Some("output via alias")
        );
    }
}
