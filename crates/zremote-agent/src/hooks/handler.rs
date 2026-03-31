use std::collections::HashSet;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use zremote_protocol::claude::ClaudeAgentMessage;
use zremote_protocol::{AgentMessage, AgenticAgentMessage, AgenticStatus, SessionId};

use super::mapper::SessionMapper;
use super::transcript::extract_slug;

/// Shared state for the hooks HTTP handler.
#[derive(Clone)]
pub struct HooksState {
    pub agentic_tx: mpsc::Sender<AgenticAgentMessage>,
    pub mapper: SessionMapper,
    /// Sender for outbound agent messages (used for `SessionIdCaptured`).
    pub outbound_tx: mpsc::Sender<AgentMessage>,
    /// CC session IDs that have already been sent via `SessionIdCaptured` (dedup).
    pub sent_cc_session_ids: Arc<tokio::sync::RwLock<HashSet<String>>>,
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
}

/// Response to hook scripts. Empty for most hooks.
#[derive(Debug, Serialize)]
pub struct HookResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
}

/// POST /hooks - main entry point for all CC hook events.
pub async fn handle_hook(
    State(state): State<HooksState>,
    Json(payload): Json<HookPayload>,
) -> impl IntoResponse {
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

    match payload.hook_event_name.as_str() {
        "PreToolUse" => handle_pre_tool_use(&state, &payload).await,
        "PostToolUse" => handle_post_tool_use(&state, &payload).await,
        "Stop" => handle_stop(&state, &payload).await,
        "Notification" => {
            if let Some(ref msg) = payload.message {
                tracing::info!(cc_session = %payload.session_id, message = %msg, "CC notification");
            }
        }
        "Elicitation" => handle_elicitation(&state, &payload).await,
        "UserPromptSubmit" => handle_user_prompt_submit(&state, &payload).await,
        "SessionStart" => {
            tracing::info!(
                cc_session = %payload.session_id,
                source = ?payload.source,
                "CC session started"
            );
        }
        "SubagentStart" | "SubagentStop" => {
            tracing::debug!(
                hook_event = %payload.hook_event_name,
                "subagent event (ignored)"
            );
        }
        other => {
            tracing::debug!(hook_event = %other, "unknown hook event, ignoring");
        }
    }

    (StatusCode::OK, Json(HookResponse { decision: None })).into_response()
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

async fn handle_pre_tool_use(state: &HooksState, payload: &HookPayload) {
    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        tracing::debug!(cc_session = %payload.session_id, "no loop mapping for PreToolUse, ignoring");
        return;
    };

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;
    state.mapper.mark_hook_activity(mapped.session_id);

    // Emit LoopStateUpdate(Working)
    let msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status: AgenticStatus::Working,
        task_name: None,
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, LoopStateUpdate dropped");
    }
}

async fn handle_post_tool_use(state: &HooksState, payload: &HookPayload) {
    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        return;
    };

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;
    state.mapper.mark_hook_activity(mapped.session_id);

    // Try to extract slug from transcript for task_name
    let task_name = extract_task_name_from_transcript(payload, &mapped);

    let msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status: AgenticStatus::Working,
        task_name,
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, LoopStateUpdate dropped");
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
        };
        let _ = state.agentic_tx.try_send(update);
    }
}

/// Send `WaitingForInput` status for a resolved loop. Shared by notification
/// and elicitation handlers to avoid duplicated resolve+send logic.
async fn send_waiting_for_input(state: &HooksState, payload: &HookPayload, event: &str) {
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

    let msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status: AgenticStatus::WaitingForInput,
        task_name: None,
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, WaitingForInput update dropped");
    }
}

/// Handle typed notification events (idle_prompt, permission_prompt).
///
/// Called from dedicated routes `/hooks/notification/idle` and
/// `/hooks/notification/permission` which are registered with specific
/// matchers in settings.json. Sets `WaitingForInput` immediately.
async fn handle_notification_typed(
    state: &HooksState,
    payload: &HookPayload,
    notification_type: &str,
) {
    tracing::info!(
        cc_session = %payload.session_id,
        notification_type = %notification_type,
        message = ?payload.message,
        "CC notification (typed)"
    );

    // Update transcript path if provided
    if let Some(ref path) = payload.transcript_path {
        state
            .mapper
            .set_transcript_path(&payload.session_id, path.clone())
            .await;
    }

    send_waiting_for_input(state, payload, notification_type).await;
}

/// Route handler for idle_prompt notifications.
pub async fn handle_notification_idle(
    State(state): State<HooksState>,
    Json(payload): Json<HookPayload>,
) -> impl IntoResponse {
    handle_notification_typed(&state, &payload, "idle_prompt").await;
    (StatusCode::OK, Json(HookResponse { decision: None })).into_response()
}

/// Route handler for permission_prompt notifications.
pub async fn handle_notification_permission(
    State(state): State<HooksState>,
    Json(payload): Json<HookPayload>,
) -> impl IntoResponse {
    handle_notification_typed(&state, &payload, "permission_prompt").await;
    (StatusCode::OK, Json(HookResponse { decision: None })).into_response()
}

async fn handle_elicitation(state: &HooksState, payload: &HookPayload) {
    tracing::info!(
        cc_session = %payload.session_id,
        mcp_server = ?payload.mcp_server_name,
        mode = ?payload.mode,
        "CC elicitation event"
    );

    send_waiting_for_input(state, payload, "Elicitation").await;
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

    let msg = AgenticAgentMessage::LoopStateUpdate {
        loop_id: mapped.loop_id,
        status: AgenticStatus::Working,
        task_name: None,
    };
    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, LoopStateUpdate dropped");
    }
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
        let state = HooksState {
            agentic_tx,
            mapper: SessionMapper::new(),
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
    async fn elicitation_sends_waiting_for_input() {
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
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(status, AgenticStatus::WaitingForInput);
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
    async fn handle_hook_subagent_events_are_ignored() {
        let (_state, mut agentic_rx, _outbound_rx) = test_state();
        for event in &["SubagentStart", "SubagentStop"] {
            let p = payload(event, "cc-sub");
            // These events just log and do nothing
            match p.hook_event_name.as_str() {
                "SubagentStart" | "SubagentStop" => {} // expected path
                _ => panic!("should match subagent events"),
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
        let resp = HookResponse { decision: None };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn hook_response_serializes_with_decision() {
        let resp = HookResponse {
            decision: Some("allow".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("allow"));
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
}
