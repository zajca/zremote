use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AgenticLoopId, SessionId};

/// Status of an agentic loop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgenticStatus {
    Working,
    WaitingForInput,
    Paused,
    Error,
    Completed,
}

/// Status of a tool call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    Approved,
    Rejected,
    Running,
    Completed,
    Failed,
}

/// User action on an agentic loop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserAction {
    Approve,
    Reject,
    ProvideInput,
    Pause,
    Resume,
    Stop,
}

/// Role in a transcript entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    Assistant,
    User,
    Tool,
    System,
}

/// Permission action for tool calls.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionAction {
    AutoApprove,
    Ask,
    Deny,
}

/// A permission rule for tool calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionRule {
    pub tool_pattern: String,
    pub action: PermissionAction,
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
        model: String,
    },
    LoopStateUpdate {
        loop_id: AgenticLoopId,
        status: AgenticStatus,
        current_step: Option<String>,
        context_usage_pct: f32,
        total_tokens: u64,
        estimated_cost_usd: f64,
        pending_tool_calls: u32,
    },
    LoopToolCall {
        loop_id: AgenticLoopId,
        tool_call_id: Uuid,
        tool_name: String,
        arguments_json: String,
        status: ToolCallStatus,
    },
    LoopToolResult {
        loop_id: AgenticLoopId,
        tool_call_id: Uuid,
        result_preview: String,
        duration_ms: u64,
    },
    LoopTranscript {
        loop_id: AgenticLoopId,
        role: TranscriptRole,
        content: String,
        tool_call_id: Option<Uuid>,
        timestamp: DateTime<Utc>,
    },
    LoopMetrics {
        loop_id: AgenticLoopId,
        tokens_in: u64,
        tokens_out: u64,
        model: String,
        context_used: u64,
        context_max: u64,
        estimated_cost_usd: f64,
        task_name: Option<String>,
    },
    LoopEnded {
        loop_id: AgenticLoopId,
        reason: String,
        summary: Option<String>,
    },
}

/// Agentic messages sent from server to agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum AgenticServerMessage {
    UserAction {
        loop_id: AgenticLoopId,
        action: UserAction,
        payload: Option<String>,
    },
    PermissionRulesUpdate {
        rules: Vec<PermissionRule>,
    },
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

    fn roundtrip_server(msg: &AgenticServerMessage) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: AgenticServerMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    #[test]
    fn loop_detected_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopDetected {
            loop_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            project_path: "/home/user/project".to_string(),
            tool_name: "claude-code".to_string(),
            model: "claude-sonnet-4".to_string(),
        });
    }

    #[test]
    fn loop_state_update_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopStateUpdate {
            loop_id: Uuid::new_v4(),
            status: AgenticStatus::Working,
            current_step: Some("Reading files".to_string()),
            context_usage_pct: 45.2,
            total_tokens: 12345,
            estimated_cost_usd: 0.42,
            pending_tool_calls: 1,
        });
    }

    #[test]
    fn loop_tool_call_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopToolCall {
            loop_id: Uuid::new_v4(),
            tool_call_id: Uuid::new_v4(),
            tool_name: "Read".to_string(),
            arguments_json: r#"{"path":"/src/main.rs"}"#.to_string(),
            status: ToolCallStatus::Pending,
        });
    }

    #[test]
    fn loop_tool_result_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopToolResult {
            loop_id: Uuid::new_v4(),
            tool_call_id: Uuid::new_v4(),
            result_preview: "File contents: fn main()...".to_string(),
            duration_ms: 150,
        });
    }

    #[test]
    fn loop_transcript_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopTranscript {
            loop_id: Uuid::new_v4(),
            role: TranscriptRole::Assistant,
            content: "I'll help you refactor this code.".to_string(),
            tool_call_id: None,
            timestamp: Utc::now(),
        });
        roundtrip_agent(&AgenticAgentMessage::LoopTranscript {
            loop_id: Uuid::new_v4(),
            role: TranscriptRole::Tool,
            content: "file contents here".to_string(),
            tool_call_id: Some(Uuid::new_v4()),
            timestamp: Utc::now(),
        });
    }

    #[test]
    fn loop_metrics_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopMetrics {
            loop_id: Uuid::new_v4(),
            tokens_in: 45231,
            tokens_out: 3100,
            model: "claude-sonnet-4".to_string(),
            context_used: 45231,
            context_max: 100000,
            estimated_cost_usd: 0.42,
            task_name: Some("test-task-name".to_string()),
        });
        roundtrip_agent(&AgenticAgentMessage::LoopMetrics {
            loop_id: Uuid::new_v4(),
            tokens_in: 45231,
            tokens_out: 3100,
            model: "claude-sonnet-4".to_string(),
            context_used: 45231,
            context_max: 100000,
            estimated_cost_usd: 0.42,
            task_name: None,
        });
    }

    #[test]
    fn loop_ended_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::LoopEnded {
            loop_id: Uuid::new_v4(),
            reason: "completed".to_string(),
            summary: Some("Refactored 3 files".to_string()),
        });
        roundtrip_agent(&AgenticAgentMessage::LoopEnded {
            loop_id: Uuid::new_v4(),
            reason: "error".to_string(),
            summary: None,
        });
    }

    #[test]
    fn user_action_roundtrip() {
        roundtrip_server(&AgenticServerMessage::UserAction {
            loop_id: Uuid::new_v4(),
            action: UserAction::Approve,
            payload: None,
        });
        roundtrip_server(&AgenticServerMessage::UserAction {
            loop_id: Uuid::new_v4(),
            action: UserAction::ProvideInput,
            payload: Some("yes, proceed".to_string()),
        });
    }

    #[test]
    fn permission_rules_update_roundtrip() {
        roundtrip_server(&AgenticServerMessage::PermissionRulesUpdate {
            rules: vec![
                PermissionRule {
                    tool_pattern: "Read".to_string(),
                    action: PermissionAction::AutoApprove,
                },
                PermissionRule {
                    tool_pattern: "Bash*".to_string(),
                    action: PermissionAction::Ask,
                },
                PermissionRule {
                    tool_pattern: "Write".to_string(),
                    action: PermissionAction::Deny,
                },
            ],
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
    fn tool_call_status_serialization() {
        assert_eq!(
            serde_json::to_string(&ToolCallStatus::Pending).unwrap(),
            r#""pending""#
        );
        assert_eq!(
            serde_json::to_string(&ToolCallStatus::Approved).unwrap(),
            r#""approved""#
        );
    }

    #[test]
    fn user_action_serialization() {
        assert_eq!(
            serde_json::to_string(&UserAction::Approve).unwrap(),
            r#""approve""#
        );
        assert_eq!(
            serde_json::to_string(&UserAction::ProvideInput).unwrap(),
            r#""provide_input""#
        );
    }

    #[test]
    fn permission_action_serialization() {
        assert_eq!(
            serde_json::to_string(&PermissionAction::AutoApprove).unwrap(),
            r#""auto_approve""#
        );
    }
}
