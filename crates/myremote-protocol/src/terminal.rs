use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agentic::AgenticServerMessage;
use crate::project::ProjectInfo;
use crate::{HostId, SessionId};

/// Messages sent from agent to server (terminal/connection layer).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum AgentMessage {
    Register {
        hostname: String,
        agent_version: String,
        os: String,
        arch: String,
        token: String,
    },
    Heartbeat {
        timestamp: DateTime<Utc>,
    },
    TerminalOutput {
        session_id: SessionId,
        data: Vec<u8>,
    },
    SessionCreated {
        session_id: SessionId,
        shell: String,
        pid: u32,
    },
    SessionClosed {
        session_id: SessionId,
        exit_code: Option<i32>,
    },
    Error {
        session_id: Option<SessionId>,
        message: String,
    },
    ProjectDiscovered {
        path: String,
        name: String,
        has_claude_config: bool,
        project_type: String,
    },
    ProjectList {
        projects: Vec<ProjectInfo>,
    },
}

/// Messages sent from server to agent (terminal/connection layer).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum ServerMessage {
    RegisterAck {
        host_id: HostId,
    },
    HeartbeatAck {
        timestamp: DateTime<Utc>,
    },
    SessionCreate {
        session_id: SessionId,
        shell: Option<String>,
        cols: u16,
        rows: u16,
        working_dir: Option<String>,
    },
    SessionClose {
        session_id: SessionId,
    },
    TerminalInput {
        session_id: SessionId,
        data: Vec<u8>,
    },
    TerminalResize {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },
    Error {
        message: String,
    },
    AgenticAction(AgenticServerMessage),
    ProjectScan,
    ProjectRegister {
        path: String,
    },
    ProjectRemove {
        path: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn roundtrip_agent(msg: &AgentMessage) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: AgentMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    fn roundtrip_server(msg: &ServerMessage) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: ServerMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    #[test]
    fn register_roundtrip() {
        roundtrip_agent(&AgentMessage::Register {
            hostname: "dev-machine".to_string(),
            agent_version: "0.1.0".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            token: "secret-token".to_string(),
        });
    }

    #[test]
    fn register_ack_roundtrip() {
        roundtrip_server(&ServerMessage::RegisterAck {
            host_id: Uuid::new_v4(),
        });
    }

    #[test]
    fn heartbeat_roundtrip() {
        roundtrip_agent(&AgentMessage::Heartbeat {
            timestamp: Utc::now(),
        });
        roundtrip_server(&ServerMessage::HeartbeatAck {
            timestamp: Utc::now(),
        });
    }

    #[test]
    fn terminal_output_roundtrip() {
        roundtrip_agent(&AgentMessage::TerminalOutput {
            session_id: Uuid::new_v4(),
            data: vec![0x1b, 0x5b, 0x48],
        });
    }

    #[test]
    fn terminal_input_roundtrip() {
        roundtrip_server(&ServerMessage::TerminalInput {
            session_id: Uuid::new_v4(),
            data: vec![0x68, 0x65, 0x6c, 0x6c, 0x6f],
        });
    }

    #[test]
    fn terminal_resize_roundtrip() {
        roundtrip_server(&ServerMessage::TerminalResize {
            session_id: Uuid::new_v4(),
            cols: 80,
            rows: 24,
        });
    }

    #[test]
    fn session_create_roundtrip() {
        roundtrip_server(&ServerMessage::SessionCreate {
            session_id: Uuid::new_v4(),
            shell: Some("/bin/bash".to_string()),
            cols: 120,
            rows: 40,
            working_dir: Some("/home/user".to_string()),
        });
        roundtrip_server(&ServerMessage::SessionCreate {
            session_id: Uuid::new_v4(),
            shell: None,
            cols: 80,
            rows: 24,
            working_dir: None,
        });
    }

    #[test]
    fn session_created_roundtrip() {
        roundtrip_agent(&AgentMessage::SessionCreated {
            session_id: Uuid::new_v4(),
            shell: "/bin/bash".to_string(),
            pid: 12345,
        });
    }

    #[test]
    fn session_close_roundtrip() {
        roundtrip_server(&ServerMessage::SessionClose {
            session_id: Uuid::new_v4(),
        });
        roundtrip_agent(&AgentMessage::SessionClosed {
            session_id: Uuid::new_v4(),
            exit_code: Some(0),
        });
        roundtrip_agent(&AgentMessage::SessionClosed {
            session_id: Uuid::new_v4(),
            exit_code: None,
        });
    }

    #[test]
    fn error_roundtrip() {
        roundtrip_agent(&AgentMessage::Error {
            session_id: Some(Uuid::new_v4()),
            message: "PTY spawn failed".to_string(),
        });
        roundtrip_agent(&AgentMessage::Error {
            session_id: None,
            message: "general error".to_string(),
        });
        roundtrip_server(&ServerMessage::Error {
            message: "unknown host".to_string(),
        });
    }

    #[test]
    fn project_discovered_roundtrip() {
        roundtrip_agent(&AgentMessage::ProjectDiscovered {
            path: "/home/user/myproject".to_string(),
            name: "myproject".to_string(),
            has_claude_config: true,
            project_type: "rust".to_string(),
        });
    }

    #[test]
    fn project_list_roundtrip() {
        use crate::project::ProjectInfo;
        roundtrip_agent(&AgentMessage::ProjectList {
            projects: vec![
                ProjectInfo {
                    path: "/home/user/project-a".to_string(),
                    name: "project-a".to_string(),
                    has_claude_config: true,
                    project_type: "rust".to_string(),
                },
                ProjectInfo {
                    path: "/home/user/project-b".to_string(),
                    name: "project-b".to_string(),
                    has_claude_config: false,
                    project_type: "node".to_string(),
                },
            ],
        });
    }

    #[test]
    fn project_scan_roundtrip() {
        roundtrip_server(&ServerMessage::ProjectScan);
    }

    #[test]
    fn project_register_roundtrip() {
        roundtrip_server(&ServerMessage::ProjectRegister {
            path: "/home/user/myproject".to_string(),
        });
    }

    #[test]
    fn project_remove_roundtrip() {
        roundtrip_server(&ServerMessage::ProjectRemove {
            path: "/home/user/myproject".to_string(),
        });
    }
}
