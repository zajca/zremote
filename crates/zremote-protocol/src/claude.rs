use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::SessionId;

/// Status of a Claude task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeTaskStatus {
    Starting,
    Active,
    Completed,
    Error,
    Suspended,
    /// Forward-compatibility: unknown status from a newer server.
    #[serde(other)]
    Unknown,
}

/// Discovered Claude Code session info (for resume).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaudeSessionInfo {
    pub session_id: String,
    pub project_path: String,
    pub model: Option<String>,
    pub last_active: Option<String>,
    pub message_count: Option<u32>,
    pub summary: Option<String>,
}

/// Claude messages sent from server to agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
// StartSession is much larger than DiscoverSessions due to many String fields;
// boxing would require an API change across all call sites.
#[allow(clippy::large_enum_variant)]
pub enum ClaudeServerMessage {
    StartSession {
        session_id: SessionId,
        claude_task_id: Uuid,
        working_dir: String,
        model: Option<String>,
        initial_prompt: Option<String>,
        resume_cc_session_id: Option<String>,
        allowed_tools: Vec<String>,
        skip_permissions: bool,
        output_format: Option<String>,
        custom_flags: Option<String>,
        #[serde(default)]
        continue_last: bool,
        #[serde(default)]
        development_channels: Vec<String>,
        #[serde(default)]
        print_mode: bool,
    },
    DiscoverSessions {
        project_path: String,
    },
}

/// Claude messages sent from agent to server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum ClaudeAgentMessage {
    SessionStarted {
        claude_task_id: Uuid,
        session_id: SessionId,
    },
    SessionStartFailed {
        claude_task_id: Uuid,
        session_id: SessionId,
        error: String,
    },
    SessionsDiscovered {
        project_path: String,
        sessions: Vec<ClaudeSessionInfo>,
    },
    SessionIdCaptured {
        claude_task_id: Uuid,
        cc_session_id: String,
    },
    /// Claude Code status line metrics forwarded from agent in server mode.
    MetricsUpdate {
        cc_session_id: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        tokens_in: Option<u64>,
        #[serde(default)]
        tokens_out: Option<u64>,
        #[serde(default)]
        context_used_pct: Option<u64>,
        #[serde(default)]
        context_window_size: Option<u64>,
        #[serde(default)]
        rate_limit_5h_pct: Option<u64>,
        #[serde(default)]
        rate_limit_7d_pct: Option<u64>,
        #[serde(default)]
        lines_added: Option<i64>,
        #[serde(default)]
        lines_removed: Option<i64>,
        #[serde(default)]
        cc_version: Option<String>,
        #[serde(default)]
        permission_mode: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn roundtrip_agent(msg: &ClaudeAgentMessage) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: ClaudeAgentMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    fn roundtrip_server(msg: &ClaudeServerMessage) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: ClaudeServerMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    #[test]
    fn claude_task_status_serialization() {
        assert_eq!(
            serde_json::to_string(&ClaudeTaskStatus::Starting).unwrap(),
            r#""starting""#
        );
        assert_eq!(
            serde_json::to_string(&ClaudeTaskStatus::Active).unwrap(),
            r#""active""#
        );
        assert_eq!(
            serde_json::to_string(&ClaudeTaskStatus::Completed).unwrap(),
            r#""completed""#
        );
        assert_eq!(
            serde_json::to_string(&ClaudeTaskStatus::Error).unwrap(),
            r#""error""#
        );
        assert_eq!(
            serde_json::to_string(&ClaudeTaskStatus::Suspended).unwrap(),
            r#""suspended""#
        );
    }

    #[test]
    fn claude_task_status_deserialization() {
        assert_eq!(
            serde_json::from_str::<ClaudeTaskStatus>(r#""starting""#).unwrap(),
            ClaudeTaskStatus::Starting
        );
        assert_eq!(
            serde_json::from_str::<ClaudeTaskStatus>(r#""active""#).unwrap(),
            ClaudeTaskStatus::Active
        );
        assert_eq!(
            serde_json::from_str::<ClaudeTaskStatus>(r#""completed""#).unwrap(),
            ClaudeTaskStatus::Completed
        );
        assert_eq!(
            serde_json::from_str::<ClaudeTaskStatus>(r#""error""#).unwrap(),
            ClaudeTaskStatus::Error
        );
        assert_eq!(
            serde_json::from_str::<ClaudeTaskStatus>(r#""suspended""#).unwrap(),
            ClaudeTaskStatus::Suspended
        );
        assert_eq!(
            serde_json::from_str::<ClaudeTaskStatus>(r#""some_future_status""#).unwrap(),
            ClaudeTaskStatus::Unknown
        );
    }

    #[test]
    fn start_session_roundtrip() {
        roundtrip_server(&ClaudeServerMessage::StartSession {
            session_id: Uuid::new_v4(),
            claude_task_id: Uuid::new_v4(),
            working_dir: "/home/user/project".to_string(),
            model: Some("claude-sonnet-4-20250514".to_string()),
            initial_prompt: Some("Fix the bug in main.rs".to_string()),
            resume_cc_session_id: None,
            allowed_tools: vec!["Read".to_string(), "Write".to_string()],
            skip_permissions: false,
            output_format: None,
            custom_flags: None,
            continue_last: false,
            development_channels: vec![],
            print_mode: false,
        });
    }

    #[test]
    fn start_session_minimal_roundtrip() {
        roundtrip_server(&ClaudeServerMessage::StartSession {
            session_id: Uuid::new_v4(),
            claude_task_id: Uuid::new_v4(),
            working_dir: "/home/user/project".to_string(),
            model: None,
            initial_prompt: None,
            resume_cc_session_id: None,
            allowed_tools: vec![],
            skip_permissions: false,
            output_format: None,
            custom_flags: None,
            continue_last: false,
            development_channels: vec![],
            print_mode: false,
        });
    }

    #[test]
    fn start_session_with_resume_roundtrip() {
        roundtrip_server(&ClaudeServerMessage::StartSession {
            session_id: Uuid::new_v4(),
            claude_task_id: Uuid::new_v4(),
            working_dir: "/home/user/project".to_string(),
            model: None,
            initial_prompt: None,
            resume_cc_session_id: Some("abc123-session".to_string()),
            allowed_tools: vec![],
            skip_permissions: true,
            output_format: Some("stream-json".to_string()),
            custom_flags: Some("--verbose".to_string()),
            continue_last: false,
            development_channels: vec![],
            print_mode: false,
        });
    }

    #[test]
    fn start_session_with_continue_roundtrip() {
        roundtrip_server(&ClaudeServerMessage::StartSession {
            session_id: Uuid::new_v4(),
            claude_task_id: Uuid::new_v4(),
            working_dir: "/home/user/project".to_string(),
            model: None,
            initial_prompt: None,
            resume_cc_session_id: None,
            allowed_tools: vec![],
            skip_permissions: false,
            output_format: None,
            custom_flags: None,
            continue_last: true,
            development_channels: vec![],
            print_mode: false,
        });
    }

    #[test]
    fn discover_sessions_roundtrip() {
        roundtrip_server(&ClaudeServerMessage::DiscoverSessions {
            project_path: "/home/user/project".to_string(),
        });
    }

    #[test]
    fn session_started_roundtrip() {
        roundtrip_agent(&ClaudeAgentMessage::SessionStarted {
            claude_task_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
        });
    }

    #[test]
    fn session_start_failed_roundtrip() {
        roundtrip_agent(&ClaudeAgentMessage::SessionStartFailed {
            claude_task_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            error: "PTY spawn failed: no such shell".to_string(),
        });
    }

    #[test]
    fn sessions_discovered_roundtrip() {
        roundtrip_agent(&ClaudeAgentMessage::SessionsDiscovered {
            project_path: "/home/user/project".to_string(),
            sessions: vec![
                ClaudeSessionInfo {
                    session_id: "abc123".to_string(),
                    project_path: "/home/user/project".to_string(),
                    model: Some("claude-sonnet-4-20250514".to_string()),
                    last_active: Some("2026-03-16T10:00:00Z".to_string()),
                    message_count: Some(42),
                    summary: Some("Refactoring auth module".to_string()),
                },
                ClaudeSessionInfo {
                    session_id: "def456".to_string(),
                    project_path: "/home/user/project".to_string(),
                    model: None,
                    last_active: None,
                    message_count: None,
                    summary: None,
                },
            ],
        });
    }

    #[test]
    fn session_id_captured_roundtrip() {
        roundtrip_agent(&ClaudeAgentMessage::SessionIdCaptured {
            claude_task_id: Uuid::new_v4(),
            cc_session_id: "abc123-session-id".to_string(),
        });
    }

    #[test]
    fn sessions_discovered_empty_roundtrip() {
        roundtrip_agent(&ClaudeAgentMessage::SessionsDiscovered {
            project_path: "/home/user/project".to_string(),
            sessions: vec![],
        });
    }

    #[test]
    fn claude_session_info_roundtrip() {
        let info = ClaudeSessionInfo {
            session_id: "test-session-id".to_string(),
            project_path: "/home/user/project".to_string(),
            model: Some("claude-sonnet-4-20250514".to_string()),
            last_active: Some("2026-03-16T12:00:00Z".to_string()),
            message_count: Some(10),
            summary: Some("Working on tests".to_string()),
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let parsed: ClaudeSessionInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, parsed);
    }

    #[test]
    fn claude_session_info_minimal_roundtrip() {
        let info = ClaudeSessionInfo {
            session_id: "minimal".to_string(),
            project_path: "/tmp".to_string(),
            model: None,
            last_active: None,
            message_count: None,
            summary: None,
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let parsed: ClaudeSessionInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, parsed);
    }

    #[test]
    fn server_message_json_structure() {
        let msg = ClaudeServerMessage::StartSession {
            session_id: Uuid::nil(),
            claude_task_id: Uuid::nil(),
            working_dir: "/tmp".to_string(),
            model: None,
            initial_prompt: None,
            resume_cc_session_id: None,
            allowed_tools: vec![],
            skip_permissions: false,
            output_format: None,
            custom_flags: None,
            continue_last: false,
            development_channels: vec![],
            print_mode: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "StartSession");
        assert!(value["payload"].is_object());
    }

    #[test]
    fn agent_message_json_structure() {
        let msg = ClaudeAgentMessage::SessionStarted {
            claude_task_id: Uuid::nil(),
            session_id: Uuid::nil(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "SessionStarted");
        assert!(value["payload"].is_object());
    }

    #[test]
    fn metrics_update_roundtrip() {
        let msg = ClaudeAgentMessage::MetricsUpdate {
            cc_session_id: "cc-abc-123".to_string(),
            model: Some("opus".to_string()),
            cost_usd: Some(2.93),
            tokens_in: Some(30000),
            tokens_out: Some(15000),
            context_used_pct: Some(45),
            context_window_size: Some(1_000_000),
            rate_limit_5h_pct: Some(11),
            rate_limit_7d_pct: Some(85),
            lines_added: Some(168),
            lines_removed: Some(2),
            cc_version: Some("2.1.83".to_string()),
            permission_mode: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let parsed: ClaudeAgentMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, parsed);
    }

    #[test]
    fn metrics_update_minimal_roundtrip() {
        let msg = ClaudeAgentMessage::MetricsUpdate {
            cc_session_id: "cc-minimal".to_string(),
            model: None,
            cost_usd: None,
            tokens_in: None,
            tokens_out: None,
            context_used_pct: None,
            context_window_size: None,
            rate_limit_5h_pct: None,
            rate_limit_7d_pct: None,
            lines_added: None,
            lines_removed: None,
            cc_version: None,
            permission_mode: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let parsed: ClaudeAgentMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, parsed);
    }
}
