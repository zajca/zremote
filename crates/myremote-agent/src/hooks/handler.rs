use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use myremote_protocol::claude::ClaudeAgentMessage;
use myremote_protocol::{AgentMessage, AgenticAgentMessage, SessionId, ToolCallStatus};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::mapper::SessionMapper;
use super::metrics::aggregate_metrics;
use super::permission::PermissionManager;
use super::transcript::parse_transcript_file;

/// Shared state for the hooks HTTP handler.
#[derive(Clone)]
pub struct HooksState {
    pub agentic_tx: mpsc::Sender<AgenticAgentMessage>,
    pub mapper: SessionMapper,
    pub permission_manager: Arc<PermissionManager>,
    /// Track tool call start times for duration calculation.
    pub tool_call_starts: Arc<tokio::sync::RwLock<std::collections::HashMap<String, Instant>>>,
    /// Sender for outbound agent messages (used for `SessionIdCaptured`).
    pub outbound_tx: mpsc::Sender<AgentMessage>,
    /// CC session IDs that have already been sent via `SessionIdCaptured` (dedup).
    pub sent_cc_session_ids: Arc<tokio::sync::RwLock<HashSet<String>>>,
}

/// The JSON payload received from Claude Code hooks via stdin.
#[derive(Debug, Deserialize)]
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
}

/// Response to hook scripts. For most hooks this is empty.
/// For PermissionRequest, it may contain a decision.
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
        "PermissionRequest" => {
            return handle_permission_request(&state, &payload).await;
        }
        "Notification" => {
            if let Some(ref msg) = payload.message {
                tracing::info!(cc_session = %payload.session_id, message = %msg, "CC notification");
            }
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

    let tool_use_id = payload.tool_use_id.as_deref().unwrap_or("unknown");

    let tool_call_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, tool_use_id.as_bytes());

    let tool_name = payload
        .tool_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let arguments_json = payload
        .tool_input
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_default();

    // Track start time for duration calculation
    state
        .tool_call_starts
        .write()
        .await
        .insert(tool_use_id.to_string(), Instant::now());

    let msg = AgenticAgentMessage::LoopToolCall {
        loop_id: mapped.loop_id,
        tool_call_id,
        tool_name,
        arguments_json,
        status: ToolCallStatus::Pending,
    };

    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, LoopToolCall dropped");
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

    let tool_use_id = payload.tool_use_id.as_deref().unwrap_or("unknown");

    let tool_call_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, tool_use_id.as_bytes());

    // Calculate duration from PreToolUse
    let duration_ms = state
        .tool_call_starts
        .write()
        .await
        .remove(tool_use_id)
        .map(|start| start.elapsed().as_millis() as u64)
        .unwrap_or(0);

    let result_preview = payload
        .tool_response
        .as_ref()
        .map(|v| {
            let s = serde_json::to_string(v).unwrap_or_default();
            if s.len() > 500 {
                format!("{}...", &s[..500])
            } else {
                s
            }
        })
        .unwrap_or_default();

    let msg = AgenticAgentMessage::LoopToolResult {
        loop_id: mapped.loop_id,
        tool_call_id,
        result_preview,
        duration_ms,
    };

    if state.agentic_tx.try_send(msg).is_err() {
        tracing::warn!("agentic channel full, LoopToolResult dropped");
    }

    // Incrementally parse transcript to emit live metrics after each tool call
    emit_incremental_metrics(state, payload, &mapped).await;
}

/// Parse the transcript file incrementally and emit updated LoopMetrics.
/// Called after each PostToolUse to provide real-time token/cost updates.
///
/// Reads new transcript entries from the last offset (to avoid duplicates),
/// but aggregates metrics from the **entire** file (offset 0) to get correct totals.
async fn emit_incremental_metrics(
    state: &HooksState,
    payload: &HookPayload,
    mapped: &super::mapper::MappedSession,
) {
    let transcript_path = if let Some(ref path) = payload.transcript_path {
        path.clone()
    } else if let Some(ref path) = mapped.transcript_path {
        path.clone()
    } else {
        return;
    };

    // Read new transcript entries from last offset (incremental)
    let offset = mapped.transcript_offset;
    match parse_transcript_file(&transcript_path, offset).await {
        Ok((entries, new_offset, _)) => {
            // Emit any new transcript entries
            for entry in entries {
                let msg = AgenticAgentMessage::LoopTranscript {
                    loop_id: mapped.loop_id,
                    role: entry.role,
                    content: entry.content,
                    tool_call_id: entry.tool_call_id,
                    timestamp: Utc::now(),
                };
                if state.agentic_tx.try_send(msg).is_err() {
                    tracing::warn!("agentic channel full, LoopTranscript dropped");
                    break;
                }
            }

            // Update offset for next incremental parse
            state
                .mapper
                .set_transcript_offset(&payload.session_id, new_offset)
                .await;
        }
        Err(e) => {
            tracing::debug!(
                path = %transcript_path,
                error = %e,
                "failed to parse transcript for incremental entries"
            );
        }
    }

    // Read entire transcript from the start (offset 0) to aggregate total metrics
    match parse_transcript_file(&transcript_path, 0).await {
        Ok((_, _, token_data)) => {
            if let Some(metrics) = aggregate_metrics(&token_data) {
                let msg = AgenticAgentMessage::LoopMetrics {
                    loop_id: mapped.loop_id,
                    tokens_in: metrics.tokens_in,
                    tokens_out: metrics.tokens_out,
                    model: metrics.model,
                    context_used: metrics.context_used,
                    context_max: metrics.context_max,
                    estimated_cost_usd: metrics.estimated_cost_usd,
                };
                if state.agentic_tx.try_send(msg).is_err() {
                    tracing::warn!("agentic channel full, LoopMetrics dropped");
                }
            }
        }
        Err(e) => {
            tracing::debug!(
                path = %transcript_path,
                error = %e,
                "failed to parse transcript for metrics aggregation"
            );
        }
    }
}

async fn handle_stop(state: &HooksState, payload: &HookPayload) {
    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        return;
    };

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;

    // Parse transcript file for conversation entries
    if let Some(ref transcript_path) = payload.transcript_path {
        let offset = mapped.transcript_offset;
        match parse_transcript_file(transcript_path, offset).await {
            Ok((entries, new_offset, token_data)) => {
                // Emit transcript entries
                for entry in entries {
                    let msg = AgenticAgentMessage::LoopTranscript {
                        loop_id: mapped.loop_id,
                        role: entry.role,
                        content: entry.content,
                        tool_call_id: entry.tool_call_id,
                        timestamp: Utc::now(),
                    };
                    if state.agentic_tx.try_send(msg).is_err() {
                        tracing::warn!("agentic channel full, LoopTranscript dropped");
                        break;
                    }
                }

                // Emit aggregated metrics
                if let Some(metrics) = aggregate_metrics(&token_data) {
                    let msg = AgenticAgentMessage::LoopMetrics {
                        loop_id: mapped.loop_id,
                        tokens_in: metrics.tokens_in,
                        tokens_out: metrics.tokens_out,
                        model: metrics.model,
                        context_used: metrics.context_used,
                        context_max: metrics.context_max,
                        estimated_cost_usd: metrics.estimated_cost_usd,
                    };
                    if state.agentic_tx.try_send(msg).is_err() {
                        tracing::warn!("agentic channel full, LoopMetrics dropped");
                    }
                }

                // Update offset for incremental parsing
                state
                    .mapper
                    .set_transcript_offset(&payload.session_id, new_offset)
                    .await;
            }
            Err(e) => {
                tracing::warn!(
                    path = %transcript_path,
                    error = %e,
                    "failed to parse transcript file"
                );
            }
        }
    }
}

async fn handle_permission_request(
    state: &HooksState,
    payload: &HookPayload,
) -> axum::response::Response {
    let Some(mapped) = state
        .mapper
        .resolve_loop_id(&payload.session_id, payload.cwd.as_deref())
        .await
    else {
        // No loop mapping - pass through (exit 0 equivalent)
        return (StatusCode::OK, Json(HookResponse { decision: None })).into_response();
    };

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;

    let tool_name = payload
        .tool_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let tool_input = payload
        .tool_input
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_default();

    // Check permission rules first
    let decision = state
        .permission_manager
        .check_permission(&tool_name, mapped.loop_id, &tool_input, &state.agentic_tx)
        .await;

    match decision {
        super::permission::PermissionDecision::Allow => (
            StatusCode::OK,
            Json(HookResponse {
                decision: Some("allow".to_string()),
            }),
        )
            .into_response(),
        super::permission::PermissionDecision::Deny => {
            // Return 200 with empty body - CC will show terminal prompt
            (
                StatusCode::OK,
                Json(HookResponse {
                    decision: Some("deny".to_string()),
                }),
            )
                .into_response()
        }
        super::permission::PermissionDecision::Ask => {
            // Emit pending tool call and wait for user decision
            let tool_use_id = payload.tool_use_id.as_deref().unwrap_or("unknown");
            let tool_call_id =
                uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, tool_use_id.as_bytes());

            let msg = AgenticAgentMessage::LoopToolCall {
                loop_id: mapped.loop_id,
                tool_call_id,
                tool_name: tool_name.clone(),
                arguments_json: tool_input,
                status: ToolCallStatus::Pending,
            };
            if state.agentic_tx.try_send(msg).is_err() {
                tracing::warn!("agentic channel full, permission LoopToolCall dropped");
            }

            // Wait for user decision (up to 55s)
            let result = state
                .permission_manager
                .wait_for_decision(mapped.loop_id, &tool_name)
                .await;

            match result {
                super::permission::PermissionDecision::Allow => (
                    StatusCode::OK,
                    Json(HookResponse {
                        decision: Some("allow".to_string()),
                    }),
                )
                    .into_response(),
                super::permission::PermissionDecision::Deny => (
                    StatusCode::OK,
                    Json(HookResponse {
                        decision: Some("deny".to_string()),
                    }),
                )
                    .into_response(),
                super::permission::PermissionDecision::Ask => {
                    // Timeout - pass through to terminal
                    (StatusCode::OK, Json(HookResponse { decision: None })).into_response()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_pre_tool_use_hook() {
        let json = r#"{
            "session_id": "abc-123",
            "hook_event_name": "PreToolUse",
            "transcript_path": "/home/user/.claude/projects/foo/session.jsonl",
            "cwd": "/home/user/project",
            "tool_name": "Read",
            "tool_input": {"file_path": "/src/main.rs"},
            "tool_use_id": "toolu_abc123"
        }"#;

        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "PreToolUse");
        assert_eq!(payload.session_id, "abc-123");
        assert_eq!(payload.tool_name.as_deref(), Some("Read"));
        assert_eq!(payload.tool_use_id.as_deref(), Some("toolu_abc123"));
        assert!(payload.tool_input.is_some());
    }

    #[test]
    fn deserialize_post_tool_use_hook() {
        let json = r#"{
            "session_id": "abc-123",
            "hook_event_name": "PostToolUse",
            "tool_name": "Read",
            "tool_use_id": "toolu_abc123",
            "tool_response": "file contents here..."
        }"#;

        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "PostToolUse");
        assert!(payload.tool_response.is_some());
    }

    #[test]
    fn deserialize_stop_hook() {
        let json = r#"{
            "session_id": "abc-123",
            "hook_event_name": "Stop",
            "transcript_path": "/home/user/.claude/projects/foo/session.jsonl",
            "stop_hook_active": true
        }"#;

        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "Stop");
        assert!(payload.transcript_path.is_some());
    }

    #[test]
    fn deserialize_permission_request_hook() {
        let json = r#"{
            "session_id": "abc-123",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /tmp/test"}
        }"#;

        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "PermissionRequest");
        assert_eq!(payload.tool_name.as_deref(), Some("Bash"));
    }

    #[test]
    fn deserialize_notification_hook() {
        let json = r#"{
            "session_id": "abc-123",
            "hook_event_name": "Notification",
            "message": "Task completed"
        }"#;

        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "Notification");
        assert_eq!(payload.message.as_deref(), Some("Task completed"));
    }

    #[test]
    fn deserialize_minimal_hook() {
        let json = r#"{
            "session_id": "abc-123",
            "hook_event_name": "SubagentStart"
        }"#;

        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.hook_event_name, "SubagentStart");
        assert!(payload.tool_name.is_none());
        assert!(payload.transcript_path.is_none());
    }

    #[test]
    fn tool_call_id_deterministic() {
        let id1 = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"toolu_abc123");
        let id2 = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"toolu_abc123");
        assert_eq!(id1, id2);

        let id3 = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"toolu_xyz789");
        assert_ne!(id1, id3);
    }

    #[test]
    fn hook_response_serialization() {
        let resp = HookResponse { decision: None };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, "{}");

        let resp = HookResponse {
            decision: Some("allow".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("allow"));
    }

    #[test]
    fn hook_response_deny_serialization() {
        let resp = HookResponse {
            decision: Some("deny".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("deny"));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["decision"], "deny");
    }

    #[test]
    fn deserialize_hook_with_all_fields() {
        let json = r#"{
            "session_id": "sess-full",
            "hook_event_name": "PreToolUse",
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": "/home/user/project",
            "tool_name": "Bash",
            "tool_input": {"command": "ls -la"},
            "tool_use_id": "toolu_full",
            "tool_response": {"output": "file1\nfile2"},
            "message": "notification text"
        }"#;

        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.session_id, "sess-full");
        assert_eq!(payload.hook_event_name, "PreToolUse");
        assert_eq!(
            payload.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl")
        );
        assert_eq!(payload.cwd.as_deref(), Some("/home/user/project"));
        assert_eq!(payload.tool_name.as_deref(), Some("Bash"));
        assert!(payload.tool_input.is_some());
        assert_eq!(payload.tool_use_id.as_deref(), Some("toolu_full"));
        assert!(payload.tool_response.is_some());
        assert_eq!(payload.message.as_deref(), Some("notification text"));
    }

    #[test]
    fn deserialize_hook_with_only_required_fields() {
        let json = r#"{
            "session_id": "minimal",
            "hook_event_name": "Stop"
        }"#;

        let payload: HookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.session_id, "minimal");
        assert_eq!(payload.hook_event_name, "Stop");
        assert!(payload.transcript_path.is_none());
        assert!(payload.cwd.is_none());
        assert!(payload.tool_name.is_none());
        assert!(payload.tool_input.is_none());
        assert!(payload.tool_use_id.is_none());
        assert!(payload.tool_response.is_none());
        assert!(payload.message.is_none());
    }

    #[test]
    fn tool_call_id_different_for_different_inputs() {
        let id_a = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"toolu_a");
        let id_b = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"toolu_b");
        let id_c = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"toolu_c");
        assert_ne!(id_a, id_b);
        assert_ne!(id_b, id_c);
        assert_ne!(id_a, id_c);
    }

    #[test]
    fn tool_call_id_unknown_fallback() {
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"unknown");
        // Should still produce a valid UUID
        assert_eq!(id.get_version(), Some(uuid::Version::Sha1));
    }

    /// Helper to create a test `HooksState` with channels.
    fn make_test_hooks_state() -> (
        HooksState,
        mpsc::Receiver<AgenticAgentMessage>,
        mpsc::Receiver<AgentMessage>,
    ) {
        let (agentic_tx, agentic_rx) = mpsc::channel(64);
        let (outbound_tx, outbound_rx) = mpsc::channel(64);
        let state = HooksState {
            agentic_tx,
            mapper: SessionMapper::new(),
            permission_manager: Arc::new(PermissionManager::new()),
            tool_call_starts: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            outbound_tx,
            sent_cc_session_ids: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
        };
        (state, agentic_rx, outbound_rx)
    }

    #[tokio::test]
    async fn handle_pre_tool_use_no_mapping_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();
        let payload = HookPayload {
            session_id: "unknown-session".to_string(),
            hook_event_name: "PreToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Read".to_string()),
            tool_input: Some(serde_json::json!({"file_path": "/src/main.rs"})),
            tool_use_id: Some("toolu_1".to_string()),
            tool_response: None,
            message: None,
        };

        handle_pre_tool_use(&state, &payload).await;

        // No message should be sent since there's no loop mapping
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn handle_pre_tool_use_with_mapping_sends_tool_call() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        // Set up a mapping
        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-123".to_string(), loop_id, session_id)
            .await;

        let payload = HookPayload {
            session_id: "cc-123".to_string(),
            hook_event_name: "PreToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Bash".to_string()),
            tool_input: Some(serde_json::json!({"command": "ls"})),
            tool_use_id: Some("toolu_abc".to_string()),
            tool_response: None,
            message: None,
        };

        handle_pre_tool_use(&state, &payload).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopToolCall {
                loop_id: lid,
                tool_name,
                status,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert_eq!(tool_name, "Bash");
                assert_eq!(status, myremote_protocol::ToolCallStatus::Pending);
            }
            _ => panic!("expected LoopToolCall"),
        }

        // Should have tracked the start time
        let starts = state.tool_call_starts.read().await;
        assert!(starts.contains_key("toolu_abc"));
    }

    #[tokio::test]
    async fn handle_pre_tool_use_missing_tool_name_defaults_to_unknown() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-noname".to_string(), loop_id, session_id)
            .await;

        let payload = HookPayload {
            session_id: "cc-noname".to_string(),
            hook_event_name: "PreToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
        };

        handle_pre_tool_use(&state, &payload).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopToolCall { tool_name, .. } => {
                assert_eq!(tool_name, "unknown");
            }
            _ => panic!("expected LoopToolCall"),
        }
    }

    #[tokio::test]
    async fn handle_post_tool_use_no_mapping_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let payload = HookPayload {
            session_id: "unknown-session".to_string(),
            hook_event_name: "PostToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Read".to_string()),
            tool_input: None,
            tool_use_id: Some("toolu_1".to_string()),
            tool_response: Some(serde_json::json!("file contents")),
            message: None,
        };

        handle_post_tool_use(&state, &payload).await;
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn handle_post_tool_use_with_mapping_sends_tool_result() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-post".to_string(), loop_id, session_id)
            .await;

        // Insert a start time to test duration calculation
        state
            .tool_call_starts
            .write()
            .await
            .insert("toolu_post".to_string(), Instant::now());

        let payload = HookPayload {
            session_id: "cc-post".to_string(),
            hook_event_name: "PostToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Read".to_string()),
            tool_input: None,
            tool_use_id: Some("toolu_post".to_string()),
            tool_response: Some(serde_json::json!("result data")),
            message: None,
        };

        handle_post_tool_use(&state, &payload).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopToolResult {
                loop_id: lid,
                result_preview,
                duration_ms,
                ..
            } => {
                assert_eq!(lid, loop_id);
                assert!(result_preview.contains("result data"));
                // Duration should be >= 0 (we just inserted the start time)
                assert!(duration_ms < 1000);
            }
            _ => panic!("expected LoopToolResult"),
        }

        // Start time should have been removed
        let starts = state.tool_call_starts.read().await;
        assert!(!starts.contains_key("toolu_post"));
    }

    #[tokio::test]
    async fn handle_post_tool_use_truncates_long_response() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-long".to_string(), loop_id, session_id)
            .await;

        // Create a response longer than 500 chars
        let long_response = "x".repeat(1000);
        let payload = HookPayload {
            session_id: "cc-long".to_string(),
            hook_event_name: "PostToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: Some("toolu_long".to_string()),
            tool_response: Some(serde_json::Value::String(long_response)),
            message: None,
        };

        handle_post_tool_use(&state, &payload).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopToolResult { result_preview, .. } => {
                assert!(result_preview.ends_with("..."));
                // The serialized JSON string is longer than 500, so it gets truncated
                assert!(result_preview.len() <= 510);
            }
            _ => panic!("expected LoopToolResult"),
        }
    }

    #[tokio::test]
    async fn handle_stop_no_mapping_is_noop() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let payload = HookPayload {
            session_id: "unknown".to_string(),
            hook_event_name: "Stop".to_string(),
            transcript_path: Some("/tmp/test.jsonl".to_string()),
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
        };

        handle_stop(&state, &payload).await;
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn handle_stop_with_transcript_file() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-stop".to_string(), loop_id, session_id)
            .await;

        // Create a temp transcript file
        let dir = tempfile::tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        let content = r#"{"role":"user","content":"Hello"}
{"role":"assistant","content":"Hi there","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"model":"claude-sonnet-4-20250514"}
"#;
        tokio::fs::write(&transcript_path, content).await.unwrap();

        let payload = HookPayload {
            session_id: "cc-stop".to_string(),
            hook_event_name: "Stop".to_string(),
            transcript_path: Some(transcript_path.to_str().unwrap().to_string()),
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
        };

        handle_stop(&state, &payload).await;

        // Should receive transcript entries and metrics
        let mut got_transcript = false;
        let mut got_metrics = false;
        while let Ok(msg) = agentic_rx.try_recv() {
            match msg {
                AgenticAgentMessage::LoopTranscript { .. } => got_transcript = true,
                AgenticAgentMessage::LoopMetrics { .. } => got_metrics = true,
                _ => {}
            }
        }
        assert!(got_transcript);
        assert!(got_metrics);
    }

    #[tokio::test]
    async fn handle_stop_without_transcript_path() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-stop-nopath".to_string(), loop_id, session_id)
            .await;

        let payload = HookPayload {
            session_id: "cc-stop-nopath".to_string(),
            hook_event_name: "Stop".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
        };

        handle_stop(&state, &payload).await;
        // No transcript path means no messages
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn try_capture_cc_session_id_no_claude_task() {
        let (state, _agentic_rx, mut outbound_rx) = make_test_hooks_state();
        let session_id = uuid::Uuid::new_v4();

        // No claude task registered, should not send anything
        try_capture_cc_session_id(&state, "cc-123", &session_id).await;
        assert!(outbound_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn try_capture_cc_session_id_sends_once() {
        let (state, _agentic_rx, mut outbound_rx) = make_test_hooks_state();
        let session_id = uuid::Uuid::new_v4();
        let claude_task_id = uuid::Uuid::new_v4();

        // Register a claude task for this session
        state
            .mapper
            .register_claude_task(session_id, claude_task_id)
            .await;

        // First call should send
        try_capture_cc_session_id(&state, "cc-capture", &session_id).await;
        assert!(outbound_rx.try_recv().is_ok());

        // Second call should NOT send (dedup)
        try_capture_cc_session_id(&state, "cc-capture", &session_id).await;
        assert!(outbound_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn handle_hook_updates_transcript_path() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state
            .mapper
            .register_cc_session("cc-path".to_string(), loop_id, session_id)
            .await;

        let payload = HookPayload {
            session_id: "cc-path".to_string(),
            hook_event_name: "SubagentStart".to_string(),
            transcript_path: Some("/new/path.jsonl".to_string()),
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
        };

        // Manually run the transcript path update logic
        if let Some(ref path) = payload.transcript_path {
            state
                .mapper
                .set_transcript_path(&payload.session_id, path.clone())
                .await;
        }

        let mapped = state.mapper.lookup_by_cc_session("cc-path").await.unwrap();
        assert_eq!(mapped.transcript_path.as_deref(), Some("/new/path.jsonl"));
    }

    #[tokio::test]
    async fn handle_hook_dispatches_pre_tool_use() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-dispatch".to_string(), loop_id, session_id)
            .await;

        let payload = HookPayload {
            session_id: "cc-dispatch".to_string(),
            hook_event_name: "PreToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Grep".to_string()),
            tool_input: Some(serde_json::json!({"pattern": "test"})),
            tool_use_id: Some("toolu_dispatch".to_string()),
            tool_response: None,
            message: None,
        };

        let resp = handle_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let msg = agentic_rx.try_recv().unwrap();
        assert!(matches!(msg, AgenticAgentMessage::LoopToolCall { .. }));
    }

    #[tokio::test]
    async fn handle_hook_dispatches_post_tool_use() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-post-dispatch".to_string(), loop_id, session_id)
            .await;

        let payload = HookPayload {
            session_id: "cc-post-dispatch".to_string(),
            hook_event_name: "PostToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Read".to_string()),
            tool_input: None,
            tool_use_id: Some("toolu_post_dispatch".to_string()),
            tool_response: Some(serde_json::json!("content")),
            message: None,
        };

        let resp = handle_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let msg = agentic_rx.try_recv().unwrap();
        assert!(matches!(msg, AgenticAgentMessage::LoopToolResult { .. }));
    }

    #[tokio::test]
    async fn handle_hook_dispatches_stop() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        let payload = HookPayload {
            session_id: "cc-stop-dispatch".to_string(),
            hook_event_name: "Stop".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
        };

        let resp = handle_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn handle_hook_dispatches_notification() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        let payload = HookPayload {
            session_id: "cc-notif".to_string(),
            hook_event_name: "Notification".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: Some("Build finished".to_string()),
        };

        let resp = handle_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn handle_hook_dispatches_subagent_events() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        for event_name in &["SubagentStart", "SubagentStop"] {
            let payload = HookPayload {
                session_id: "cc-sub".to_string(),
                hook_event_name: (*event_name).to_string(),
                transcript_path: None,
                cwd: None,
                tool_name: None,
                tool_input: None,
                tool_use_id: None,
                tool_response: None,
                message: None,
            };

            let resp = handle_hook(State(state.clone()), Json(payload))
                .await
                .into_response();
            assert_eq!(resp.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn handle_hook_dispatches_unknown_event() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        let payload = HookPayload {
            session_id: "cc-unknown".to_string(),
            hook_event_name: "FutureHookEvent".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
        };

        let resp = handle_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn handle_hook_permission_request_no_mapping() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        let payload = HookPayload {
            session_id: "cc-perm-no-map".to_string(),
            hook_event_name: "PermissionRequest".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Bash".to_string()),
            tool_input: Some(serde_json::json!({"command": "rm -rf /"})),
            tool_use_id: Some("toolu_perm".to_string()),
            tool_response: None,
            message: None,
        };

        let resp = handle_hook(State(state), Json(payload))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn handle_permission_request_with_allow_rule() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-perm-allow".to_string(), loop_id, session_id)
            .await;

        // Set up an auto-approve rule for Read
        state
            .permission_manager
            .update_rules(vec![myremote_protocol::PermissionRule {
                tool_pattern: "Read".to_string(),
                action: myremote_protocol::PermissionAction::AutoApprove,
            }])
            .await;

        let payload = HookPayload {
            session_id: "cc-perm-allow".to_string(),
            hook_event_name: "PermissionRequest".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Read".to_string()),
            tool_input: Some(serde_json::json!({"file_path": "/main.rs"})),
            tool_use_id: Some("toolu_allow".to_string()),
            tool_response: None,
            message: None,
        };

        let resp = handle_permission_request(&state, &payload).await;
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["decision"], "allow");
    }

    #[tokio::test]
    async fn handle_permission_request_with_deny_rule() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-perm-deny".to_string(), loop_id, session_id)
            .await;

        state
            .permission_manager
            .update_rules(vec![myremote_protocol::PermissionRule {
                tool_pattern: "Bash*".to_string(),
                action: myremote_protocol::PermissionAction::Deny,
            }])
            .await;

        let payload = HookPayload {
            session_id: "cc-perm-deny".to_string(),
            hook_event_name: "PermissionRequest".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Bash".to_string()),
            tool_input: Some(serde_json::json!({"command": "ls"})),
            tool_use_id: Some("toolu_deny".to_string()),
            tool_response: None,
            message: None,
        };

        let resp = handle_permission_request(&state, &payload).await;
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["decision"], "deny");
    }

    #[tokio::test]
    async fn handle_stop_with_invalid_transcript_file() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-stop-invalid".to_string(), loop_id, session_id)
            .await;

        let payload = HookPayload {
            session_id: "cc-stop-invalid".to_string(),
            hook_event_name: "Stop".to_string(),
            transcript_path: Some("/nonexistent/path/transcript.jsonl".to_string()),
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
        };

        handle_stop(&state, &payload).await;
        // Invalid file should not produce any messages (error is logged)
        assert!(agentic_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn handle_post_tool_use_no_start_time_defaults_zero_duration() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-no-start".to_string(), loop_id, session_id)
            .await;

        // No pre-tool-use tracked, so no start time exists
        let payload = HookPayload {
            session_id: "cc-no-start".to_string(),
            hook_event_name: "PostToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Read".to_string()),
            tool_input: None,
            tool_use_id: Some("toolu_no_pre".to_string()),
            tool_response: Some(serde_json::json!("result")),
            message: None,
        };

        handle_post_tool_use(&state, &payload).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopToolResult { duration_ms, .. } => {
                assert_eq!(duration_ms, 0);
            }
            _ => panic!("expected LoopToolResult"),
        }
    }

    #[tokio::test]
    async fn handle_post_tool_use_no_response_defaults_empty() {
        let (state, mut agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-no-resp".to_string(), loop_id, session_id)
            .await;

        let payload = HookPayload {
            session_id: "cc-no-resp".to_string(),
            hook_event_name: "PostToolUse".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: Some("Read".to_string()),
            tool_input: None,
            tool_use_id: Some("toolu_no_resp".to_string()),
            tool_response: None,
            message: None,
        };

        handle_post_tool_use(&state, &payload).await;

        let msg = agentic_rx.try_recv().unwrap();
        match msg {
            AgenticAgentMessage::LoopToolResult { result_preview, .. } => {
                assert!(result_preview.is_empty());
            }
            _ => panic!("expected LoopToolResult"),
        }
    }

    #[tokio::test]
    async fn handle_hook_sets_transcript_path_before_dispatch() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-path-before".to_string(), loop_id, session_id)
            .await;

        let payload = HookPayload {
            session_id: "cc-path-before".to_string(),
            hook_event_name: "PreToolUse".to_string(),
            transcript_path: Some("/updated/transcript.jsonl".to_string()),
            cwd: None,
            tool_name: Some("Read".to_string()),
            tool_input: None,
            tool_use_id: Some("toolu_path".to_string()),
            tool_response: None,
            message: None,
        };

        handle_hook(State(state.clone()), Json(payload))
            .await
            .into_response();

        let mapped = state
            .mapper
            .lookup_by_cc_session("cc-path-before")
            .await
            .unwrap();
        assert_eq!(
            mapped.transcript_path.as_deref(),
            Some("/updated/transcript.jsonl")
        );
    }

    #[tokio::test]
    async fn handle_permission_request_missing_tool_name() {
        let (state, _agentic_rx, _outbound_rx) = make_test_hooks_state();

        let session_id = uuid::Uuid::new_v4();
        let loop_id = uuid::Uuid::new_v4();
        state.mapper.register_loop(session_id, loop_id).await;
        state
            .mapper
            .register_cc_session("cc-perm-noname".to_string(), loop_id, session_id)
            .await;

        // Auto-approve wildcard to test with missing tool name defaulting to "unknown"
        state
            .permission_manager
            .update_rules(vec![myremote_protocol::PermissionRule {
                tool_pattern: "*".to_string(),
                action: myremote_protocol::PermissionAction::AutoApprove,
            }])
            .await;

        let payload = HookPayload {
            session_id: "cc-perm-noname".to_string(),
            hook_event_name: "PermissionRequest".to_string(),
            transcript_path: None,
            cwd: None,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            message: None,
        };

        let resp = handle_permission_request(&state, &payload).await;
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Wildcard matches "unknown" default
        assert_eq!(json["decision"], "allow");
    }
}
