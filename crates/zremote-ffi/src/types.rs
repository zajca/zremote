// ---------------------------------------------------------------------------
// FFI enums
// ---------------------------------------------------------------------------

/// Agentic loop status.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiAgenticStatus {
    Working,
    WaitingForInput,
    Error,
    Completed,
    Unknown,
}

impl From<zremote_client::AgenticStatus> for FfiAgenticStatus {
    fn from(s: zremote_client::AgenticStatus) -> Self {
        match s {
            zremote_client::AgenticStatus::Working => Self::Working,
            zremote_client::AgenticStatus::WaitingForInput => Self::WaitingForInput,
            zremote_client::AgenticStatus::Error => Self::Error,
            zremote_client::AgenticStatus::Completed => Self::Completed,
            zremote_client::AgenticStatus::Unknown => Self::Unknown,
        }
    }
}

/// Claude task status.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiClaudeTaskStatus {
    Starting,
    Active,
    Completed,
    Error,
}

impl From<zremote_client::ClaudeTaskStatus> for FfiClaudeTaskStatus {
    fn from(s: zremote_client::ClaudeTaskStatus) -> Self {
        match s {
            zremote_client::ClaudeTaskStatus::Starting => Self::Starting,
            zremote_client::ClaudeTaskStatus::Active => Self::Active,
            zremote_client::ClaudeTaskStatus::Completed => Self::Completed,
            zremote_client::ClaudeTaskStatus::Error => Self::Error,
        }
    }
}

/// Knowledge service status.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiKnowledgeServiceStatus {
    Starting,
    Ready,
    Indexing,
    Error,
    Stopped,
}

impl From<zremote_client::KnowledgeServiceStatus> for FfiKnowledgeServiceStatus {
    fn from(s: zremote_client::KnowledgeServiceStatus) -> Self {
        match s {
            zremote_client::KnowledgeServiceStatus::Starting => Self::Starting,
            zremote_client::KnowledgeServiceStatus::Ready => Self::Ready,
            zremote_client::KnowledgeServiceStatus::Indexing => Self::Indexing,
            zremote_client::KnowledgeServiceStatus::Error => Self::Error,
            zremote_client::KnowledgeServiceStatus::Stopped => Self::Stopped,
        }
    }
}

/// Memory category.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiMemoryCategory {
    Pattern,
    Decision,
    Pitfall,
    Preference,
    Architecture,
    Convention,
}

impl From<zremote_client::MemoryCategory> for FfiMemoryCategory {
    fn from(c: zremote_client::MemoryCategory) -> Self {
        match c {
            zremote_client::MemoryCategory::Pattern => Self::Pattern,
            zremote_client::MemoryCategory::Decision => Self::Decision,
            zremote_client::MemoryCategory::Pitfall => Self::Pitfall,
            zremote_client::MemoryCategory::Preference => Self::Preference,
            zremote_client::MemoryCategory::Architecture => Self::Architecture,
            zremote_client::MemoryCategory::Convention => Self::Convention,
        }
    }
}

impl From<FfiMemoryCategory> for zremote_client::MemoryCategory {
    fn from(c: FfiMemoryCategory) -> Self {
        match c {
            FfiMemoryCategory::Pattern => Self::Pattern,
            FfiMemoryCategory::Decision => Self::Decision,
            FfiMemoryCategory::Pitfall => Self::Pitfall,
            FfiMemoryCategory::Preference => Self::Preference,
            FfiMemoryCategory::Architecture => Self::Architecture,
            FfiMemoryCategory::Convention => Self::Convention,
        }
    }
}

/// Knowledge search tier.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiSearchTier {
    L0,
    L1,
    L2,
}

impl From<zremote_client::SearchTier> for FfiSearchTier {
    fn from(t: zremote_client::SearchTier) -> Self {
        match t {
            zremote_client::SearchTier::L0 => Self::L0,
            zremote_client::SearchTier::L1 => Self::L1,
            zremote_client::SearchTier::L2 => Self::L2,
        }
    }
}

impl From<FfiSearchTier> for zremote_client::SearchTier {
    fn from(t: FfiSearchTier) -> Self {
        match t {
            FfiSearchTier::L0 => Self::L0,
            FfiSearchTier::L1 => Self::L1,
            FfiSearchTier::L2 => Self::L2,
        }
    }
}

// ---------------------------------------------------------------------------
// FFI response records
// ---------------------------------------------------------------------------

/// Host as returned by the API.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiHost {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub status: String,
    pub last_seen_at: Option<String>,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<zremote_client::types::Host> for FfiHost {
    fn from(h: zremote_client::types::Host) -> Self {
        Self {
            id: h.id,
            name: h.name,
            hostname: h.hostname,
            status: h.status,
            last_seen_at: h.last_seen_at,
            agent_version: h.agent_version,
            os: h.os,
            arch: h.arch,
            created_at: h.created_at,
            updated_at: h.updated_at,
        }
    }
}

/// Session creation response.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCreateSessionResponse {
    pub id: String,
    pub status: String,
}

impl From<zremote_client::types::CreateSessionResponse> for FfiCreateSessionResponse {
    fn from(r: zremote_client::types::CreateSessionResponse) -> Self {
        Self {
            id: r.id,
            status: r.status,
        }
    }
}

/// Terminal session.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSession {
    pub id: String,
    pub host_id: String,
    pub name: Option<String>,
    pub shell: Option<String>,
    pub status: String,
    pub working_dir: Option<String>,
    pub project_id: Option<String>,
    pub pid: Option<i64>,
    pub exit_code: Option<i32>,
    pub created_at: String,
    pub closed_at: Option<String>,
}

impl From<zremote_client::types::Session> for FfiSession {
    fn from(s: zremote_client::types::Session) -> Self {
        Self {
            id: s.id,
            host_id: s.host_id,
            name: s.name,
            shell: s.shell,
            status: s.status,
            working_dir: s.working_dir,
            project_id: s.project_id,
            pid: s.pid,
            exit_code: s.exit_code,
            created_at: s.created_at,
            closed_at: s.closed_at,
        }
    }
}

/// Project.
#[derive(Debug, Clone, uniffi::Record)]
#[allow(clippy::struct_excessive_bools)] // Mirrors SDK type with multiple boolean fields
pub struct FfiProject {
    pub id: String,
    pub host_id: String,
    pub path: String,
    pub name: String,
    pub has_claude_config: bool,
    pub has_zremote_config: bool,
    pub project_type: String,
    pub created_at: String,
    pub parent_project_id: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit_hash: Option<String>,
    pub git_commit_message: Option<String>,
    pub git_is_dirty: bool,
    pub git_ahead: i32,
    pub git_behind: i32,
    pub git_remotes: Option<String>,
    pub git_updated_at: Option<String>,
    pub pinned: bool,
}

impl From<zremote_client::types::Project> for FfiProject {
    fn from(p: zremote_client::types::Project) -> Self {
        Self {
            id: p.id,
            host_id: p.host_id,
            path: p.path,
            name: p.name,
            has_claude_config: p.has_claude_config,
            has_zremote_config: p.has_zremote_config,
            project_type: p.project_type,
            created_at: p.created_at,
            parent_project_id: p.parent_project_id,
            git_branch: p.git_branch,
            git_commit_hash: p.git_commit_hash,
            git_commit_message: p.git_commit_message,
            git_is_dirty: p.git_is_dirty,
            git_ahead: p.git_ahead,
            git_behind: p.git_behind,
            git_remotes: p.git_remotes,
            git_updated_at: p.git_updated_at,
            pinned: p.pinned,
        }
    }
}

/// Agentic loop.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAgenticLoop {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub status: FfiAgenticStatus,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
    pub task_name: Option<String>,
}

impl From<zremote_client::types::AgenticLoop> for FfiAgenticLoop {
    fn from(l: zremote_client::types::AgenticLoop) -> Self {
        Self {
            id: l.id,
            session_id: l.session_id,
            project_path: l.project_path,
            tool_name: l.tool_name,
            status: l.status.into(),
            started_at: l.started_at,
            ended_at: l.ended_at,
            end_reason: l.end_reason,
            task_name: l.task_name,
        }
    }
}

/// Config key-value pair.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiConfigValue {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

impl From<zremote_client::types::ConfigValue> for FfiConfigValue {
    fn from(c: zremote_client::types::ConfigValue) -> Self {
        Self {
            key: c.key,
            value: c.value,
            updated_at: c.updated_at,
        }
    }
}

/// Server mode info.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiModeInfo {
    pub mode: String,
    pub version: Option<String>,
}

impl From<zremote_client::types::ModeInfo> for FfiModeInfo {
    fn from(m: zremote_client::types::ModeInfo) -> Self {
        Self {
            mode: m.mode,
            version: m.version,
        }
    }
}

/// Knowledge base status.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiKnowledgeBase {
    pub id: String,
    pub host_id: String,
    pub status: FfiKnowledgeServiceStatus,
    pub openviking_version: Option<String>,
    pub last_error: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: String,
}

impl From<zremote_client::types::KnowledgeBase> for FfiKnowledgeBase {
    fn from(k: zremote_client::types::KnowledgeBase) -> Self {
        Self {
            id: k.id,
            host_id: k.host_id,
            status: k.status.into(),
            openviking_version: k.openviking_version,
            last_error: k.last_error,
            started_at: k.started_at,
            updated_at: k.updated_at,
        }
    }
}

/// Knowledge memory entry.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMemory {
    pub id: String,
    pub project_id: String,
    pub loop_id: Option<String>,
    pub key: String,
    pub content: String,
    pub category: FfiMemoryCategory,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
}

impl From<zremote_client::types::Memory> for FfiMemory {
    fn from(m: zremote_client::types::Memory) -> Self {
        Self {
            id: m.id,
            project_id: m.project_id,
            loop_id: m.loop_id,
            key: m.key,
            content: m.content,
            category: m.category.into(),
            confidence: m.confidence,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// Claude task.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiClaudeTask {
    pub id: String,
    pub session_id: String,
    pub host_id: String,
    pub project_path: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub claude_session_id: Option<String>,
    pub resume_from: Option<String>,
    pub status: FfiClaudeTaskStatus,
    pub options_json: Option<String>,
    pub loop_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub total_tokens_in: Option<i64>,
    pub total_tokens_out: Option<i64>,
    pub summary: Option<String>,
    pub task_name: Option<String>,
    pub created_at: String,
}

impl From<zremote_client::types::ClaudeTask> for FfiClaudeTask {
    fn from(t: zremote_client::types::ClaudeTask) -> Self {
        Self {
            id: t.id,
            session_id: t.session_id,
            host_id: t.host_id,
            project_path: t.project_path,
            project_id: t.project_id,
            model: t.model,
            initial_prompt: t.initial_prompt,
            claude_session_id: t.claude_session_id,
            resume_from: t.resume_from,
            status: t.status.into(),
            options_json: t.options_json,
            loop_id: t.loop_id,
            started_at: t.started_at,
            ended_at: t.ended_at,
            total_cost_usd: t.total_cost_usd,
            total_tokens_in: t.total_tokens_in,
            total_tokens_out: t.total_tokens_out,
            summary: t.summary,
            task_name: t.task_name,
            created_at: t.created_at,
        }
    }
}

/// Directory entry.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDirectoryEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
}

impl From<zremote_client::DirectoryEntry> for FfiDirectoryEntry {
    fn from(d: zremote_client::DirectoryEntry) -> Self {
        Self {
            name: d.name,
            is_dir: d.is_dir,
            is_symlink: d.is_symlink,
        }
    }
}

/// Worktree info.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiWorktreeInfo {
    pub path: String,
    pub branch: Option<String>,
    pub commit_hash: Option<String>,
    pub is_detached: bool,
    pub is_locked: bool,
    pub is_dirty: bool,
    pub commit_message: Option<String>,
}

impl From<zremote_client::WorktreeInfo> for FfiWorktreeInfo {
    fn from(w: zremote_client::WorktreeInfo) -> Self {
        Self {
            path: w.path,
            branch: w.branch,
            commit_hash: w.commit_hash,
            is_detached: w.is_detached,
            is_locked: w.is_locked,
            is_dirty: w.is_dirty,
            commit_message: w.commit_message,
        }
    }
}

/// Knowledge search result.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSearchResult {
    pub path: String,
    pub score: f64,
    pub snippet: String,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub tier: FfiSearchTier,
}

impl From<zremote_client::SearchResult> for FfiSearchResult {
    fn from(r: zremote_client::SearchResult) -> Self {
        Self {
            path: r.path,
            score: r.score,
            snippet: r.snippet,
            line_start: r.line_start,
            line_end: r.line_end,
            tier: r.tier.into(),
        }
    }
}

/// Extracted memory from transcript.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiExtractedMemory {
    pub key: String,
    pub content: String,
    pub category: FfiMemoryCategory,
    pub confidence: f64,
    pub source_loop_id: String,
}

impl From<zremote_client::ExtractedMemory> for FfiExtractedMemory {
    fn from(m: zremote_client::ExtractedMemory) -> Self {
        Self {
            key: m.key,
            content: m.content,
            category: m.category.into(),
            confidence: m.confidence,
            source_loop_id: m.source_loop_id.to_string(),
        }
    }
}

/// Claude session info (for resume/discover).
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiClaudeSessionInfo {
    pub session_id: String,
    pub project_path: String,
    pub model: Option<String>,
    pub last_active: Option<String>,
    pub message_count: Option<u32>,
    pub summary: Option<String>,
}

impl From<zremote_client::ClaudeSessionInfo> for FfiClaudeSessionInfo {
    fn from(s: zremote_client::ClaudeSessionInfo) -> Self {
        Self {
            session_id: s.session_id,
            project_path: s.project_path,
            model: s.model,
            last_active: s.last_active,
            message_count: s.message_count,
            summary: s.summary,
        }
    }
}

// ---------------------------------------------------------------------------
// Event info records (used in EventListener callbacks)
// ---------------------------------------------------------------------------

/// Loop info in server events.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLoopInfo {
    pub id: String,
    pub session_id: String,
    pub project_path: Option<String>,
    pub tool_name: String,
    pub status: FfiAgenticStatus,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
    pub task_name: Option<String>,
}

impl From<zremote_client::types::LoopInfo> for FfiLoopInfo {
    fn from(l: zremote_client::types::LoopInfo) -> Self {
        Self {
            id: l.id,
            session_id: l.session_id,
            project_path: l.project_path,
            tool_name: l.tool_name,
            status: l.status.into(),
            started_at: l.started_at,
            ended_at: l.ended_at,
            end_reason: l.end_reason,
            task_name: l.task_name,
        }
    }
}

/// Host info in server events.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiHostInfo {
    pub id: String,
    pub hostname: String,
    pub status: String,
    pub agent_version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
}

impl From<zremote_client::types::HostInfo> for FfiHostInfo {
    fn from(h: zremote_client::types::HostInfo) -> Self {
        Self {
            id: h.id,
            hostname: h.hostname,
            status: h.status,
            agent_version: h.agent_version,
            os: h.os,
            arch: h.arch,
        }
    }
}

/// Claude session metrics from event stream.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiClaudeSessionMetrics {
    pub session_id: String,
    pub model: Option<String>,
    pub context_used_pct: Option<f64>,
    pub context_window_size: Option<u64>,
    pub cost_usd: Option<f64>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub lines_added: Option<i64>,
    pub lines_removed: Option<i64>,
    pub rate_limit_5h_pct: Option<u64>,
    pub rate_limit_7d_pct: Option<u64>,
}

/// Session info in server events.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSessionInfo {
    pub id: String,
    pub host_id: String,
    pub shell: Option<String>,
    pub status: String,
}

impl From<zremote_client::types::SessionInfo> for FfiSessionInfo {
    fn from(s: zremote_client::types::SessionInfo) -> Self {
        Self {
            id: s.id,
            host_id: s.host_id,
            shell: s.shell,
            status: s.status,
        }
    }
}

// ---------------------------------------------------------------------------
// FFI request records
// ---------------------------------------------------------------------------

/// Request to create a new terminal session.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCreateSessionRequest {
    pub name: Option<String>,
    pub shell: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub working_dir: Option<String>,
}

impl From<FfiCreateSessionRequest> for zremote_client::CreateSessionRequest {
    fn from(r: FfiCreateSessionRequest) -> Self {
        Self {
            name: r.name,
            shell: r.shell,
            cols: r.cols,
            rows: r.rows,
            working_dir: r.working_dir,
        }
    }
}

/// Request to update a host name.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiUpdateHostRequest {
    pub name: String,
}

/// Request to update a project.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiUpdateProjectRequest {
    pub pinned: Option<bool>,
}

/// Request to add a project.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiAddProjectRequest {
    pub path: String,
}

/// Request to create a worktree.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCreateWorktreeRequest {
    pub branch: String,
    pub path: Option<String>,
    pub new_branch: bool,
}

/// Filter for listing agentic loops.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiListLoopsFilter {
    pub status: Option<String>,
    pub host_id: Option<String>,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
}

impl From<FfiListLoopsFilter> for zremote_client::ListLoopsFilter {
    fn from(f: FfiListLoopsFilter) -> Self {
        Self {
            status: f.status,
            host_id: f.host_id,
            session_id: f.session_id,
            project_id: f.project_id,
        }
    }
}

/// Filter for listing Claude tasks.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiListClaudeTasksFilter {
    pub host_id: Option<String>,
    pub status: Option<String>,
    pub project_id: Option<String>,
}

impl From<FfiListClaudeTasksFilter> for zremote_client::ListClaudeTasksFilter {
    fn from(f: FfiListClaudeTasksFilter) -> Self {
        Self {
            host_id: f.host_id,
            status: f.status,
            project_id: f.project_id,
        }
    }
}

/// Request to create a Claude task.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiCreateClaudeTaskRequest {
    pub host_id: String,
    pub project_path: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub initial_prompt: Option<String>,
    pub allowed_tools: Vec<String>,
    pub skip_permissions: Option<bool>,
    pub output_format: Option<String>,
    pub custom_flags: Option<String>,
}

impl From<FfiCreateClaudeTaskRequest> for zremote_client::CreateClaudeTaskRequest {
    fn from(r: FfiCreateClaudeTaskRequest) -> Self {
        Self {
            host_id: r.host_id,
            project_path: r.project_path,
            project_id: r.project_id,
            model: r.model,
            initial_prompt: r.initial_prompt,
            allowed_tools: if r.allowed_tools.is_empty() {
                None
            } else {
                Some(r.allowed_tools)
            },
            skip_permissions: r.skip_permissions,
            output_format: r.output_format,
            custom_flags: r.custom_flags,
        }
    }
}

/// Request to search knowledge.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSearchRequest {
    pub query: String,
    pub tier: Option<FfiSearchTier>,
    pub max_results: Option<u32>,
}

impl From<FfiSearchRequest> for zremote_client::types::SearchRequest {
    fn from(r: FfiSearchRequest) -> Self {
        Self {
            query: r.query,
            tier: r.tier.map(Into::into),
            max_results: r.max_results,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agentic_status_maps_each_variant() {
        assert!(matches!(
            FfiAgenticStatus::from(zremote_client::AgenticStatus::Working),
            FfiAgenticStatus::Working
        ));
        assert!(matches!(
            FfiAgenticStatus::from(zremote_client::AgenticStatus::WaitingForInput),
            FfiAgenticStatus::WaitingForInput
        ));
        assert!(matches!(
            FfiAgenticStatus::from(zremote_client::AgenticStatus::Error),
            FfiAgenticStatus::Error
        ));
        assert!(matches!(
            FfiAgenticStatus::from(zremote_client::AgenticStatus::Completed),
            FfiAgenticStatus::Completed
        ));
        assert!(matches!(
            FfiAgenticStatus::from(zremote_client::AgenticStatus::Unknown),
            FfiAgenticStatus::Unknown
        ));
    }

    #[test]
    fn memory_category_roundtrip() {
        let cat = zremote_client::MemoryCategory::Architecture;
        let ffi: FfiMemoryCategory = cat.into();
        let back: zremote_client::MemoryCategory = ffi.into();
        assert!(matches!(back, zremote_client::MemoryCategory::Architecture));
    }

    #[test]
    fn search_tier_roundtrip() {
        let tier = zremote_client::SearchTier::L2;
        let ffi: FfiSearchTier = tier.into();
        let back: zremote_client::SearchTier = ffi.into();
        assert!(matches!(back, zremote_client::SearchTier::L2));
    }

    #[test]
    fn create_claude_task_empty_tools() {
        let ffi = FfiCreateClaudeTaskRequest {
            host_id: "h1".to_string(),
            project_path: "/tmp".to_string(),
            project_id: None,
            model: None,
            initial_prompt: Some("hello".to_string()),
            allowed_tools: vec![],
            skip_permissions: None,
            output_format: None,
            custom_flags: None,
        };
        let sdk: zremote_client::CreateClaudeTaskRequest = ffi.into();
        assert!(sdk.allowed_tools.is_none());
    }

    #[test]
    fn create_claude_task_with_tools() {
        let ffi = FfiCreateClaudeTaskRequest {
            host_id: "h1".to_string(),
            project_path: "/tmp".to_string(),
            project_id: None,
            model: None,
            initial_prompt: None,
            allowed_tools: vec!["bash".to_string(), "read".to_string()],
            skip_permissions: None,
            output_format: None,
            custom_flags: None,
        };
        let sdk: zremote_client::CreateClaudeTaskRequest = ffi.into();
        assert_eq!(sdk.allowed_tools.unwrap().len(), 2);
    }
}
