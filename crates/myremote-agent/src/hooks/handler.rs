use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
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

    (
        StatusCode::OK,
        Json(HookResponse { decision: None }),
    )
        .into_response()
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

    let tool_use_id = payload
        .tool_use_id
        .as_deref()
        .unwrap_or("unknown");

    let tool_call_id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        tool_use_id.as_bytes(),
    );

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

    let tool_use_id = payload
        .tool_use_id
        .as_deref()
        .unwrap_or("unknown");

    let tool_call_id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        tool_use_id.as_bytes(),
    );

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
        return (
            StatusCode::OK,
            Json(HookResponse { decision: None }),
        )
            .into_response();
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
            let tool_use_id = payload
                .tool_use_id
                .as_deref()
                .unwrap_or("unknown");
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
                    (
                        StatusCode::OK,
                        Json(HookResponse { decision: None }),
                    )
                        .into_response()
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
}
