use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc};
use tokio::time::Instant;
use zremote_protocol::agentic::AgenticStatus;
use zremote_protocol::status::SessionStatus;
use zremote_protocol::{AgenticLoopId, HostId, SessionId};

pub const MAX_SCROLLBACK_BYTES: usize = 100 * 1024; // 100KB

/// In-memory state for an active terminal session.
pub struct SessionState {
    pub session_id: SessionId,
    pub host_id: HostId,
    pub status: SessionStatus,
    pub browser_senders: Vec<mpsc::Sender<BrowserMessage>>,
    pub scrollback: VecDeque<Vec<u8>>,
    pub scrollback_size: usize,
    /// Last known terminal dimensions (updated on resize, sent with scrollback).
    pub last_cols: u16,
    pub last_rows: u16,
}

impl SessionState {
    pub fn new(session_id: SessionId, host_id: HostId) -> Self {
        Self {
            session_id,
            host_id,
            status: SessionStatus::Creating,
            browser_senders: Vec::new(),
            scrollback: VecDeque::new(),
            scrollback_size: 0,
            last_cols: 0,
            last_rows: 0,
        }
    }

    pub fn append_scrollback(&mut self, data: Vec<u8>) {
        self.scrollback_size += data.len();
        self.scrollback.push_back(data);
        while self.scrollback_size > MAX_SCROLLBACK_BYTES {
            if let Some(old) = self.scrollback.pop_front() {
                self.scrollback_size -= old.len();
            }
        }
    }
}

/// Binary frame type tags for WebSocket terminal output.
pub const BINARY_TAG_OUTPUT: u8 = 0x01;
pub const BINARY_TAG_PANE_OUTPUT: u8 = 0x02;

/// Encode terminal output as a binary WebSocket frame.
///
/// Format:
/// - Main pane (`pane_id` = None): `[0x01] [raw bytes...]`
/// - Specific pane: `[0x02] [pane_id_len: u8] [pane_id UTF-8] [raw bytes...]`
#[must_use]
pub fn encode_binary_output(pane_id: Option<&str>, data: &[u8]) -> Vec<u8> {
    match pane_id {
        None => {
            let mut frame = Vec::with_capacity(1 + data.len());
            frame.push(BINARY_TAG_OUTPUT);
            frame.extend_from_slice(data);
            frame
        }
        Some(pid) => {
            let pid_bytes = pid.as_bytes();
            let pid_len = u8::try_from(pid_bytes.len()).unwrap_or(u8::MAX);
            let mut frame = Vec::with_capacity(2 + usize::from(pid_len) + data.len());
            frame.push(BINARY_TAG_PANE_OUTPUT);
            frame.push(pid_len);
            frame.extend_from_slice(&pid_bytes[..usize::from(pid_len)]);
            frame.extend_from_slice(data);
            frame
        }
    }
}

/// Decode a binary WebSocket frame into (`pane_id`, data).
///
/// Returns `None` if the frame is empty or has an unknown tag.
#[must_use]
pub fn decode_binary_output(frame: &[u8]) -> Option<(Option<String>, &[u8])> {
    let (&tag, rest) = frame.split_first()?;
    match tag {
        BINARY_TAG_OUTPUT => Some((None, rest)),
        BINARY_TAG_PANE_OUTPUT => {
            let (&pid_len, rest) = rest.split_first()?;
            let pid_len = usize::from(pid_len);
            if rest.len() < pid_len {
                return None;
            }
            let pid = std::str::from_utf8(&rest[..pid_len]).ok()?;
            Some((Some(pid.to_owned()), &rest[pid_len..]))
        }
        _ => None,
    }
}

pub mod base64_serde {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(data: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(data))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(serde::de::Error::custom)
    }
}

/// Messages sent from server to browser WebSocket clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BrowserMessage {
    #[serde(rename = "output")]
    Output {
        #[serde(skip_serializing_if = "Option::is_none")]
        pane_id: Option<String>,
        #[serde(with = "base64_serde")]
        data: Vec<u8>,
    },
    #[serde(rename = "session_closed")]
    SessionClosed { exit_code: Option<i32> },
    #[serde(rename = "session_suspended")]
    SessionSuspended,
    #[serde(rename = "session_resumed")]
    SessionResumed,
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "scrollback_start")]
    ScrollbackStart {
        #[serde(default)]
        cols: u16,
        #[serde(default)]
        rows: u16,
    },
    #[serde(rename = "scrollback_end")]
    ScrollbackEnd,
}

/// Thread-safe store for active session state.
pub type SessionStore = Arc<RwLock<HashMap<SessionId, SessionState>>>;

/// In-memory state for an active agentic loop.
#[derive(Debug)]
pub struct AgenticLoopState {
    pub loop_id: AgenticLoopId,
    pub session_id: SessionId,
    pub host_id: HostId,
    pub status: AgenticStatus,
    pub task_name: Option<String>,
    pub permission_mode: Option<String>,
    pub last_updated: Instant,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: Option<f64>,
}

/// Thread-safe store for active agentic loop state.
pub type AgenticLoopStore = Arc<DashMap<AgenticLoopId, AgenticLoopState>>;

pub use zremote_protocol::events::{HostInfo, LoopInfo, ServerEvent, SessionInfo};

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use zremote_protocol::claude::ClaudeTaskStatus;
    use zremote_protocol::status::HostStatus;

    // --- SessionState tests ---

    #[test]
    fn session_state_new_has_empty_scrollback() {
        let state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        assert!(state.scrollback.is_empty());
        assert_eq!(state.scrollback_size, 0);
        assert_eq!(state.status, SessionStatus::Creating);
    }

    #[test]
    fn append_scrollback_within_limit() {
        let mut state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        let data = vec![0x41; 100]; // 100 bytes
        state.append_scrollback(data.clone());
        assert_eq!(state.scrollback.len(), 1);
        assert_eq!(state.scrollback_size, 100);
        assert_eq!(state.scrollback[0], data);
    }

    #[test]
    fn append_scrollback_multiple_chunks() {
        let mut state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        state.append_scrollback(vec![1; 50]);
        state.append_scrollback(vec![2; 75]);
        state.append_scrollback(vec![3; 25]);
        assert_eq!(state.scrollback.len(), 3);
        assert_eq!(state.scrollback_size, 150);
    }

    #[test]
    fn append_scrollback_evicts_old_data_when_exceeding_limit() {
        let mut state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        // MAX_SCROLLBACK_BYTES = 100 * 1024 = 102400
        let chunk_size = 40_000;
        // Add 3 chunks = 120_000 bytes, which exceeds the limit
        state.append_scrollback(vec![1; chunk_size]); // 40k
        state.append_scrollback(vec![2; chunk_size]); // 80k
        state.append_scrollback(vec![3; chunk_size]); // 120k -> evict first -> 80k

        // First chunk should have been evicted
        assert_eq!(state.scrollback.len(), 2);
        assert_eq!(state.scrollback_size, 80_000);
        // Remaining chunks should be the second and third
        assert_eq!(state.scrollback[0][0], 2);
        assert_eq!(state.scrollback[1][0], 3);
    }

    #[test]
    fn append_scrollback_evicts_multiple_old_chunks() {
        let mut state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        // Fill with many small chunks
        for i in 0..10 {
            state.append_scrollback(vec![i; 20_000]); // 20k each
        }
        // Total would be 200k but limit is ~100k, so oldest chunks get evicted
        assert!(state.scrollback_size <= MAX_SCROLLBACK_BYTES);
        // With 20k chunks and 100k limit, we should have at most 5 chunks
        assert!(state.scrollback.len() <= 5);
    }

    // --- BrowserMessage serialization tests ---

    #[test]
    fn browser_message_output_serialization() {
        let msg = BrowserMessage::Output {
            pane_id: None,
            data: vec![72, 101, 108, 108, 111],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "output");
        // base64 encoding of "Hello"
        assert_eq!(json["data"], "SGVsbG8=");
        // pane_id should be omitted when None
        assert!(json.get("pane_id").is_none());
    }

    #[test]
    fn browser_message_output_with_pane_id_serialization() {
        let msg = BrowserMessage::Output {
            pane_id: Some("%5".to_string()),
            data: vec![72, 101, 108, 108, 111],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "output");
        assert_eq!(json["pane_id"], "%5");
        assert_eq!(json["data"], "SGVsbG8=");
    }

    #[test]
    fn browser_message_session_closed_serialization() {
        let msg = BrowserMessage::SessionClosed { exit_code: Some(0) };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert_eq!(json["exit_code"], 0);
    }

    #[test]
    fn browser_message_session_closed_no_exit_code() {
        let msg = BrowserMessage::SessionClosed { exit_code: None };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert!(json["exit_code"].is_null());
    }

    #[test]
    fn browser_message_error_serialization() {
        let msg = BrowserMessage::Error {
            message: "test error".to_string(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["message"], "test error");
    }

    #[test]
    fn browser_message_session_suspended_serialization() {
        let msg = BrowserMessage::SessionSuspended;
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_suspended");
    }

    #[test]
    fn browser_message_session_resumed_serialization() {
        let msg = BrowserMessage::SessionResumed;
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "session_resumed");
    }

    #[test]
    fn browser_message_roundtrip() {
        let messages = vec![
            BrowserMessage::Output {
                pane_id: None,
                data: vec![1, 2, 3],
            },
            BrowserMessage::Output {
                pane_id: Some("%3".to_string()),
                data: vec![4, 5, 6],
            },
            BrowserMessage::SessionClosed {
                exit_code: Some(42),
            },
            BrowserMessage::SessionSuspended,
            BrowserMessage::SessionResumed,
            BrowserMessage::Error {
                message: "fail".to_string(),
            },
            BrowserMessage::ScrollbackStart {
                cols: 120,
                rows: 40,
            },
            BrowserMessage::ScrollbackEnd,
        ];
        for msg in &messages {
            let json = serde_json::to_string(msg).unwrap();
            let parsed: BrowserMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{parsed:?}"), format!("{msg:?}"));
        }
    }

    #[test]
    fn browser_message_output_base64_encoding() {
        let msg = BrowserMessage::Output {
            pane_id: None,
            data: vec![72, 101, 108, 108, 111],
        };
        let json_str = serde_json::to_string(&msg).unwrap();
        // Verify it's a string, not an array of numbers
        assert!(json_str.contains("\"SGVsbG8=\""));
        assert!(!json_str.contains("[72,"));

        // Verify roundtrip
        let parsed: BrowserMessage = serde_json::from_str(&json_str).unwrap();
        match parsed {
            BrowserMessage::Output { data, .. } => assert_eq!(data, vec![72, 101, 108, 108, 111]),
            _ => panic!("expected Output variant"),
        }
    }

    #[test]
    fn browser_message_scrollback_start_serialization() {
        let msg = BrowserMessage::ScrollbackStart {
            cols: 120,
            rows: 40,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "scrollback_start");
        assert_eq!(json["cols"], 120);
        assert_eq!(json["rows"], 40);
    }

    #[test]
    fn browser_message_scrollback_end_serialization() {
        let msg = BrowserMessage::ScrollbackEnd;
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "scrollback_end");
    }

    // --- ServerEvent serialization tests ---

    #[test]
    fn server_event_host_connected_serialization() {
        let event = ServerEvent::HostConnected {
            host: HostInfo {
                id: "host-1".to_string(),
                hostname: "my-host".to_string(),
                status: HostStatus::Online,
                agent_version: Some("0.1.0".to_string()),
                os: Some("linux".to_string()),
                arch: Some("x86_64".to_string()),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "host_connected");
        assert_eq!(json["host"]["id"], "host-1");
        assert_eq!(json["host"]["hostname"], "my-host");
    }

    #[test]
    fn server_event_host_disconnected_serialization() {
        let event = ServerEvent::HostDisconnected {
            host_id: "host-1".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "host_disconnected");
        assert_eq!(json["host_id"], "host-1");
    }

    #[test]
    fn server_event_host_status_changed_serialization() {
        let event = ServerEvent::HostStatusChanged {
            host_id: "host-1".to_string(),
            status: HostStatus::Offline,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "host_status_changed");
        assert_eq!(json["host_id"], "host-1");
        assert_eq!(json["status"], "offline");
    }

    #[test]
    fn server_event_session_created_serialization() {
        let event = ServerEvent::SessionCreated {
            session: SessionInfo {
                id: "sess-1".to_string(),
                host_id: "host-1".to_string(),
                shell: Some("/bin/bash".to_string()),
                status: SessionStatus::Creating,
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "session_created");
        assert_eq!(json["session"]["id"], "sess-1");
        assert_eq!(json["session"]["shell"], "/bin/bash");
    }

    #[test]
    fn server_event_session_closed_serialization() {
        let event = ServerEvent::SessionClosed {
            session_id: "sess-1".to_string(),
            exit_code: Some(0),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert_eq!(json["session_id"], "sess-1");
        assert_eq!(json["exit_code"], 0);
    }

    #[test]
    fn server_event_session_closed_no_exit_code() {
        let event = ServerEvent::SessionClosed {
            session_id: "sess-1".to_string(),
            exit_code: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "session_closed");
        assert!(json["exit_code"].is_null());
    }

    #[test]
    fn server_event_projects_updated_serialization() {
        let event = ServerEvent::ProjectsUpdated {
            host_id: "host-1".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "projects_updated");
        assert_eq!(json["host_id"], "host-1");
    }

    #[test]
    fn server_event_knowledge_status_changed_serialization() {
        let event = ServerEvent::KnowledgeStatusChanged {
            host_id: "host-1".to_string(),
            status: "ready".to_string(),
            error: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "knowledge_status_changed");
        assert_eq!(json["host_id"], "host-1");
        assert_eq!(json["status"], "ready");
        assert!(json["error"].is_null());
    }

    #[test]
    fn server_event_knowledge_status_changed_with_error() {
        let event = ServerEvent::KnowledgeStatusChanged {
            host_id: "host-1".to_string(),
            status: "error".to_string(),
            error: Some("failed to start".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "knowledge_status_changed");
        assert_eq!(json["error"], "failed to start");
    }

    #[test]
    fn server_event_indexing_progress_serialization() {
        let event = ServerEvent::IndexingProgress {
            project_id: "proj-1".to_string(),
            project_path: "/home/user/project".to_string(),
            status: "in_progress".to_string(),
            files_processed: 42,
            files_total: 150,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "indexing_progress");
        assert_eq!(json["project_id"], "proj-1");
        assert_eq!(json["project_path"], "/home/user/project");
        assert_eq!(json["status"], "in_progress");
        assert_eq!(json["files_processed"], 42);
        assert_eq!(json["files_total"], 150);
    }

    #[test]
    fn server_event_memory_extracted_serialization() {
        let event = ServerEvent::MemoryExtracted {
            project_id: "proj-1".to_string(),
            loop_id: "loop-1".to_string(),
            memory_count: 3,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "memory_extracted");
        assert_eq!(json["project_id"], "proj-1");
        assert_eq!(json["loop_id"], "loop-1");
        assert_eq!(json["memory_count"], 3);
    }

    #[test]
    fn server_event_worktree_error_serialization() {
        let event = ServerEvent::WorktreeError {
            host_id: "host-1".to_string(),
            project_path: "/home/user/repo".to_string(),
            message: "branch already exists".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "worktree_error");
        assert_eq!(json["host_id"], "host-1");
        assert_eq!(json["project_path"], "/home/user/repo");
        assert_eq!(json["message"], "branch already exists");
    }

    #[test]
    fn server_event_claude_task_started_serialization() {
        let event = ServerEvent::ClaudeTaskStarted {
            task_id: "task-1".to_string(),
            session_id: "sess-1".to_string(),
            host_id: "host-1".to_string(),
            project_path: "/home/user/project".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "claude_task_started");
        assert_eq!(json["task_id"], "task-1");
        assert_eq!(json["session_id"], "sess-1");
        assert_eq!(json["host_id"], "host-1");
        assert_eq!(json["project_path"], "/home/user/project");
    }

    #[test]
    fn server_event_claude_task_updated_serialization() {
        let event = ServerEvent::ClaudeTaskUpdated {
            task_id: "task-1".to_string(),
            status: ClaudeTaskStatus::Active,
            loop_id: Some("loop-1".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "claude_task_updated");
        assert_eq!(json["task_id"], "task-1");
        assert_eq!(json["status"], "active");
        assert_eq!(json["loop_id"], "loop-1");
    }

    #[test]
    fn server_event_claude_task_updated_no_loop_serialization() {
        let event = ServerEvent::ClaudeTaskUpdated {
            task_id: "task-1".to_string(),
            status: ClaudeTaskStatus::Starting,
            loop_id: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "claude_task_updated");
        assert!(json["loop_id"].is_null());
    }

    #[test]
    fn server_event_claude_task_ended_serialization() {
        let event = ServerEvent::ClaudeTaskEnded {
            task_id: "task-1".to_string(),
            status: ClaudeTaskStatus::Completed,
            summary: Some("Fixed the bug".to_string()),
            session_id: Some("s-1".to_string()),
            host_id: Some("h-1".to_string()),
            project_path: Some("/home/user/project".to_string()),
            task_name: Some("fix tests".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "claude_task_ended");
        assert_eq!(json["task_id"], "task-1");
        assert_eq!(json["status"], "completed");
        assert_eq!(json["summary"], "Fixed the bug");
        assert_eq!(json["session_id"], "s-1");
        assert_eq!(json["host_id"], "h-1");
        assert_eq!(json["project_path"], "/home/user/project");
        assert_eq!(json["task_name"], "fix tests");
    }

    // --- Binary frame encoding/decoding tests ---

    #[test]
    fn encode_binary_output_main_pane() {
        let data = b"hello terminal";
        let frame = super::encode_binary_output(None, data);
        assert_eq!(frame[0], super::BINARY_TAG_OUTPUT);
        assert_eq!(&frame[1..], data);
    }

    #[test]
    fn encode_binary_output_specific_pane() {
        let data = b"pane output";
        let frame = super::encode_binary_output(Some("%5"), data);
        assert_eq!(frame[0], super::BINARY_TAG_PANE_OUTPUT);
        assert_eq!(frame[1], 2); // "%5" is 2 bytes
        assert_eq!(&frame[2..4], b"%5");
        assert_eq!(&frame[4..], data);
    }

    #[test]
    fn decode_binary_output_main_pane() {
        let data = b"hello";
        let frame = super::encode_binary_output(None, data);
        let (pane_id, decoded) = super::decode_binary_output(&frame).unwrap();
        assert!(pane_id.is_none());
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_binary_output_specific_pane() {
        let data = b"output";
        let frame = super::encode_binary_output(Some("%12"), data);
        let (pane_id, decoded) = super::decode_binary_output(&frame).unwrap();
        assert_eq!(pane_id.as_deref(), Some("%12"));
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_binary_output_empty_data() {
        let frame = super::encode_binary_output(None, b"");
        let (pane_id, decoded) = super::decode_binary_output(&frame).unwrap();
        assert!(pane_id.is_none());
        assert!(decoded.is_empty());
    }

    #[test]
    fn decode_binary_output_empty_frame() {
        assert!(super::decode_binary_output(&[]).is_none());
    }

    #[test]
    fn decode_binary_output_unknown_tag() {
        assert!(super::decode_binary_output(&[0xFF, 0x01]).is_none());
    }

    #[test]
    fn decode_binary_output_truncated_pane_frame() {
        // Tag + pid_len=5, but only 2 bytes of pid
        assert!(super::decode_binary_output(&[0x02, 5, b'a', b'b']).is_none());
    }

    #[test]
    fn server_event_claude_task_ended_error_serialization() {
        let event = ServerEvent::ClaudeTaskEnded {
            task_id: "task-1".to_string(),
            status: ClaudeTaskStatus::Error,
            summary: Some("PTY spawn failed".to_string()),
            session_id: None,
            host_id: None,
            project_path: None,
            task_name: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "claude_task_ended");
        assert_eq!(json["status"], "error");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn server_event_roundtrip() {
        let events = vec![
            ServerEvent::HostConnected {
                host: HostInfo {
                    id: "h1".to_string(),
                    hostname: "host".to_string(),
                    status: HostStatus::Online,
                    agent_version: None,
                    os: None,
                    arch: None,
                },
            },
            ServerEvent::HostDisconnected {
                host_id: "h1".to_string(),
            },
            ServerEvent::HostStatusChanged {
                host_id: "h1".to_string(),
                status: HostStatus::Offline,
            },
            ServerEvent::SessionCreated {
                session: SessionInfo {
                    id: "s1".to_string(),
                    host_id: "h1".to_string(),
                    shell: None,
                    status: SessionStatus::Creating,
                },
            },
            ServerEvent::SessionClosed {
                session_id: "s1".to_string(),
                exit_code: Some(1),
            },
            ServerEvent::SessionSuspended {
                session_id: "s1".to_string(),
            },
            ServerEvent::SessionResumed {
                session_id: "s1".to_string(),
            },
            ServerEvent::SessionUpdated {
                session_id: "s1".to_string(),
            },
            ServerEvent::LoopDetected {
                loop_info: LoopInfo {
                    id: "l1".to_string(),
                    session_id: "s1".to_string(),
                    project_path: None,
                    tool_name: "claude-code".to_string(),
                    status: zremote_protocol::AgenticStatus::Working,
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    ended_at: None,
                    end_reason: None,
                    task_name: None,
                    prompt_message: None,
                    permission_mode: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    cost_usd: None,
                },
                host_id: "h1".to_string(),
                hostname: "host".to_string(),
            },
            ServerEvent::LoopStatusChanged {
                loop_info: LoopInfo {
                    id: "l1".to_string(),
                    session_id: "s1".to_string(),
                    project_path: None,
                    tool_name: "claude-code".to_string(),
                    status: zremote_protocol::AgenticStatus::Working,
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    ended_at: None,
                    end_reason: None,
                    task_name: None,
                    prompt_message: None,
                    permission_mode: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    cost_usd: None,
                },
                host_id: "h1".to_string(),
                hostname: "host".to_string(),
            },
            ServerEvent::LoopEnded {
                loop_info: LoopInfo {
                    id: "l1".to_string(),
                    session_id: "s1".to_string(),
                    project_path: None,
                    tool_name: "claude-code".to_string(),
                    status: zremote_protocol::AgenticStatus::Completed,
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    ended_at: Some("2026-01-01T01:00:00Z".to_string()),
                    end_reason: Some("completed".to_string()),
                    task_name: None,
                    prompt_message: None,
                    permission_mode: None,
                    input_tokens: 0,
                    output_tokens: 0,
                    cost_usd: None,
                },
                host_id: "h1".to_string(),
                hostname: "host".to_string(),
            },
            ServerEvent::ProjectsUpdated {
                host_id: "h1".to_string(),
            },
            ServerEvent::KnowledgeStatusChanged {
                host_id: "h1".to_string(),
                status: "ready".to_string(),
                error: None,
            },
            ServerEvent::IndexingProgress {
                project_id: "p1".to_string(),
                project_path: "/home/user/project".to_string(),
                status: "in_progress".to_string(),
                files_processed: 10,
                files_total: 100,
            },
            ServerEvent::MemoryExtracted {
                project_id: "p1".to_string(),
                loop_id: "l1".to_string(),
                memory_count: 5,
            },
            ServerEvent::WorktreeError {
                host_id: "h1".to_string(),
                project_path: "/home/user/repo".to_string(),
                message: "error message".to_string(),
            },
            ServerEvent::ClaudeTaskStarted {
                task_id: "t1".to_string(),
                session_id: "s1".to_string(),
                host_id: "h1".to_string(),
                project_path: "/home/user/project".to_string(),
            },
            ServerEvent::ClaudeTaskUpdated {
                task_id: "t1".to_string(),
                status: ClaudeTaskStatus::Active,
                loop_id: Some("l1".to_string()),
            },
            ServerEvent::ClaudeTaskEnded {
                task_id: "t1".to_string(),
                status: ClaudeTaskStatus::Completed,
                summary: Some("done".to_string()),
                session_id: Some("s1".to_string()),
                host_id: Some("h1".to_string()),
                project_path: Some("/home/user/project".to_string()),
                task_name: Some("fix bug".to_string()),
            },
            ServerEvent::EventsLagged { missed: 10 },
        ];
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{parsed:?}"), format!("{event:?}"));
        }
    }
}
