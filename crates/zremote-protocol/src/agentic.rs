use serde::{Deserialize, Serialize};

use crate::{AgenticLoopId, NodeStatus, SessionId};

/// Status of an agentic loop.
///
/// `Idle` is a non-notifying heuristic fallback emitted by the output analyzer
/// when the PTY has been silent for a short window but no explicit signal
/// (e.g. a Claude Code `Notification` or `Elicitation` hook) has arrived. Only
/// hook-driven `WaitingForInput` / `RequiresAction` statuses are authoritative
/// for user-facing notifications (Telegram, toasts).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgenticStatus {
    Working,
    WaitingForInput,
    RequiresAction,
    Idle,
    Error,
    Completed,
    #[serde(other)]
    Unknown,
}

/// Agentic messages sent from agent to server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum AgenticAgentMessage {
    LoopDetected {
        loop_id: AgenticLoopId,
        session_id: SessionId,
        project_path: String,
        tool_name: String,
    },
    LoopStateUpdate {
        loop_id: AgenticLoopId,
        status: AgenticStatus,
        task_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_description: Option<String>,
    },
    LoopEnded {
        loop_id: AgenticLoopId,
        reason: String,
    },
    LoopMetricsUpdate {
        loop_id: AgenticLoopId,
        input_tokens: u64,
        output_tokens: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },
    /// `PreToolUse` hook, or PTY analyzer at tool start.
    ExecutionNodeOpened {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_id: Option<AgenticLoopId>,
        tool_use_id: String,
        timestamp: i64,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        working_dir: String,
    },

    /// `PostToolUse` hook, or PTY analyzer at tool finish.
    ExecutionNodeClosed {
        session_id: SessionId,
        tool_use_id: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        duration_ms: i64,
        status: NodeStatus,
    },

    /// Stop / `StopFailure` hook: close every running node for this session.
    SessionExecutionStopped { session_id: SessionId },
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn roundtrip_agent(msg: &AgenticAgentMessage) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: AgenticAgentMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    #[test]
    fn loop_detected_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopDetected {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            project_path: "/home/user/project".to_string(),
            tool_name: "claude-code".to_string(),
        });
    }

    #[test]
    fn loop_state_update_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopStateUpdate {
            loop_id: Uuid::new_v4(),
            status: AgenticStatus::Working,
            task_name: Some("fix-tests".to_string()),
            prompt_message: None,
            permission_mode: None,
            action_tool_name: None,
            action_description: None,
        });
        roundtrip_agent(&AgenticAgentMessage::LoopStateUpdate {
            loop_id: Uuid::new_v4(),
            status: AgenticStatus::WaitingForInput,
            task_name: None,
            prompt_message: None,
            permission_mode: None,
            action_tool_name: None,
            action_description: None,
        });
    }

    #[test]
    fn loop_state_update_with_prompt_message_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopStateUpdate {
            loop_id: Uuid::new_v4(),
            status: AgenticStatus::WaitingForInput,
            task_name: None,
            prompt_message: Some("Allow Read tool?".into()),
            permission_mode: None,
            action_tool_name: None,
            action_description: None,
        });
    }

    #[test]
    fn loop_ended_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopEnded {
            loop_id: Uuid::new_v4(),
            reason: "completed".to_string(),
        });
        roundtrip_agent(&AgenticAgentMessage::LoopEnded {
            loop_id: Uuid::new_v4(),
            reason: "error".to_string(),
        });
    }

    #[test]
    fn loop_metrics_update_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopMetricsUpdate {
            loop_id: Uuid::new_v4(),
            input_tokens: 5000,
            output_tokens: 1200,
            cost_usd: Some(0.15),
        });
        roundtrip_agent(&AgenticAgentMessage::LoopMetricsUpdate {
            loop_id: Uuid::new_v4(),
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: None,
        });
    }

    #[test]
    fn execution_node_opened_closed_stopped_roundtrip() {
        use crate::NodeStatus;
        roundtrip_agent(&AgenticAgentMessage::ExecutionNodeOpened {
            session_id: Uuid::new_v4(),
            loop_id: Some(Uuid::new_v4()),
            tool_use_id: "toolu_abc123".to_string(),
            timestamp: 1_711_843_200_000,
            kind: "read".to_string(),
            input: Some("Read src/main.rs".to_string()),
            working_dir: "/home/user/project".to_string(),
        });
        roundtrip_agent(&AgenticAgentMessage::ExecutionNodeOpened {
            session_id: Uuid::new_v4(),
            loop_id: None,
            tool_use_id: "pty-00000000-0000-0000-0000-000000000001".to_string(),
            timestamp: 1_711_843_200_000,
            kind: "bash".to_string(),
            input: None,
            working_dir: "/home/user".to_string(),
        });
        roundtrip_agent(&AgenticAgentMessage::ExecutionNodeClosed {
            session_id: Uuid::new_v4(),
            tool_use_id: "toolu_abc123".to_string(),
            kind: "read".to_string(),
            output_summary: Some("fn main() {}".to_string()),
            exit_code: None,
            duration_ms: 1234,
            status: NodeStatus::Completed,
        });
        roundtrip_agent(&AgenticAgentMessage::ExecutionNodeClosed {
            session_id: Uuid::new_v4(),
            tool_use_id: "toolu_xyz".to_string(),
            kind: "bash".to_string(),
            output_summary: None,
            exit_code: Some(0),
            duration_ms: 50,
            status: NodeStatus::Stopped,
        });
        roundtrip_agent(&AgenticAgentMessage::SessionExecutionStopped {
            session_id: Uuid::new_v4(),
        });
    }

    #[test]
    fn agentic_status_serialization() {
        assert_eq!(
            serde_json::to_string(&AgenticStatus::WaitingForInput).unwrap(),
            r#""waiting_for_input""#
        );
        assert_eq!(
            serde_json::to_string(&AgenticStatus::Working).unwrap(),
            r#""working""#
        );
    }

    #[test]
    fn requires_action_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopStateUpdate {
            loop_id: Uuid::new_v4(),
            status: AgenticStatus::RequiresAction,
            task_name: Some("deploy".to_string()),
            prompt_message: None,
            permission_mode: None,
            action_tool_name: Some("Bash".to_string()),
            action_description: Some("Run deploy script".to_string()),
        });
    }

    #[test]
    fn loop_state_update_with_action_fields_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopStateUpdate {
            loop_id: Uuid::new_v4(),
            status: AgenticStatus::WaitingForInput,
            task_name: None,
            prompt_message: Some("Allow Bash?".into()),
            permission_mode: Some("plan".into()),
            action_tool_name: Some("Bash".to_string()),
            action_description: Some("rm -rf /tmp/build".to_string()),
        });
    }

    #[test]
    fn loop_state_update_backward_compat_missing_action_fields() {
        let json = r#"{"type":"LoopStateUpdate","payload":{"loop_id":"00000000-0000-0000-0000-000000000001","status":"working","task_name":null,"prompt_message":null,"permission_mode":null}}"#;
        let parsed: AgenticAgentMessage = serde_json::from_str(json).expect("deserialize");
        match parsed {
            AgenticAgentMessage::LoopStateUpdate {
                action_tool_name,
                action_description,
                ..
            } => {
                assert!(action_tool_name.is_none());
                assert!(action_description.is_none());
            }
            other => panic!("expected LoopStateUpdate, got {other:?}"),
        }
    }

    #[test]
    fn requires_action_serialization() {
        assert_eq!(
            serde_json::to_string(&AgenticStatus::RequiresAction).unwrap(),
            r#""requires_action""#
        );
    }

    #[test]
    fn unknown_status_deserialization() {
        let json = r#""some_future_status""#;
        let status: AgenticStatus = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(status, AgenticStatus::Unknown);
    }

    #[test]
    fn idle_status_serialization() {
        assert_eq!(
            serde_json::to_string(&AgenticStatus::Idle).unwrap(),
            r#""idle""#
        );
    }

    #[test]
    fn idle_status_roundtrip() {
        let status: AgenticStatus = serde_json::from_str(r#""idle""#).expect("should deserialize");
        assert_eq!(status, AgenticStatus::Idle);
    }
}
