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
        return;
    };

    try_capture_cc_session_id(state, &payload.session_id, &mapped.session_id).await;

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
    if let Ok(home) = std::env::var("HOME") {
        let allowed_prefix = format!("{home}/.claude/projects/");
        if !transcript_path.starts_with(&allowed_prefix) {
            tracing::warn!(
                path = %transcript_path,
                "transcript_path outside allowed directory, ignoring"
            );
            return None;
        }
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
    }
}
