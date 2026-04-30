use serde::{Deserialize, Serialize};

use crate::AgenticStatus;
use crate::claude::ClaudeTaskStatus;
use crate::status::{HostStatus, SessionStatus};

/// Lifecycle stage of a worktree creation job. Emitted at least once per
/// stage — clients treat absent intermediate stages as "skipped fast".
///
/// `Done` and `Failed` are terminal; every job emits exactly one terminal
/// stage. `percent` accompanies each event but is advisory only — stages are
/// the source of truth for UI state.
#[derive(Debug, Clone, Hash, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCreationStage {
    Init,
    /// Reserved for a future remote-clone / async-fetch workflow (Phase 5+).
    /// The Phase 2 implementation does not emit this stage; it is kept here
    /// so the protocol is forward-compatible when the agent gains support.
    Fetching,
    Creating,
    Finalizing,
    Done,
    Failed,
    /// Forward-compat placeholder for stages added in future agent versions.
    #[serde(other)]
    Unknown,
}

/// Loop information for server events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopInfo {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub status: AgenticStatus,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
    pub task_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_available: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_description: Option<String>,
    /// Human-readable project name, resolved on the server/agent from the
    /// `projects` table by `(host_id, project_path)`. When `None`, the client
    /// may fall back to the basename of `project_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
}

/// Nested host info in server events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub id: String,
    pub hostname: String,
    #[serde(default)]
    pub status: HostStatus,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
}

/// Nested session info in server events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub host_id: String,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub status: SessionStatus,
}

/// Status of an execution node.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Running,
    Completed,
    Stopped,
    Stale,
    /// Forward-compat placeholder for statuses added in future agent versions.
    #[serde(other)]
    #[default]
    Unknown,
}

/// Real-time events broadcast to WebSocket clients and integrations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "host_connected")]
    HostConnected { host: HostInfo },
    #[serde(rename = "host_disconnected")]
    HostDisconnected { host_id: String },
    #[serde(rename = "host_status_changed")]
    HostStatusChanged { host_id: String, status: HostStatus },
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
    #[serde(rename = "session_updated")]
    SessionUpdated { session_id: String },
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
    #[serde(rename = "agentic_loop_metrics_update")]
    LoopMetricsUpdated {
        #[serde(rename = "loop")]
        loop_info: LoopInfo,
        host_id: String,
        hostname: String,
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
    #[serde(rename = "worktree_creation_progress")]
    WorktreeCreationProgress {
        project_id: String,
        job_id: String,
        stage: WorktreeCreationStage,
        /// Advisory completion percentage (0..=100). UI should prefer `stage`
        /// for state transitions and treat `percent` as an animation hint.
        #[serde(default)]
        percent: u8,
        /// Optional human-readable status line (e.g. "fetching origin").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
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
        status: ClaudeTaskStatus,
        loop_id: Option<String>,
    },
    #[serde(rename = "claude_task_ended")]
    ClaudeTaskEnded {
        task_id: String,
        status: ClaudeTaskStatus,
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_name: Option<String>,
    },
    #[serde(rename = "claude_session_metrics")]
    ClaudeSessionMetrics {
        session_id: String,
        model: Option<String>,
        context_used_pct: Option<f64>,
        context_window_size: Option<u64>,
        cost_usd: Option<f64>,
        tokens_in: Option<u64>,
        tokens_out: Option<u64>,
        lines_added: Option<i64>,
        lines_removed: Option<i64>,
        rate_limit_5h_pct: Option<u64>,
        rate_limit_7d_pct: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
    },
    #[serde(rename = "execution_node_created")]
    ExecutionNodeCreated {
        session_id: String,
        host_id: String,
        node_id: i64,
        tool_use_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_id: Option<String>,
        #[serde(default)]
        timestamp: i64,
        #[serde(default)]
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        working_dir: String,
        status: NodeStatus,
    },
    #[serde(rename = "execution_node_updated")]
    ExecutionNodeUpdated {
        session_id: String,
        host_id: String,
        node_id: i64,
        tool_use_id: String,
        status: NodeStatus,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        duration_ms: i64,
    },
    #[serde(rename = "channel_permission_requested")]
    ChannelPermissionRequested {
        session_id: String,
        host_id: String,
        request_id: String,
        tool_name: String,
    },
    #[serde(rename = "channel_worker_reply")]
    ChannelWorkerReply {
        session_id: String,
        host_id: String,
        message: String,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        metadata: std::collections::HashMap<String, String>,
    },
    #[serde(rename = "events_lagged")]
    EventsLagged { missed: u64 },
    /// Unknown event type for forward compatibility.
    /// New event types added in future versions will deserialize as `Unknown`
    /// instead of failing, allowing older clients to gracefully ignore them.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_loop_info(status: AgenticStatus) -> LoopInfo {
        LoopInfo {
            id: "l1".to_string(),
            session_id: "s1".to_string(),
            project_path: None,
            tool_name: "claude-code".to_string(),
            status,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            ended_at: None,
            end_reason: None,
            task_name: None,
            prompt_message: None,
            permission_mode: None,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: None,
            channel_available: None,
            action_tool_name: None,
            action_description: None,
            project_name: None,
        }
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
                loop_info: make_loop_info(AgenticStatus::Working),
                host_id: "h1".to_string(),
                hostname: "host".to_string(),
            },
            ServerEvent::LoopStatusChanged {
                loop_info: make_loop_info(AgenticStatus::WaitingForInput),
                host_id: "h1".to_string(),
                hostname: "host".to_string(),
            },
            ServerEvent::LoopEnded {
                loop_info: LoopInfo {
                    ended_at: Some("2026-01-01T01:00:00Z".to_string()),
                    end_reason: Some("completed".to_string()),
                    ..make_loop_info(AgenticStatus::Completed)
                },
                host_id: "h1".to_string(),
                hostname: "host".to_string(),
            },
            ServerEvent::LoopMetricsUpdated {
                loop_info: LoopInfo {
                    input_tokens: 5000,
                    output_tokens: 1200,
                    cost_usd: Some(0.15),
                    ..make_loop_info(AgenticStatus::Working)
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
            ServerEvent::ClaudeSessionMetrics {
                session_id: "cs1".to_string(),
                model: Some("opus".to_string()),
                context_used_pct: Some(45.0),
                context_window_size: Some(1_000_000),
                cost_usd: Some(2.5),
                tokens_in: Some(30000),
                tokens_out: Some(15000),
                lines_added: Some(100),
                lines_removed: Some(10),
                rate_limit_5h_pct: Some(11),
                rate_limit_7d_pct: Some(85),
                permission_mode: None,
            },
            ServerEvent::EventsLagged { missed: 42 },
        ];
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{parsed:?}"), format!("{event:?}"));
        }
    }

    #[test]
    fn loop_info_backward_compat_missing_metrics() {
        let json = r#"{"id":"l1","session_id":"s1","project_path":null,"tool_name":"t","status":"working","started_at":"2026-01-01T00:00:00Z","ended_at":null,"end_reason":null,"task_name":null}"#;
        let info: LoopInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.input_tokens, 0);
        assert_eq!(info.output_tokens, 0);
        assert!(info.cost_usd.is_none());
    }

    #[test]
    fn loop_info_backward_compat_missing_project_name() {
        // Payload from an older agent/server that never serialized `project_name`.
        let json = r#"{"id":"l1","session_id":"s1","project_path":"/work/repo","tool_name":"t","status":"working","started_at":"2026-01-01T00:00:00Z","ended_at":null,"end_reason":null,"task_name":null}"#;
        let info: LoopInfo = serde_json::from_str(json).unwrap();
        assert!(info.project_name.is_none());
    }

    #[test]
    fn loop_info_project_name_roundtrip() {
        let info = LoopInfo {
            project_name: Some("myremote".to_string()),
            ..make_loop_info(AgenticStatus::WaitingForInput)
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"project_name\":\"myremote\""));
        let parsed: LoopInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.project_name.as_deref(), Some("myremote"));
    }

    #[test]
    fn loop_info_project_name_none_skipped_in_json() {
        // When project_name is None the field should not appear in the JSON —
        // preserves wire compatibility with older clients.
        let info = make_loop_info(AgenticStatus::Working);
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("project_name"));
    }

    #[test]
    fn loop_info_status_unknown_variant() {
        let json = r#"{"id":"l1","session_id":"s1","project_path":null,"tool_name":"t","status":"some_future_status","started_at":"2026-01-01T00:00:00Z","ended_at":null,"end_reason":null,"task_name":null}"#;
        let info: LoopInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.status, AgenticStatus::Unknown);
    }

    #[test]
    fn host_info_missing_status_defaults() {
        let json = r#"{"id":"h1","hostname":"host","agent_version":null,"os":null,"arch":null}"#;
        let info: HostInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.status, HostStatus::default());
    }

    #[test]
    fn session_info_missing_fields_default() {
        let json = r#"{"id":"s1","host_id":"h1"}"#;
        let info: SessionInfo = serde_json::from_str(json).unwrap();
        assert!(info.shell.is_none());
        assert_eq!(info.status, SessionStatus::default());
    }

    #[test]
    fn events_lagged_serialization() {
        let event = ServerEvent::EventsLagged { missed: 150 };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "events_lagged");
        assert_eq!(json["missed"], 150);
    }

    #[test]
    fn events_lagged_deserialization() {
        let json = r#"{"type":"events_lagged","missed":99}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        match event {
            ServerEvent::EventsLagged { missed } => assert_eq!(missed, 99),
            other => panic!("expected EventsLagged, got {other:?}"),
        }
    }

    #[test]
    fn unknown_event_type_deserializes() {
        let json = r#"{"type":"future_event_v2","some_field":"value"}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, ServerEvent::Unknown));
    }

    #[test]
    fn unknown_event_roundtrip_serializes_as_unknown() {
        let event = ServerEvent::Unknown;
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Unknown"));
    }

    #[test]
    fn channel_permission_requested_roundtrip() {
        let event = ServerEvent::ChannelPermissionRequested {
            session_id: "s1".to_string(),
            host_id: "h1".to_string(),
            request_id: "perm-001".to_string(),
            tool_name: "Bash".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{parsed:?}"), format!("{event:?}"));
    }

    #[test]
    fn worktree_progress_event_serialization_round_trip() {
        let events = vec![
            ServerEvent::WorktreeCreationProgress {
                project_id: "p1".to_string(),
                job_id: "job-abc".to_string(),
                stage: WorktreeCreationStage::Init,
                percent: 0,
                message: None,
            },
            ServerEvent::WorktreeCreationProgress {
                project_id: "p1".to_string(),
                job_id: "job-abc".to_string(),
                stage: WorktreeCreationStage::Creating,
                percent: 50,
                message: Some("running git worktree add".to_string()),
            },
            ServerEvent::WorktreeCreationProgress {
                project_id: "p1".to_string(),
                job_id: "job-abc".to_string(),
                stage: WorktreeCreationStage::Done,
                percent: 100,
                message: None,
            },
            ServerEvent::WorktreeCreationProgress {
                project_id: "p1".to_string(),
                job_id: "job-abc".to_string(),
                stage: WorktreeCreationStage::Failed,
                percent: 50,
                message: Some("branch exists".to_string()),
            },
        ];
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{parsed:?}"), format!("{event:?}"));
        }

        // Stages serialize as snake_case.
        let json = serde_json::to_value(&WorktreeCreationStage::Fetching).unwrap();
        assert_eq!(json, serde_json::json!("fetching"));
    }

    #[test]
    fn worktree_progress_unknown_stage_deserializes_as_unknown() {
        let stage: WorktreeCreationStage =
            serde_json::from_value(serde_json::json!("some_future_stage")).unwrap();
        assert_eq!(stage, WorktreeCreationStage::Unknown);
    }

    #[test]
    fn worktree_progress_percent_defaults_and_message_skipped() {
        // Older agent omits percent and message.
        let json = r#"{"type":"worktree_creation_progress","project_id":"p1","job_id":"j","stage":"init"}"#;
        let event: ServerEvent = serde_json::from_str(json).unwrap();
        match event {
            ServerEvent::WorktreeCreationProgress {
                percent, message, ..
            } => {
                assert_eq!(percent, 0);
                assert!(message.is_none());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn channel_worker_reply_roundtrip() {
        let event = ServerEvent::ChannelWorkerReply {
            session_id: "s1".to_string(),
            host_id: "h1".to_string(),
            message: "Tests fixed".to_string(),
            metadata: std::collections::HashMap::from([(
                "duration".to_string(),
                "30s".to_string(),
            )]),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{parsed:?}"), format!("{event:?}"));
    }

    // Test #11: Round-trip for ExecutionNodeCreated and ExecutionNodeUpdated
    #[test]
    fn execution_node_created_and_updated_roundtrip() {
        let created = ServerEvent::ExecutionNodeCreated {
            session_id: "s1".to_string(),
            host_id: "h1".to_string(),
            node_id: 42,
            tool_use_id: "toolu_abc".to_string(),
            loop_id: Some("l1".to_string()),
            timestamp: 1_711_843_200_000,
            kind: "read".to_string(),
            input: Some("src/main.rs".to_string()),
            working_dir: "/home/user".to_string(),
            status: NodeStatus::Running,
        };
        let json = serde_json::to_string(&created).unwrap();
        let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{parsed:?}"), format!("{created:?}"));

        let updated = ServerEvent::ExecutionNodeUpdated {
            session_id: "s1".to_string(),
            host_id: "h1".to_string(),
            node_id: 42,
            tool_use_id: "toolu_abc".to_string(),
            status: NodeStatus::Completed,
            kind: "read".to_string(),
            output_summary: Some("fn main() {}".to_string()),
            exit_code: None,
            duration_ms: 1234,
        };
        let json = serde_json::to_string(&updated).unwrap();
        let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{parsed:?}"), format!("{updated:?}"));
    }

    // Test #12: NodeStatus serializes as snake_case
    #[test]
    fn node_status_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&NodeStatus::Running).unwrap(),
            r#""running""#
        );
        assert_eq!(
            serde_json::to_string(&NodeStatus::Completed).unwrap(),
            r#""completed""#
        );
        assert_eq!(
            serde_json::to_string(&NodeStatus::Stopped).unwrap(),
            r#""stopped""#
        );
        assert_eq!(
            serde_json::to_string(&NodeStatus::Stale).unwrap(),
            r#""stale""#
        );
    }

    #[test]
    fn channel_worker_reply_empty_metadata_roundtrip() {
        let event = ServerEvent::ChannelWorkerReply {
            session_id: "s1".to_string(),
            host_id: "h1".to_string(),
            message: "Done".to_string(),
            metadata: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("metadata"));
        let parsed: ServerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{parsed:?}"), format!("{event:?}"));
    }

    #[test]
    fn loop_info_channel_available_backward_compat() {
        let json = r#"{"id":"l1","session_id":"s1","project_path":null,"tool_name":"t","status":"working","started_at":"2026-01-01T00:00:00Z","ended_at":null,"end_reason":null,"task_name":null}"#;
        let info: LoopInfo = serde_json::from_str(json).unwrap();
        assert!(info.channel_available.is_none());
    }

    #[test]
    fn loop_info_channel_available_present() {
        let mut info = make_loop_info(AgenticStatus::Working);
        info.channel_available = Some(true);
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("channel_available"));
        let parsed: LoopInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.channel_available, Some(true));
    }
}
