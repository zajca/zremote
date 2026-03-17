use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc};
use tokio::time::Instant;
use zremote_protocol::agentic::AgenticStatus;
use zremote_protocol::{AgenticLoopId, HostId, SessionId};

pub const MAX_SCROLLBACK_BYTES: usize = 100 * 1024; // 100KB

/// In-memory state for an active terminal session.
pub struct SessionState {
    pub session_id: SessionId,
    pub host_id: HostId,
    pub status: String,
    pub browser_senders: Vec<mpsc::Sender<BrowserMessage>>,
    pub scrollback: VecDeque<Vec<u8>>,
    pub scrollback_size: usize,
    /// Per-pane scrollback buffers for extra tmux panes (pane_id -> (chunks, total_size)).
    pub pane_scrollbacks: HashMap<String, (VecDeque<Vec<u8>>, usize)>,
}

impl SessionState {
    pub fn new(session_id: SessionId, host_id: HostId) -> Self {
        Self {
            session_id,
            host_id,
            status: "creating".to_string(),
            browser_senders: Vec::new(),
            scrollback: VecDeque::new(),
            scrollback_size: 0,
            pane_scrollbacks: HashMap::new(),
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

    /// Append data to a specific pane's scrollback buffer.
    pub fn append_pane_scrollback(&mut self, pane_id: &str, data: Vec<u8>) {
        let (chunks, size) = self
            .pane_scrollbacks
            .entry(pane_id.to_owned())
            .or_insert_with(|| (VecDeque::new(), 0));
        *size += data.len();
        chunks.push_back(data);
        while *size > MAX_SCROLLBACK_BYTES {
            if let Some(old) = chunks.pop_front() {
                *size -= old.len();
            }
        }
    }

    /// Remove a pane's scrollback buffer.
    pub fn remove_pane_scrollback(&mut self, pane_id: &str) {
        self.pane_scrollbacks.remove(pane_id);
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
    ScrollbackStart,
    #[serde(rename = "scrollback_end")]
    ScrollbackEnd,
    #[serde(rename = "pane_added")]
    PaneAdded { pane_id: String, index: u16 },
    #[serde(rename = "pane_removed")]
    PaneRemoved { pane_id: String },
}

/// Thread-safe store for active session state.
pub type SessionStore = Arc<RwLock<HashMap<SessionId, SessionState>>>;

/// In-memory state for an active agentic loop.
#[derive(Debug)]
pub struct AgenticLoopState {
    pub loop_id: AgenticLoopId,
    pub session_id: SessionId,
    pub status: AgenticStatus,
    pub pending_tool_calls: VecDeque<PendingToolCall>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub estimated_cost_usd: f64,
    pub context_used: u64,
    pub context_max: u64,
    pub last_updated: Instant,
}

/// A pending tool call in the agentic loop queue.
#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub tool_call_id: uuid::Uuid,
    pub tool_name: String,
    pub arguments_json: String,
}

/// Thread-safe store for active agentic loop state.
pub type AgenticLoopStore = Arc<DashMap<AgenticLoopId, AgenticLoopState>>;

/// Loop information matching the frontend `AgenticLoop` interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopInfo {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub model: Option<String>,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub total_tokens_in: i64,
    pub total_tokens_out: i64,
    pub estimated_cost_usd: f64,
    pub end_reason: Option<String>,
    pub summary: Option<String>,
    pub context_used: i64,
    pub context_max: i64,
    pub pending_tool_calls: i64,
    pub task_name: Option<String>,
}

/// Tool call information matching the frontend `ToolCall` interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallInfo {
    pub id: String,
    pub loop_id: String,
    pub tool_name: String,
    pub arguments_json: Option<String>,
    pub status: String,
    pub result_preview: Option<String>,
    pub duration_ms: Option<i64>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// Transcript entry information matching the frontend `TranscriptEntry` interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntryInfo {
    pub id: i64,
    pub loop_id: String,
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub timestamp: String,
}

/// Real-time events broadcast to browser WebSocket clients and Telegram bot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "host_connected")]
    HostConnected { host: HostInfo },
    #[serde(rename = "host_disconnected")]
    HostDisconnected { host_id: String },
    #[serde(rename = "host_status_changed")]
    HostStatusChanged { host_id: String, status: String },
    #[serde(rename = "session_created")]
    SessionCreated { session: SessionInfo },
    #[serde(rename = "session_closed")]
    SessionClosed {
        session_id: String,
        exit_code: Option<i32>,
    },
    #[serde(rename = "session_suspended")]
    SessionSuspended { session_id: String },
    #[serde(rename = "session_resumed")]
    SessionResumed { session_id: String },
    #[serde(rename = "agentic_loop_detected")]
    LoopDetected {
        #[serde(rename = "loop")]
        loop_info: LoopInfo,
        host_id: String,
        hostname: String,
    },
    #[serde(rename = "agentic_loop_state_update")]
    LoopStatusChanged {
        #[serde(rename = "loop")]
        loop_info: LoopInfo,
        host_id: String,
        hostname: String,
    },
    #[serde(rename = "agentic_loop_ended")]
    LoopEnded {
        #[serde(rename = "loop")]
        loop_info: LoopInfo,
        host_id: String,
        hostname: String,
    },
    #[serde(rename = "agentic_loop_tool_call")]
    ToolCallPending {
        loop_id: String,
        tool_call: ToolCallInfo,
        host_id: String,
        hostname: String,
    },
    #[serde(rename = "agentic_loop_tool_result")]
    ToolCallResult {
        loop_id: String,
        tool_call: ToolCallInfo,
    },
    #[serde(rename = "agentic_loop_transcript")]
    LoopTranscript {
        loop_id: String,
        transcript_entry: TranscriptEntryInfo,
    },
    #[serde(rename = "agentic_loop_metrics")]
    LoopMetrics {
        #[serde(rename = "loop")]
        loop_info: LoopInfo,
    },
    #[serde(rename = "projects_updated")]
    ProjectsUpdated { host_id: String },
    #[serde(rename = "knowledge_status_changed")]
    KnowledgeStatusChanged {
        host_id: String,
        status: String,
        error: Option<String>,
    },
    #[serde(rename = "indexing_progress")]
    IndexingProgress {
        project_id: String,
        project_path: String,
        status: String,
        files_processed: u64,
        files_total: u64,
    },
    #[serde(rename = "memory_extracted")]
    MemoryExtracted {
        project_id: String,
        loop_id: String,
        memory_count: u32,
    },
    #[serde(rename = "worktree_error")]
    WorktreeError {
        host_id: String,
        project_path: String,
        message: String,
    },
    #[serde(rename = "claude_task_started")]
    ClaudeTaskStarted {
        task_id: String,
        session_id: String,
        host_id: String,
        project_path: String,
    },
    #[serde(rename = "claude_task_updated")]
    ClaudeTaskUpdated {
        task_id: String,
        status: String,
        loop_id: Option<String>,
    },
    #[serde(rename = "claude_task_ended")]
    ClaudeTaskEnded {
        task_id: String,
        status: String,
        summary: Option<String>,
        total_cost_usd: f64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub id: String,
    pub hostname: String,
    pub status: String,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub host_id: String,
    pub shell: Option<String>,
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // --- SessionState tests ---

    #[test]
    fn session_state_new_has_empty_scrollback() {
        let state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        assert!(state.scrollback.is_empty());
        assert_eq!(state.scrollback_size, 0);
        assert_eq!(state.status, "creating");
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
            BrowserMessage::ScrollbackStart,
            BrowserMessage::ScrollbackEnd,
            BrowserMessage::PaneAdded {
                pane_id: "%5".to_string(),
                index: 1,
            },
            BrowserMessage::PaneRemoved {
                pane_id: "%5".to_string(),
            },
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
    fn browser_message_pane_added_serialization() {
        let msg = BrowserMessage::PaneAdded {
            pane_id: "%7".to_string(),
            index: 2,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "pane_added");
        assert_eq!(json["pane_id"], "%7");
        assert_eq!(json["index"], 2);
    }

    #[test]
    fn browser_message_pane_removed_serialization() {
        let msg = BrowserMessage::PaneRemoved {
            pane_id: "%7".to_string(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "pane_removed");
        assert_eq!(json["pane_id"], "%7");
    }

    #[test]
    fn session_state_pane_scrollback() {
        let mut state = SessionState::new(Uuid::new_v4(), Uuid::new_v4());
        state.append_pane_scrollback("%5", vec![1, 2, 3]);
        state.append_pane_scrollback("%5", vec![4, 5]);
        assert_eq!(state.pane_scrollbacks.len(), 1);
        let (chunks, size) = state.pane_scrollbacks.get("%5").unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(*size, 5);

        state.remove_pane_scrollback("%5");
        assert!(state.pane_scrollbacks.is_empty());
    }

    #[test]
    fn browser_message_scrollback_start_serialization() {
        let msg = BrowserMessage::ScrollbackStart;
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "scrollback_start");
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
                status: "online".to_string(),
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
            status: "offline".to_string(),
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
                status: "creating".to_string(),
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
            status: "active".to_string(),
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
            status: "starting".to_string(),
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
            status: "completed".to_string(),
            summary: Some("Fixed the bug".to_string()),
            total_cost_usd: 0.42,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "claude_task_ended");
        assert_eq!(json["task_id"], "task-1");
        assert_eq!(json["status"], "completed");
        assert_eq!(json["summary"], "Fixed the bug");
        assert_eq!(json["total_cost_usd"], 0.42);
    }

    #[test]
    fn server_event_claude_task_ended_error_serialization() {
        let event = ServerEvent::ClaudeTaskEnded {
            task_id: "task-1".to_string(),
            status: "error".to_string(),
            summary: Some("PTY spawn failed".to_string()),
            total_cost_usd: 0.0,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "claude_task_ended");
        assert_eq!(json["status"], "error");
        assert_eq!(json["total_cost_usd"], 0.0);
    }

    #[test]
    fn server_event_roundtrip() {
        let events = vec![
            ServerEvent::HostConnected {
                host: HostInfo {
                    id: "h1".to_string(),
                    hostname: "host".to_string(),
                    status: "online".to_string(),
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
                status: "offline".to_string(),
            },
            ServerEvent::SessionCreated {
                session: SessionInfo {
                    id: "s1".to_string(),
                    host_id: "h1".to_string(),
                    shell: None,
                    status: "creating".to_string(),
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
            ServerEvent::LoopDetected {
                loop_info: LoopInfo {
                    id: "l1".to_string(),
                    session_id: "s1".to_string(),
                    project_path: None,
                    tool_name: "claude-code".to_string(),
                    model: None,
                    status: "working".to_string(),
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    ended_at: None,
                    total_tokens_in: 0,
                    total_tokens_out: 0,
                    estimated_cost_usd: 0.0,
                    end_reason: None,
                    summary: None,
                    context_used: 0,
                    context_max: 0,
                    pending_tool_calls: 0,
                    task_name: None,
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
                    model: None,
                    status: "working".to_string(),
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    ended_at: None,
                    total_tokens_in: 0,
                    total_tokens_out: 0,
                    estimated_cost_usd: 0.0,
                    end_reason: None,
                    summary: None,
                    context_used: 0,
                    context_max: 0,
                    pending_tool_calls: 0,
                    task_name: None,
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
                    model: None,
                    status: "completed".to_string(),
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    ended_at: Some("2026-01-01T01:00:00Z".to_string()),
                    total_tokens_in: 100,
                    total_tokens_out: 200,
                    estimated_cost_usd: 0.42,
                    end_reason: Some("completed".to_string()),
                    summary: Some("done".to_string()),
                    context_used: 0,
                    context_max: 0,
                    pending_tool_calls: 0,
                    task_name: None,
                },
                host_id: "h1".to_string(),
                hostname: "host".to_string(),
            },
            ServerEvent::ToolCallPending {
                loop_id: "l1".to_string(),
                tool_call: ToolCallInfo {
                    id: "tc1".to_string(),
                    loop_id: "l1".to_string(),
                    tool_name: "Bash".to_string(),
                    arguments_json: Some(r#"{"cmd":"ls"}"#.to_string()),
                    status: "pending".to_string(),
                    result_preview: None,
                    duration_ms: None,
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    resolved_at: None,
                },
                host_id: "h1".to_string(),
                hostname: "host".to_string(),
            },
            ServerEvent::ToolCallResult {
                loop_id: "l1".to_string(),
                tool_call: ToolCallInfo {
                    id: "tc1".to_string(),
                    loop_id: "l1".to_string(),
                    tool_name: "Bash".to_string(),
                    arguments_json: Some(r#"{"cmd":"ls"}"#.to_string()),
                    status: "completed".to_string(),
                    result_preview: Some("file.txt".to_string()),
                    duration_ms: Some(100),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                    resolved_at: Some("2026-01-01T00:00:01Z".to_string()),
                },
            },
            ServerEvent::LoopTranscript {
                loop_id: "l1".to_string(),
                transcript_entry: TranscriptEntryInfo {
                    id: 1,
                    loop_id: "l1".to_string(),
                    role: "assistant".to_string(),
                    content: "Hello".to_string(),
                    tool_call_id: None,
                    timestamp: "2026-01-01T00:00:00Z".to_string(),
                },
            },
            ServerEvent::LoopMetrics {
                loop_info: LoopInfo {
                    id: "l1".to_string(),
                    session_id: "s1".to_string(),
                    project_path: None,
                    tool_name: "claude-code".to_string(),
                    model: Some("sonnet".to_string()),
                    status: "working".to_string(),
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    ended_at: None,
                    total_tokens_in: 500,
                    total_tokens_out: 1000,
                    estimated_cost_usd: 0.05,
                    end_reason: None,
                    summary: None,
                    context_used: 5000,
                    context_max: 200_000,
                    pending_tool_calls: 0,
                    task_name: None,
                },
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
                status: "active".to_string(),
                loop_id: Some("l1".to_string()),
            },
            ServerEvent::ClaudeTaskEnded {
                task_id: "t1".to_string(),
                status: "completed".to_string(),
                summary: Some("done".to_string()),
                total_cost_usd: 1.23,
            },
        ];
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{parsed:?}"), format!("{event:?}"));
        }
    }
}
