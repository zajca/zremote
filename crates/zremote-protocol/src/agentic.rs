use serde::{Deserialize, Serialize};

use crate::{AgenticLoopId, SessionId};

/// Status of an agentic loop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgenticStatus {
    Working,
    WaitingForInput,
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
    },
    LoopEnded {
        loop_id: AgenticLoopId,
        reason: String,
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
        });
        roundtrip_agent(&AgenticAgentMessage::LoopStateUpdate {
            loop_id: Uuid::new_v4(),
            status: AgenticStatus::WaitingForInput,
            task_name: None,
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
    fn unknown_status_deserialization() {
        let json = r#""some_future_status""#;
        let status: AgenticStatus = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(status, AgenticStatus::Unknown);
    }
}
