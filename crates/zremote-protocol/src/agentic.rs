use serde::{Deserialize, Serialize};

use crate::{AgenticLoopId, SessionId};

/// Status of an agentic loop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgenticStatus {
    Working,
    WaitingForInput,
    RequiresAction,
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
    ExecutionNode {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_id: Option<AgenticLoopId>,
        timestamp: i64,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        working_dir: String,
        duration_ms: i64,
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
    fn execution_node_roundtrip() {
        roundtrip_agent(&AgenticAgentMessage::ExecutionNode {
            session_id: Uuid::new_v4(),
            loop_id: Some(Uuid::new_v4()),
            timestamp: 1_711_843_200_000,
            kind: "tool_call".to_string(),
            input: Some("Read src/main.rs".to_string()),
            output_summary: Some("fn main() {}".to_string()),
            exit_code: None,
            working_dir: "/home/user/project".to_string(),
            duration_ms: 1234,
        });
        roundtrip_agent(&AgenticAgentMessage::ExecutionNode {
            session_id: Uuid::new_v4(),
            loop_id: None,
            timestamp: 1_711_843_200_000,
            kind: "shell_command".to_string(),
            input: None,
            output_summary: None,
            exit_code: Some(0),
            working_dir: "/home/user".to_string(),
            duration_ms: 50,
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
}
