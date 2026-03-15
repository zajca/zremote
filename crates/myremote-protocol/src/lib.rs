use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type HostId = Uuid;
pub type SessionId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum Message {
    Heartbeat,
    HeartbeatAck,
    TerminalData {
        session_id: SessionId,
        data: Vec<u8>,
    },
    TerminalResize {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },
    SessionCreate {
        session_id: SessionId,
        shell: Option<String>,
    },
    SessionClose {
        session_id: SessionId,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: &Message) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    #[test]
    fn heartbeat_roundtrip() {
        roundtrip(&Message::Heartbeat);
        roundtrip(&Message::HeartbeatAck);
    }

    #[test]
    fn terminal_data_roundtrip() {
        roundtrip(&Message::TerminalData {
            session_id: Uuid::new_v4(),
            data: vec![0x1b, 0x5b, 0x48],
        });
    }

    #[test]
    fn terminal_resize_roundtrip() {
        roundtrip(&Message::TerminalResize {
            session_id: Uuid::new_v4(),
            cols: 80,
            rows: 24,
        });
    }

    #[test]
    fn session_create_roundtrip() {
        roundtrip(&Message::SessionCreate {
            session_id: Uuid::new_v4(),
            shell: Some("/bin/bash".to_string()),
        });
        roundtrip(&Message::SessionCreate {
            session_id: Uuid::new_v4(),
            shell: None,
        });
    }

    #[test]
    fn session_close_roundtrip() {
        roundtrip(&Message::SessionClose {
            session_id: Uuid::new_v4(),
        });
    }

    #[test]
    fn error_roundtrip() {
        roundtrip(&Message::Error {
            message: "something went wrong".to_string(),
        });
    }
}
