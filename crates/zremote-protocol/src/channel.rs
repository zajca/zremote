use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::SessionId;

/// Messages pushed into a CC session via Channel Bridge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ChannelMessage {
    /// Commander/user sends instructions to a running worker.
    Instruction {
        from: String,
        content: String,
        #[serde(default)]
        priority: Priority,
    },
    /// Context update (memories, file changes, cross-worker output).
    ContextUpdate {
        kind: ContextUpdateKind,
        content: String,
        #[serde(default)]
        estimated_tokens: usize,
    },
    /// Orchestration signal (continue, abort, pause, switch task).
    Signal {
        action: SignalAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Responses from a CC worker back to `ZRemote` (via tool calls).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ChannelResponse {
    /// Reply from worker (via `zremote_reply` tool).
    Reply {
        message: String,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        metadata: HashMap<String, String>,
    },
    /// Status report (via `zremote_report_status` tool).
    StatusReport {
        status: WorkerStatus,
        summary: String,
    },
    /// Context request (via `zremote_request_context` tool).
    ContextRequest {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
}

/// Message priority for channel instructions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    #[default]
    Normal,
    High,
    Urgent,
}

/// Kind of context update pushed via channel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextUpdateKind {
    Memory,
    FileChanged,
    WorkerOutput,
    ConventionUpdate,
}

/// Orchestration signal action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignalAction {
    Continue,
    Abort,
    Pause,
    SwitchTask,
}

/// Worker status reported via `zremote_report_status` tool.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Progress,
    Blocked,
    Completed,
    Error,
}

/// Channel messages sent from server to agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum ChannelServerAction {
    /// Send a message to a CC worker via channel.
    ChannelSend {
        session_id: SessionId,
        message: ChannelMessage,
    },
    /// Respond to a permission request from a CC worker.
    PermissionResponse {
        session_id: SessionId,
        request_id: String,
        allowed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Channel messages sent from agent to server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum ChannelAgentAction {
    /// Worker sent a reply/status/context-request via channel tools.
    WorkerResponse {
        session_id: SessionId,
        response: ChannelResponse,
    },
    /// Worker hit a permission prompt and needs approval.
    PermissionRequest {
        session_id: SessionId,
        request_id: String,
        tool_name: String,
        tool_input: serde_json::Value,
    },
    /// Channel server availability changed for a session.
    ChannelStatus {
        session_id: SessionId,
        available: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn roundtrip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(msg: &T) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    // -- ChannelMessage --

    #[test]
    fn instruction_roundtrip() {
        roundtrip(&ChannelMessage::Instruction {
            from: "commander".to_string(),
            content: "Fix the failing tests".to_string(),
            priority: Priority::High,
        });
    }

    #[test]
    fn instruction_default_priority() {
        let json = r#"{"type":"Instruction","from":"cmd","content":"test"}"#;
        let msg: ChannelMessage = serde_json::from_str(json).unwrap();
        if let ChannelMessage::Instruction { priority, .. } = msg {
            assert_eq!(priority, Priority::Normal);
        } else {
            panic!("expected Instruction");
        }
    }

    #[test]
    fn context_update_roundtrip() {
        roundtrip(&ChannelMessage::ContextUpdate {
            kind: ContextUpdateKind::Memory,
            content: "New memory: prefer async".to_string(),
            estimated_tokens: 50,
        });
    }

    #[test]
    fn signal_roundtrip() {
        roundtrip(&ChannelMessage::Signal {
            action: SignalAction::Abort,
            reason: Some("Tests failing after 3 retries".to_string()),
        });
    }

    #[test]
    fn signal_without_reason_roundtrip() {
        roundtrip(&ChannelMessage::Signal {
            action: SignalAction::Continue,
            reason: None,
        });
    }

    // -- ChannelResponse --

    #[test]
    fn reply_roundtrip() {
        roundtrip(&ChannelResponse::Reply {
            message: "Tests fixed, 3 failures resolved".to_string(),
            metadata: HashMap::from([("duration".to_string(), "45s".to_string())]),
        });
    }

    #[test]
    fn reply_empty_metadata_roundtrip() {
        roundtrip(&ChannelResponse::Reply {
            message: "Done".to_string(),
            metadata: HashMap::new(),
        });
    }

    #[test]
    fn status_report_roundtrip() {
        roundtrip(&ChannelResponse::StatusReport {
            status: WorkerStatus::Blocked,
            summary: "Need database migration approved".to_string(),
        });
    }

    #[test]
    fn context_request_roundtrip() {
        roundtrip(&ChannelResponse::ContextRequest {
            kind: "memories".to_string(),
            target: None,
        });
    }

    #[test]
    fn context_request_with_target_roundtrip() {
        roundtrip(&ChannelResponse::ContextRequest {
            kind: "file".to_string(),
            target: Some("src/main.rs".to_string()),
        });
    }

    // -- ChannelServerAction --

    #[test]
    fn channel_send_roundtrip() {
        roundtrip(&ChannelServerAction::ChannelSend {
            session_id: Uuid::new_v4(),
            message: ChannelMessage::Instruction {
                from: "commander".to_string(),
                content: "Deploy to staging".to_string(),
                priority: Priority::Normal,
            },
        });
    }

    #[test]
    fn permission_response_roundtrip() {
        roundtrip(&ChannelServerAction::PermissionResponse {
            session_id: Uuid::new_v4(),
            request_id: "perm-abc-123".to_string(),
            allowed: true,
            reason: None,
        });
    }

    #[test]
    fn permission_response_with_reason_roundtrip() {
        roundtrip(&ChannelServerAction::PermissionResponse {
            session_id: Uuid::new_v4(),
            request_id: "perm-xyz".to_string(),
            allowed: false,
            reason: Some("Destructive operation not allowed".to_string()),
        });
    }

    // -- ChannelAgentAction --

    #[test]
    fn worker_response_roundtrip() {
        roundtrip(&ChannelAgentAction::WorkerResponse {
            session_id: Uuid::new_v4(),
            response: ChannelResponse::StatusReport {
                status: WorkerStatus::Completed,
                summary: "All tests pass".to_string(),
            },
        });
    }

    #[test]
    fn permission_request_roundtrip() {
        roundtrip(&ChannelAgentAction::PermissionRequest {
            session_id: Uuid::new_v4(),
            request_id: "req-001".to_string(),
            tool_name: "Bash".to_string(),
            tool_input: serde_json::json!({"command": "cargo test"}),
        });
    }

    #[test]
    fn channel_status_roundtrip() {
        roundtrip(&ChannelAgentAction::ChannelStatus {
            session_id: Uuid::new_v4(),
            available: true,
        });
    }

    // -- Enum variant coverage --

    #[test]
    fn all_priority_variants() {
        for p in [Priority::Normal, Priority::High, Priority::Urgent] {
            roundtrip(&p);
        }
    }

    #[test]
    fn all_context_update_kinds() {
        for k in [
            ContextUpdateKind::Memory,
            ContextUpdateKind::FileChanged,
            ContextUpdateKind::WorkerOutput,
            ContextUpdateKind::ConventionUpdate,
        ] {
            roundtrip(&k);
        }
    }

    #[test]
    fn all_signal_actions() {
        for a in [
            SignalAction::Continue,
            SignalAction::Abort,
            SignalAction::Pause,
            SignalAction::SwitchTask,
        ] {
            roundtrip(&a);
        }
    }

    #[test]
    fn all_worker_statuses() {
        for s in [
            WorkerStatus::Progress,
            WorkerStatus::Blocked,
            WorkerStatus::Completed,
            WorkerStatus::Error,
        ] {
            roundtrip(&s);
        }
    }
}
