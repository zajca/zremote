//! `ZRemote` client SDK — HTTP/WebSocket client for the `ZRemote` platform.
//!
//! Provides:
//! - [`ApiClient`] — REST API client for all endpoints
//! - [`EventStream`] — WebSocket event stream with auto-reconnect
//! - [`TerminalSession`] — WebSocket terminal session with binary frame support

mod client;
mod error;
mod events;
mod terminal;
pub mod types;

pub use client::{ApiClient, extract_base_url};
pub use error::ApiError;
pub use events::{ClientEvent, EventStream};
pub use terminal::TerminalSession;

/// Re-export flume for channel consumers.
pub use flume;

// Re-export commonly used types at crate root
pub use types::{
    ActionsResponse,
    AddProjectRequest,
    AgentKindInfo,
    AgentProfile,
    // SDK types
    AgenticLoop,
    // Protocol re-exports
    AgenticLoopId,
    AgenticStatus,
    ClaudeSessionInfo,
    ClaudeTask,
    ClaudeTaskStatus,
    ConfigValue,
    CreateAgentProfileRequest,
    CreateClaudeTaskRequest,
    CreateSessionRequest,
    CreateSessionResponse,
    CreateWorktreeRequest,
    DirectoryEntry,
    // Constants
    EVENT_CHANNEL_CAPACITY,
    ExtractedMemory,
    GitInfo,
    GitRemote,
    Host,
    HostId,
    HostInfo,
    HostStatus,
    IMAGE_PASTE_CHANNEL_CAPACITY,
    KnowledgeBase,
    KnowledgeServiceStatus,
    ListClaudeTasksFilter,
    ListLoopsFilter,
    LoopInfo,
    LoopInfoLite,
    Memory,
    MemoryCategory,
    ModeInfo,
    PreviewColorSpan,
    PreviewLine,
    PreviewSnapshot,
    Project,
    ProjectAction,
    ProjectInfo,
    ProjectSettings,
    RESIZE_CHANNEL_CAPACITY,
    ResumeClaudeTaskRequest,
    SearchResult,
    SearchTier,
    ServerEvent,
    Session,
    SessionId,
    SessionInfo,
    SessionStatus,
    StartAgentRequest,
    StartAgentResponse,
    TERMINAL_CHANNEL_CAPACITY,
    TerminalEvent,
    TerminalInput,
    UpdateAgentProfileRequest,
    UpdateProjectRequest,
    WorktreeInfo,
};
