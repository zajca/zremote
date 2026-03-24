use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::claude::{ClaudeAgentMessage, ClaudeServerMessage};
use crate::knowledge::{KnowledgeAgentMessage, KnowledgeServerMessage};
use crate::project::{
    DirectoryEntry, GitInfo, ProjectInfo, ProjectSettings, ResolvedActionInput, WorktreeInfo,
};
use crate::{HostId, SessionId};

/// A recovered tmux session reported by the agent during reconnection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecoveredSession {
    pub session_id: SessionId,
    pub shell: String,
    pub pid: u32,
}

/// Result of a worktree lifecycle hook execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HookResultInfo {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    pub duration_ms: u64,
}

/// Messages sent from agent to server (terminal/connection layer).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum AgentMessage {
    Register {
        hostname: String,
        agent_version: String,
        os: String,
        arch: String,
        token: String,
        #[serde(default)]
        supports_persistent_sessions: bool,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tmux_name: Option<String>,
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
        #[serde(default)]
        has_zremote_config: bool,
        project_type: String,
    },
    ProjectList {
        projects: Vec<ProjectInfo>,
    },
    GitStatusUpdate {
        path: String,
        git_info: GitInfo,
        worktrees: Vec<WorktreeInfo>,
    },
    WorktreeCreated {
        project_path: String,
        worktree: WorktreeInfo,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hook_result: Option<HookResultInfo>,
    },
    WorktreeHookResult {
        project_path: String,
        worktree_path: String,
        hook_type: String,
        success: bool,
        output: Option<String>,
        duration_ms: u64,
    },
    WorktreeDeleted {
        project_path: String,
        worktree_path: String,
    },
    WorktreeError {
        project_path: String,
        message: String,
    },
    SessionsRecovered {
        sessions: Vec<RecoveredSession>,
    },
    DirectoryListing {
        request_id: uuid::Uuid,
        path: String,
        entries: Vec<DirectoryEntry>,
        error: Option<String>,
    },
    ProjectSettingsResult {
        request_id: uuid::Uuid,
        settings: Option<Box<ProjectSettings>>,
        error: Option<String>,
    },
    ProjectSettingsSaved {
        request_id: uuid::Uuid,
        error: Option<String>,
    },
    ActionInputsResolved {
        request_id: uuid::Uuid,
        inputs: Vec<ResolvedActionInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    KnowledgeAction(KnowledgeAgentMessage),
    ClaudeAction(ClaudeAgentMessage),
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
        #[serde(default)]
        env: Option<std::collections::HashMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial_command: Option<String>,
    },
    SessionClose {
        session_id: SessionId,
    },
    TerminalInput {
        session_id: SessionId,
        data: Vec<u8>,
    },
    TerminalImagePaste {
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
    KnowledgeAction(KnowledgeServerMessage),
    ClaudeAction(ClaudeServerMessage),
    ProjectScan,
    ProjectRegister {
        path: String,
    },
    ProjectRemove {
        path: String,
    },
    ProjectGitStatus {
        path: String,
    },
    WorktreeCreate {
        project_path: String,
        branch: String,
        path: Option<String>,
        new_branch: bool,
    },
    WorktreeDelete {
        project_path: String,
        worktree_path: String,
        force: bool,
    },
    ListDirectory {
        request_id: uuid::Uuid,
        path: String,
    },
    ProjectGetSettings {
        request_id: uuid::Uuid,
        project_path: String,
    },
    ProjectSaveSettings {
        request_id: uuid::Uuid,
        project_path: String,
        settings: Box<ProjectSettings>,
    },
    ResolveActionInputs {
        request_id: uuid::Uuid,
        project_path: String,
        action_name: String,
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
            supports_persistent_sessions: false,
        });
        roundtrip_agent(&AgentMessage::Register {
            hostname: "dev-machine".to_string(),
            agent_version: "0.1.0".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            token: "secret-token".to_string(),
            supports_persistent_sessions: true,
        });
    }

    #[test]
    fn register_without_persistent_sessions_deserializes() {
        // Backward compat: older agents won't send supports_persistent_sessions
        let json = r#"{"type":"Register","payload":{"hostname":"h","agent_version":"0.1","os":"linux","arch":"x86_64","token":"t"}}"#;
        let msg: AgentMessage = serde_json::from_str(json).expect("should deserialize");
        if let AgentMessage::Register {
            supports_persistent_sessions,
            ..
        } = msg
        {
            assert!(!supports_persistent_sessions, "should default to false");
        } else {
            panic!("expected Register variant");
        }
    }

    #[test]
    fn sessions_recovered_roundtrip() {
        roundtrip_agent(&AgentMessage::SessionsRecovered {
            sessions: vec![
                RecoveredSession {
                    session_id: Uuid::new_v4(),
                    shell: "/bin/zsh".to_string(),
                    pid: 12345,
                },
                RecoveredSession {
                    session_id: Uuid::new_v4(),
                    shell: "/bin/bash".to_string(),
                    pid: 67890,
                },
            ],
        });
        roundtrip_agent(&AgentMessage::SessionsRecovered { sessions: vec![] });
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
    fn terminal_image_paste_roundtrip() {
        roundtrip_server(&ServerMessage::TerminalImagePaste {
            session_id: Uuid::new_v4(),
            data: vec![0x89, 0x50, 0x4e, 0x47], // PNG magic bytes
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
            env: Some(std::collections::HashMap::from([
                ("RUST_LOG".to_string(), "debug".to_string()),
                ("MY_VAR".to_string(), "value".to_string()),
            ])),
            initial_command: None,
        });
        roundtrip_server(&ServerMessage::SessionCreate {
            session_id: Uuid::new_v4(),
            shell: None,
            cols: 80,
            rows: 24,
            working_dir: None,
            env: None,
            initial_command: None,
        });
    }

    #[test]
    fn session_create_without_env_deserializes() {
        // Backward compat: older servers/agents won't send env field
        let json = r#"{"type":"SessionCreate","payload":{"session_id":"550e8400-e29b-41d4-a716-446655440000","shell":"/bin/bash","cols":80,"rows":24,"working_dir":null}}"#;
        let msg: ServerMessage = serde_json::from_str(json).expect("should deserialize");
        if let ServerMessage::SessionCreate {
            env,
            initial_command,
            ..
        } = msg
        {
            assert!(env.is_none(), "env should default to None");
            assert!(
                initial_command.is_none(),
                "initial_command should default to None"
            );
        } else {
            panic!("expected SessionCreate variant");
        }
    }

    #[test]
    fn session_created_roundtrip() {
        roundtrip_agent(&AgentMessage::SessionCreated {
            session_id: Uuid::new_v4(),
            shell: "/bin/bash".to_string(),
            pid: 12345,
            tmux_name: None,
        });
        roundtrip_agent(&AgentMessage::SessionCreated {
            session_id: Uuid::new_v4(),
            shell: "/bin/zsh".to_string(),
            pid: 999,
            tmux_name: Some("zremote-abc123".to_string()),
        });
    }

    #[test]
    fn session_created_without_tmux_name_deserializes() {
        // Backward compat: older agents won't send tmux_name
        let json = r#"{"type":"SessionCreated","payload":{"session_id":"550e8400-e29b-41d4-a716-446655440000","shell":"/bin/bash","pid":12345}}"#;
        let msg: AgentMessage = serde_json::from_str(json).expect("should deserialize");
        if let AgentMessage::SessionCreated { tmux_name, .. } = msg {
            assert!(tmux_name.is_none(), "tmux_name should default to None");
        } else {
            panic!("expected SessionCreated variant");
        }
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
            has_zremote_config: false,
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
                    has_zremote_config: false,
                    project_type: "rust".to_string(),
                    git_info: None,
                    worktrees: vec![],
                },
                ProjectInfo {
                    path: "/home/user/project-b".to_string(),
                    name: "project-b".to_string(),
                    has_claude_config: false,
                    has_zremote_config: true,
                    project_type: "node".to_string(),
                    git_info: None,
                    worktrees: vec![],
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

    #[test]
    fn git_status_update_roundtrip() {
        use crate::project::{GitInfo, GitRemote, WorktreeInfo};
        roundtrip_agent(&AgentMessage::GitStatusUpdate {
            path: "/home/user/repo".to_string(),
            git_info: GitInfo {
                branch: Some("main".to_string()),
                commit_hash: Some("abc1234".to_string()),
                commit_message: Some("fix: bug".to_string()),
                is_dirty: false,
                ahead: 1,
                behind: 0,
                remotes: vec![GitRemote {
                    name: "origin".to_string(),
                    url: "https://github.com/user/repo.git".to_string(),
                }],
            },
            worktrees: vec![WorktreeInfo {
                path: "/home/user/repo-feat".to_string(),
                branch: Some("feature/x".to_string()),
                commit_hash: Some("def5678".to_string()),
                is_detached: false,
                is_locked: false,
                is_dirty: false,
                commit_message: None,
            }],
        });
    }

    #[test]
    fn worktree_created_roundtrip() {
        use crate::project::WorktreeInfo;
        roundtrip_agent(&AgentMessage::WorktreeCreated {
            project_path: "/home/user/repo".to_string(),
            worktree: WorktreeInfo {
                path: "/home/user/repo-feat".to_string(),
                branch: Some("feature/new".to_string()),
                commit_hash: Some("1234567".to_string()),
                is_detached: false,
                is_locked: false,
                is_dirty: false,
                commit_message: None,
            },
            hook_result: None,
        });
    }

    #[test]
    fn worktree_deleted_roundtrip() {
        roundtrip_agent(&AgentMessage::WorktreeDeleted {
            project_path: "/home/user/repo".to_string(),
            worktree_path: "/home/user/repo-feat".to_string(),
        });
    }

    #[test]
    fn worktree_error_roundtrip() {
        roundtrip_agent(&AgentMessage::WorktreeError {
            project_path: "/home/user/repo".to_string(),
            message: "branch already exists".to_string(),
        });
    }

    #[test]
    fn project_git_status_roundtrip() {
        roundtrip_server(&ServerMessage::ProjectGitStatus {
            path: "/home/user/repo".to_string(),
        });
    }

    #[test]
    fn worktree_create_roundtrip() {
        roundtrip_server(&ServerMessage::WorktreeCreate {
            project_path: "/home/user/repo".to_string(),
            branch: "feature/new".to_string(),
            path: Some("/home/user/repo-feature".to_string()),
            new_branch: true,
        });
        roundtrip_server(&ServerMessage::WorktreeCreate {
            project_path: "/home/user/repo".to_string(),
            branch: "existing-branch".to_string(),
            path: None,
            new_branch: false,
        });
    }

    #[test]
    fn worktree_delete_roundtrip() {
        roundtrip_server(&ServerMessage::WorktreeDelete {
            project_path: "/home/user/repo".to_string(),
            worktree_path: "/home/user/repo-feat".to_string(),
            force: false,
        });
        roundtrip_server(&ServerMessage::WorktreeDelete {
            project_path: "/home/user/repo".to_string(),
            worktree_path: "/home/user/repo-feat".to_string(),
            force: true,
        });
    }

    #[test]
    fn knowledge_agent_action_roundtrip() {
        use crate::knowledge::{KnowledgeAgentMessage, KnowledgeServiceStatus};
        roundtrip_agent(&AgentMessage::KnowledgeAction(
            KnowledgeAgentMessage::ServiceStatus {
                status: KnowledgeServiceStatus::Ready,
                version: Some("0.1.0".to_string()),
                error: None,
            },
        ));
    }

    #[test]
    fn knowledge_server_action_roundtrip() {
        use crate::knowledge::{KnowledgeServerMessage, ServiceAction};
        roundtrip_server(&ServerMessage::KnowledgeAction(
            KnowledgeServerMessage::ServiceControl {
                action: ServiceAction::Start,
            },
        ));
    }

    #[test]
    fn claude_agent_action_roundtrip() {
        use crate::claude::ClaudeAgentMessage;
        roundtrip_agent(&AgentMessage::ClaudeAction(
            ClaudeAgentMessage::SessionStarted {
                claude_task_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
            },
        ));
    }

    #[test]
    fn claude_agent_action_failed_roundtrip() {
        use crate::claude::ClaudeAgentMessage;
        roundtrip_agent(&AgentMessage::ClaudeAction(
            ClaudeAgentMessage::SessionStartFailed {
                claude_task_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                error: "spawn failed".to_string(),
            },
        ));
    }

    #[test]
    fn claude_agent_action_session_id_captured_roundtrip() {
        use crate::claude::ClaudeAgentMessage;
        roundtrip_agent(&AgentMessage::ClaudeAction(
            ClaudeAgentMessage::SessionIdCaptured {
                claude_task_id: Uuid::new_v4(),
                cc_session_id: "abc-session-123".to_string(),
            },
        ));
    }

    #[test]
    fn claude_agent_action_discovered_roundtrip() {
        use crate::claude::{ClaudeAgentMessage, ClaudeSessionInfo};
        roundtrip_agent(&AgentMessage::ClaudeAction(
            ClaudeAgentMessage::SessionsDiscovered {
                project_path: "/home/user/project".to_string(),
                sessions: vec![ClaudeSessionInfo {
                    session_id: "sess-1".to_string(),
                    project_path: "/home/user/project".to_string(),
                    model: Some("claude-sonnet-4-20250514".to_string()),
                    last_active: None,
                    message_count: None,
                    summary: None,
                }],
            },
        ));
    }

    #[test]
    fn claude_server_action_start_roundtrip() {
        use crate::claude::ClaudeServerMessage;
        roundtrip_server(&ServerMessage::ClaudeAction(
            ClaudeServerMessage::StartSession {
                session_id: Uuid::new_v4(),
                claude_task_id: Uuid::new_v4(),
                working_dir: "/home/user/project".to_string(),
                model: Some("claude-sonnet-4-20250514".to_string()),
                initial_prompt: Some("Fix the tests".to_string()),
                resume_cc_session_id: None,
                allowed_tools: vec!["Read".to_string()],
                skip_permissions: false,
                output_format: None,
                custom_flags: None,
                continue_last: false,
            },
        ));
    }

    #[test]
    fn claude_server_action_discover_roundtrip() {
        use crate::claude::ClaudeServerMessage;
        roundtrip_server(&ServerMessage::ClaudeAction(
            ClaudeServerMessage::DiscoverSessions {
                project_path: "/home/user/project".to_string(),
            },
        ));
    }

    #[test]
    fn list_directory_roundtrip() {
        roundtrip_server(&ServerMessage::ListDirectory {
            request_id: Uuid::new_v4(),
            path: "/home/user".to_string(),
        });
    }

    #[test]
    fn directory_listing_roundtrip() {
        use crate::project::DirectoryEntry;
        roundtrip_agent(&AgentMessage::DirectoryListing {
            request_id: Uuid::new_v4(),
            path: "/home/user".to_string(),
            entries: vec![
                DirectoryEntry {
                    name: "src".to_string(),
                    is_dir: true,
                    is_symlink: false,
                },
                DirectoryEntry {
                    name: "README.md".to_string(),
                    is_dir: false,
                    is_symlink: false,
                },
            ],
            error: None,
        });
    }

    #[test]
    fn directory_listing_with_error_roundtrip() {
        roundtrip_agent(&AgentMessage::DirectoryListing {
            request_id: Uuid::new_v4(),
            path: "/root".to_string(),
            entries: vec![],
            error: Some("permission denied".to_string()),
        });
    }

    #[test]
    fn project_get_settings_roundtrip() {
        roundtrip_server(&ServerMessage::ProjectGetSettings {
            request_id: Uuid::new_v4(),
            project_path: "/home/user/project".to_string(),
        });
    }

    #[test]
    fn project_save_settings_roundtrip() {
        use crate::project::{AgenticSettings, ProjectSettings};
        use std::collections::HashMap;
        roundtrip_server(&ServerMessage::ProjectSaveSettings {
            request_id: Uuid::new_v4(),
            project_path: "/home/user/project".to_string(),
            settings: Box::new(ProjectSettings {
                shell: Some("/bin/zsh".to_string()),
                working_dir: None,
                env: HashMap::from([("RUST_LOG".to_string(), "debug".to_string())]),
                agentic: AgenticSettings::default(),
                actions: vec![],
                worktree: None,
                linear: None,
                prompts: vec![],
                claude: None,
            }),
        });
    }

    #[test]
    fn project_settings_result_roundtrip() {
        use crate::project::{AgenticSettings, ProjectSettings};
        roundtrip_agent(&AgentMessage::ProjectSettingsResult {
            request_id: Uuid::new_v4(),
            settings: Some(Box::new(ProjectSettings {
                shell: Some("/bin/bash".to_string()),
                working_dir: None,
                env: std::collections::HashMap::new(),
                agentic: AgenticSettings::default(),
                actions: vec![],
                worktree: None,
                linear: None,
                prompts: vec![],
                claude: None,
            })),
            error: None,
        });
    }

    #[test]
    fn project_settings_result_none_roundtrip() {
        roundtrip_agent(&AgentMessage::ProjectSettingsResult {
            request_id: Uuid::new_v4(),
            settings: None,
            error: None,
        });
    }

    #[test]
    fn project_settings_result_error_roundtrip() {
        roundtrip_agent(&AgentMessage::ProjectSettingsResult {
            request_id: Uuid::new_v4(),
            settings: None,
            error: Some("file not readable".to_string()),
        });
    }

    #[test]
    fn project_settings_saved_roundtrip() {
        roundtrip_agent(&AgentMessage::ProjectSettingsSaved {
            request_id: Uuid::new_v4(),
            error: None,
        });
    }

    #[test]
    fn project_settings_saved_error_roundtrip() {
        roundtrip_agent(&AgentMessage::ProjectSettingsSaved {
            request_id: Uuid::new_v4(),
            error: Some("permission denied".to_string()),
        });
    }

    #[test]
    fn session_create_with_initial_command_roundtrip() {
        roundtrip_server(&ServerMessage::SessionCreate {
            session_id: Uuid::new_v4(),
            shell: Some("/bin/bash".to_string()),
            cols: 80,
            rows: 24,
            working_dir: Some("/home/user".to_string()),
            env: None,
            initial_command: Some("npm run dev".to_string()),
        });
    }

    #[test]
    fn session_create_backward_compat_no_initial_command() {
        let json = r#"{"type":"SessionCreate","payload":{"session_id":"550e8400-e29b-41d4-a716-446655440000","shell":"/bin/bash","cols":80,"rows":24,"working_dir":null,"env":null}}"#;
        let msg: ServerMessage = serde_json::from_str(json).expect("should deserialize");
        if let ServerMessage::SessionCreate {
            initial_command, ..
        } = msg
        {
            assert!(
                initial_command.is_none(),
                "initial_command should default to None"
            );
        } else {
            panic!("expected SessionCreate variant");
        }
    }

    #[test]
    fn worktree_hook_result_roundtrip() {
        roundtrip_agent(&AgentMessage::WorktreeHookResult {
            project_path: "/home/user/repo".to_string(),
            worktree_path: "/home/user/repo-feat".to_string(),
            hook_type: "on_create".to_string(),
            success: true,
            output: Some("npm install completed".to_string()),
            duration_ms: 3500,
        });
    }

    #[test]
    fn hook_result_info_roundtrip() {
        let info = HookResultInfo {
            success: true,
            output: Some("setup completed".to_string()),
            duration_ms: 1200,
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let parsed: HookResultInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, parsed);

        // Minimal (no output)
        let info_minimal = HookResultInfo {
            success: false,
            output: None,
            duration_ms: 50,
        };
        let json = serde_json::to_string(&info_minimal).expect("serialize");
        let parsed: HookResultInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info_minimal, parsed);
    }

    #[test]
    fn resolve_action_inputs_roundtrip() {
        roundtrip_server(&ServerMessage::ResolveActionInputs {
            request_id: Uuid::new_v4(),
            project_path: "/home/user/project".to_string(),
            action_name: "release".to_string(),
        });
    }

    #[test]
    fn action_inputs_resolved_roundtrip() {
        use crate::project::{ActionInputOption, ResolvedActionInput};
        roundtrip_agent(&AgentMessage::ActionInputsResolved {
            request_id: Uuid::new_v4(),
            inputs: vec![ResolvedActionInput {
                name: "tag".to_string(),
                options: vec![
                    ActionInputOption {
                        value: "0.2.4".to_string(),
                        label: Some("Patch".to_string()),
                    },
                    ActionInputOption {
                        value: "0.3.0".to_string(),
                        label: None,
                    },
                ],
                error: None,
            }],
            error: None,
        });
    }

    #[test]
    fn action_inputs_resolved_with_error_roundtrip() {
        roundtrip_agent(&AgentMessage::ActionInputsResolved {
            request_id: Uuid::new_v4(),
            inputs: vec![],
            error: Some("action not found".to_string()),
        });
    }

    #[test]
    fn worktree_created_with_hook_roundtrip() {
        use crate::project::WorktreeInfo;
        roundtrip_agent(&AgentMessage::WorktreeCreated {
            project_path: "/home/user/repo".to_string(),
            worktree: WorktreeInfo {
                path: "/home/user/repo-feat".to_string(),
                branch: Some("feature/new".to_string()),
                commit_hash: Some("1234567".to_string()),
                is_detached: false,
                is_locked: false,
                is_dirty: false,
                commit_message: Some("initial commit".to_string()),
            },
            hook_result: Some(HookResultInfo {
                success: true,
                output: Some("npm install done".to_string()),
                duration_ms: 2000,
            }),
        });
    }
}
