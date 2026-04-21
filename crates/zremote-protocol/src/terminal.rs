use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agents::{AgentLifecycleMessage, AgentServerMessage};
use crate::channel::{ChannelAgentAction, ChannelServerAction};
use crate::claude::{ClaudeAgentMessage, ClaudeServerMessage};
use crate::knowledge::{KnowledgeAgentMessage, KnowledgeServerMessage};
use crate::project::{
    DiffError, DiffFile, DiffFileSummary, DiffRequest, DiffSourceOptions, DirectoryEntry, GitInfo,
    ProjectInfo, ProjectSettings, ResolvedActionInput, SendReviewRequest, SendReviewResponse,
    WorktreeInfo,
};
use crate::{HostId, SessionId};

/// A recovered persistent session reported by the agent during reconnection.
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
        /// Whether this agent can serve git diff requests (RFC git-diff-ui).
        /// Older agents that predate the diff feature omit this field and
        /// the server treats them as unable to serve diffs.
        #[serde(default)]
        supports_diff: bool,
    },
    Heartbeat {
        timestamp: DateTime<Utc>,
    },
    TerminalOutput {
        session_id: SessionId,
        data: Vec<u8>,
    },
    /// Note: `tmux_name` field was removed in 0.7.6 (tmux backend removed).
    /// Old agents sending `tmux_name` are safely ignored (serde skips unknown fields).
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
        #[serde(default)]
        has_zremote_config: bool,
        project_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        main_repo_path: Option<String>,
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
    /// Lifecycle progress for an in-flight `WorktreeCreate` request. The
    /// server converts these into `ServerEvent::WorktreeCreationProgress`
    /// broadcasts so GUIs see stages as they happen. Older servers that
    /// predate this variant will ignore it (unknown message type).
    WorktreeCreationProgress {
        project_path: String,
        job_id: String,
        stage: crate::events::WorktreeCreationStage,
        #[serde(default)]
        percent: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
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
    ChannelAction(ChannelAgentAction),
    /// Generic agentic launcher lifecycle notifications (new in RFC-003).
    /// Older servers that predate agent profiles simply ignore unknown variants.
    AgentLifecycle(AgentLifecycleMessage),
    /// Summary of files in a streamed diff. Emitted once per `ProjectDiff`
    /// request, followed by one `DiffFileChunk` per file, then `DiffFinished`.
    DiffStarted {
        request_id: uuid::Uuid,
        files: Vec<DiffFileSummary>,
    },
    /// Single file's full diff payload. `file_index` is the index into
    /// `DiffStarted.files` so the client can pair chunks to summaries under
    /// streaming.
    DiffFileChunk {
        request_id: uuid::Uuid,
        file_index: u32,
        file: DiffFile,
    },
    /// Terminal event for a streamed diff. If `error` is `Some`, the whole
    /// op failed after `DiffStarted` — client should discard pending chunks.
    DiffFinished {
        request_id: uuid::Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<DiffError>,
    },
    /// Response to `ProjectDiffSources`.
    DiffSourcesResult {
        request_id: uuid::Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<Box<DiffSourceOptions>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<DiffError>,
    },
    /// Response to `ProjectSendReview`.
    SendReviewResult {
        request_id: uuid::Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<Box<SendReviewResponse>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<DiffError>,
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
    ChannelAction(ChannelServerAction),
    /// Generic agentic launcher spawn requests (new in RFC-003). Delivered to
    /// the agent where `LauncherRegistry::get(kind)` dispatches to the right
    /// launcher. Older agents that predate this variant will ignore it.
    AgentAction(AgentServerMessage),
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
        /// Optional base ref (commit SHA, branch, or tag) to create the new
        /// branch from. Only meaningful when `new_branch` is `true`. Older
        /// servers that predate this field will deserialize their outbound
        /// message with `base_ref: None` and fall back to HEAD.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_ref: Option<String>,
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
    /// Push context (memories + conventions) to a running agent session.
    /// The agent-side `DeliveryCoordinator` handles delivery timing.
    ContextPush {
        session_id: SessionId,
        #[serde(default)]
        memories: Vec<String>,
        #[serde(default)]
        conventions: Vec<String>,
    },
    /// Request a streaming diff from the agent. Agent replies with
    /// `DiffStarted` + `DiffFileChunk` * N + `DiffFinished`.
    ProjectDiff {
        request_id: uuid::Uuid,
        request: DiffRequest,
    },
    /// Request diff-source metadata (branches + recent commits + dirty
    /// state) for the source picker. Agent replies with `DiffSourcesResult`.
    ProjectDiffSources {
        request_id: uuid::Uuid,
        project_path: String,
        /// Cap on the number of recent commits returned.
        #[serde(default)]
        max_commits: Option<u32>,
    },
    /// Ship a review (rendered markdown) to a target session. Agent replies
    /// with `SendReviewResult`.
    ProjectSendReview {
        request_id: uuid::Uuid,
        request: SendReviewRequest,
    },
    /// Cancel an in-flight `ProjectDiff` identified by `request_id`. The
    /// agent checks the token between files and aborts with
    /// `DiffFinished { error: Some(Timeout) }`.
    DiffCancel {
        request_id: uuid::Uuid,
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
            supports_diff: false,
        });
        roundtrip_agent(&AgentMessage::Register {
            hostname: "dev-machine".to_string(),
            agent_version: "0.1.0".to_string(),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            token: "secret-token".to_string(),
            supports_persistent_sessions: true,
            supports_diff: true,
        });
    }

    #[test]
    fn register_without_persistent_sessions_deserializes() {
        // Backward compat: older agents won't send supports_persistent_sessions
        let json = r#"{"type":"Register","payload":{"hostname":"h","agent_version":"0.1","os":"linux","arch":"x86_64","token":"t"}}"#;
        let msg: AgentMessage = serde_json::from_str(json).expect("should deserialize");
        if let AgentMessage::Register {
            supports_persistent_sessions,
            supports_diff,
            ..
        } = msg
        {
            assert!(!supports_persistent_sessions, "should default to false");
            assert!(!supports_diff, "supports_diff should default to false");
        } else {
            panic!("expected Register variant");
        }
    }

    #[test]
    fn register_without_supports_diff_deserializes() {
        // An agent that knows supports_persistent_sessions but predates
        // supports_diff must still deserialise cleanly.
        let json = r#"{"type":"Register","payload":{"hostname":"h","agent_version":"0.1","os":"linux","arch":"x86_64","token":"t","supports_persistent_sessions":true}}"#;
        let msg: AgentMessage = serde_json::from_str(json).expect("should deserialize");
        if let AgentMessage::Register {
            supports_persistent_sessions,
            supports_diff,
            ..
        } = msg
        {
            assert!(supports_persistent_sessions);
            assert!(!supports_diff);
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
        });
        roundtrip_agent(&AgentMessage::SessionCreated {
            session_id: Uuid::new_v4(),
            shell: "/bin/zsh".to_string(),
            pid: 999,
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
            has_zremote_config: false,
            project_type: "rust".to_string(),
            main_repo_path: None,
        });
        roundtrip_agent(&AgentMessage::ProjectDiscovered {
            path: "/home/user/myproject-wt".to_string(),
            name: "myproject-wt".to_string(),
            has_claude_config: false,
            has_zremote_config: false,
            project_type: "worktree".to_string(),
            main_repo_path: Some("/home/user/myproject".to_string()),
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
                    frameworks: vec![],
                    architecture: None,
                    conventions: vec![],
                    package_manager: None,
                    main_repo_path: None,
                },
                ProjectInfo {
                    path: "/home/user/project-b".to_string(),
                    name: "project-b".to_string(),
                    has_claude_config: false,
                    has_zremote_config: true,
                    project_type: "node".to_string(),
                    git_info: None,
                    worktrees: vec![],
                    frameworks: vec![],
                    architecture: None,
                    conventions: vec![],
                    package_manager: None,
                    main_repo_path: None,
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
    fn worktree_creation_progress_roundtrip() {
        use crate::events::WorktreeCreationStage;
        roundtrip_agent(&AgentMessage::WorktreeCreationProgress {
            project_path: "/home/user/repo".to_string(),
            job_id: "job-1".to_string(),
            stage: WorktreeCreationStage::Creating,
            percent: 25,
            message: Some("running git worktree add".to_string()),
        });
        roundtrip_agent(&AgentMessage::WorktreeCreationProgress {
            project_path: "/home/user/repo".to_string(),
            job_id: "job-1".to_string(),
            stage: WorktreeCreationStage::Done,
            percent: 100,
            message: None,
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
            base_ref: Some("main".to_string()),
        });
        roundtrip_server(&ServerMessage::WorktreeCreate {
            project_path: "/home/user/repo".to_string(),
            branch: "existing-branch".to_string(),
            path: None,
            new_branch: false,
            base_ref: None,
        });
    }

    #[test]
    fn worktree_create_accepts_missing_base_ref_for_backcompat() {
        // Older servers (pre-Phase 2) send WorktreeCreate without base_ref.
        // The new agent must still accept those messages and default to None.
        let json = r#"{"type":"WorktreeCreate","payload":{"project_path":"/r","branch":"b","path":null,"new_branch":true}}"#;
        let msg: ServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            ServerMessage::WorktreeCreate { base_ref, .. } => {
                assert!(base_ref.is_none(), "missing base_ref must default to None");
            }
            other => panic!("expected WorktreeCreate, got {other:?}"),
        }
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
                development_channels: vec![],
                print_mode: false,
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
                hooks: None,
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
                hooks: None,
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

    #[test]
    fn context_push_roundtrip() {
        roundtrip_server(&ServerMessage::ContextPush {
            session_id: Uuid::new_v4(),
            memories: vec!["memory one".to_string(), "memory two".to_string()],
            conventions: vec!["use snake_case".to_string()],
        });
    }

    #[test]
    fn context_push_empty_roundtrip() {
        roundtrip_server(&ServerMessage::ContextPush {
            session_id: Uuid::new_v4(),
            memories: vec![],
            conventions: vec![],
        });
    }

    #[test]
    fn channel_agent_action_worker_response_roundtrip() {
        use crate::channel::{ChannelAgentAction, ChannelResponse, WorkerStatus};
        roundtrip_agent(&AgentMessage::ChannelAction(
            ChannelAgentAction::WorkerResponse {
                session_id: Uuid::new_v4(),
                response: ChannelResponse::StatusReport {
                    status: WorkerStatus::Completed,
                    summary: "All tests pass".to_string(),
                },
            },
        ));
    }

    #[test]
    fn channel_agent_action_permission_request_roundtrip() {
        use crate::channel::ChannelAgentAction;
        roundtrip_agent(&AgentMessage::ChannelAction(
            ChannelAgentAction::PermissionRequest {
                session_id: Uuid::new_v4(),
                request_id: "req-001".to_string(),
                tool_name: "Bash".to_string(),
                tool_input: serde_json::json!({"command": "rm -rf /tmp/test"}),
            },
        ));
    }

    #[test]
    fn channel_agent_action_status_roundtrip() {
        use crate::channel::ChannelAgentAction;
        roundtrip_agent(&AgentMessage::ChannelAction(
            ChannelAgentAction::ChannelStatus {
                session_id: Uuid::new_v4(),
                available: true,
            },
        ));
    }

    #[test]
    fn channel_server_action_send_roundtrip() {
        use crate::channel::{ChannelMessage, ChannelServerAction, Priority};
        roundtrip_server(&ServerMessage::ChannelAction(
            ChannelServerAction::ChannelSend {
                session_id: Uuid::new_v4(),
                message: ChannelMessage::Instruction {
                    from: "commander".to_string(),
                    content: "Fix tests".to_string(),
                    priority: Priority::High,
                },
            },
        ));
    }

    #[test]
    fn channel_server_action_permission_response_roundtrip() {
        use crate::channel::ChannelServerAction;
        roundtrip_server(&ServerMessage::ChannelAction(
            ChannelServerAction::PermissionResponse {
                session_id: Uuid::new_v4(),
                request_id: "perm-001".to_string(),
                allowed: true,
                reason: None,
            },
        ));
    }

    #[test]
    fn agent_action_start_agent_roundtrip() {
        use crate::agents::{AgentProfileData, AgentServerMessage};
        use std::collections::BTreeMap;

        let mut env = BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        roundtrip_server(&ServerMessage::AgentAction(
            AgentServerMessage::StartAgent {
                session_id: Uuid::new_v4().to_string(),
                task_id: Uuid::new_v4().to_string(),
                host_id: Uuid::new_v4().to_string(),
                project_path: "/home/user/project".to_string(),
                profile: AgentProfileData {
                    id: Uuid::new_v4().to_string(),
                    agent_kind: "claude".to_string(),
                    name: "Default".to_string(),
                    description: Some("default profile".to_string()),
                    model: Some("sonnet-4-5".to_string()),
                    initial_prompt: Some("Go!".to_string()),
                    skip_permissions: false,
                    allowed_tools: vec!["Read".to_string()],
                    extra_args: vec!["--verbose".to_string()],
                    env_vars: env,
                    settings_json: serde_json::json!({"print_mode": false}),
                },
            },
        ));
    }

    #[test]
    fn agent_lifecycle_started_roundtrip() {
        use crate::agents::AgentLifecycleMessage;
        roundtrip_agent(&AgentMessage::AgentLifecycle(
            AgentLifecycleMessage::Started {
                session_id: Uuid::new_v4().to_string(),
                task_id: Uuid::new_v4().to_string(),
                agent_kind: "claude".to_string(),
            },
        ));
    }

    #[test]
    fn agent_lifecycle_start_failed_roundtrip() {
        use crate::agents::AgentLifecycleMessage;
        roundtrip_agent(&AgentMessage::AgentLifecycle(
            AgentLifecycleMessage::StartFailed {
                session_id: Uuid::new_v4().to_string(),
                task_id: Uuid::new_v4().to_string(),
                agent_kind: "claude".to_string(),
                error: "spawn failed".to_string(),
            },
        ));
    }

    #[test]
    fn project_diff_server_message_roundtrip() {
        use crate::project::{DiffRequest, DiffSource};
        roundtrip_server(&ServerMessage::ProjectDiff {
            request_id: Uuid::new_v4(),
            request: DiffRequest {
                project_id: "proj-1".to_string(),
                source: DiffSource::WorkingTree,
                file_paths: None,
                context_lines: 3,
            },
        });
    }

    #[test]
    fn project_diff_sources_server_message_roundtrip() {
        roundtrip_server(&ServerMessage::ProjectDiffSources {
            request_id: Uuid::new_v4(),
            project_path: "/home/user/repo".to_string(),
            max_commits: Some(50),
        });
        roundtrip_server(&ServerMessage::ProjectDiffSources {
            request_id: Uuid::new_v4(),
            project_path: "/home/user/repo".to_string(),
            max_commits: None,
        });
    }

    #[test]
    fn project_diff_sources_without_max_commits_deserializes() {
        let id = Uuid::new_v4();
        let raw = format!(
            r#"{{"type":"ProjectDiffSources","payload":{{"request_id":"{id}","project_path":"/home/user/repo"}}}}"#
        );
        let msg: ServerMessage = serde_json::from_str(&raw).unwrap();
        match msg {
            ServerMessage::ProjectDiffSources {
                request_id,
                project_path,
                max_commits,
            } => {
                assert_eq!(request_id, id);
                assert_eq!(project_path, "/home/user/repo");
                assert!(max_commits.is_none());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn project_send_review_server_message_roundtrip() {
        use crate::project::{DiffSource, ReviewDelivery, SendReviewRequest};
        roundtrip_server(&ServerMessage::ProjectSendReview {
            request_id: Uuid::new_v4(),
            request: SendReviewRequest {
                project_id: "proj-1".to_string(),
                source: DiffSource::WorkingTree,
                comments: vec![],
                delivery: ReviewDelivery::InjectSession,
                session_id: Some(Uuid::new_v4()),
                preamble: None,
            },
        });
    }

    #[test]
    fn diff_cancel_server_message_roundtrip() {
        roundtrip_server(&ServerMessage::DiffCancel {
            request_id: Uuid::new_v4(),
        });
    }

    #[test]
    fn diff_started_agent_message_roundtrip() {
        use crate::project::{DiffFileStatus, DiffFileSummary};
        roundtrip_agent(&AgentMessage::DiffStarted {
            request_id: Uuid::new_v4(),
            files: vec![DiffFileSummary {
                path: "src/a.rs".to_string(),
                old_path: None,
                status: DiffFileStatus::Modified,
                binary: false,
                submodule: false,
                too_large: false,
                additions: 3,
                deletions: 1,
                old_sha: Some("aaa".to_string()),
                new_sha: Some("bbb".to_string()),
                old_mode: None,
                new_mode: None,
            }],
        });
    }

    #[test]
    fn diff_file_chunk_agent_message_roundtrip() {
        use crate::project::{
            DiffFile, DiffFileStatus, DiffFileSummary, DiffHunk, DiffLine, DiffLineKind,
        };
        roundtrip_agent(&AgentMessage::DiffFileChunk {
            request_id: Uuid::new_v4(),
            file_index: 0,
            file: DiffFile {
                summary: DiffFileSummary {
                    path: "src/a.rs".to_string(),
                    old_path: None,
                    status: DiffFileStatus::Modified,
                    binary: false,
                    submodule: false,
                    too_large: false,
                    additions: 1,
                    deletions: 1,
                    old_sha: None,
                    new_sha: None,
                    old_mode: None,
                    new_mode: None,
                },
                hunks: vec![DiffHunk {
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 1,
                    header: "@@ -1 +1 @@".to_string(),
                    lines: vec![
                        DiffLine {
                            kind: DiffLineKind::Removed,
                            old_lineno: Some(1),
                            new_lineno: None,
                            content: "old".to_string(),
                        },
                        DiffLine {
                            kind: DiffLineKind::Added,
                            old_lineno: None,
                            new_lineno: Some(1),
                            content: "new".to_string(),
                        },
                    ],
                }],
            },
        });
    }

    #[test]
    fn diff_finished_agent_message_roundtrip() {
        use crate::project::{DiffError, DiffErrorCode};
        roundtrip_agent(&AgentMessage::DiffFinished {
            request_id: Uuid::new_v4(),
            error: None,
        });
        roundtrip_agent(&AgentMessage::DiffFinished {
            request_id: Uuid::new_v4(),
            error: Some(DiffError {
                code: DiffErrorCode::Timeout,
                message: "timed out".to_string(),
                hint: None,
            }),
        });
    }

    #[test]
    fn diff_sources_result_agent_message_roundtrip() {
        use crate::project::{BranchList, DiffError, DiffErrorCode, DiffSourceOptions};
        roundtrip_agent(&AgentMessage::DiffSourcesResult {
            request_id: Uuid::new_v4(),
            options: Some(Box::new(DiffSourceOptions {
                has_working_tree_changes: true,
                has_staged_changes: false,
                branches: BranchList {
                    local: vec![],
                    remote: vec![],
                    current: "main".to_string(),
                    remote_truncated: false,
                },
                recent_commits: vec![],
                head_sha: Some("deadbeef".to_string()),
                head_short_sha: Some("deadbee".to_string()),
            })),
            error: None,
        });
        roundtrip_agent(&AgentMessage::DiffSourcesResult {
            request_id: Uuid::new_v4(),
            options: None,
            error: Some(DiffError {
                code: DiffErrorCode::NotGitRepo,
                message: "not a git repo".to_string(),
                hint: None,
            }),
        });
    }

    #[test]
    fn send_review_result_agent_message_roundtrip() {
        use crate::project::{DiffError, DiffErrorCode, SendReviewResponse};
        roundtrip_agent(&AgentMessage::SendReviewResult {
            request_id: Uuid::new_v4(),
            response: Some(Box::new(SendReviewResponse {
                session_id: Uuid::new_v4(),
                delivered: 2,
            })),
            error: None,
        });
        roundtrip_agent(&AgentMessage::SendReviewResult {
            request_id: Uuid::new_v4(),
            response: None,
            error: Some(DiffError {
                code: DiffErrorCode::Other,
                message: "session not found".to_string(),
                hint: None,
            }),
        });
    }
}
