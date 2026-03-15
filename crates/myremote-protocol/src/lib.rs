use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type HostId = Uuid;
pub type SessionId = Uuid;

/// Messages sent from agent to server.
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
}

/// Messages sent from server to agent.
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
