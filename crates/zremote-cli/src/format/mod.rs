//! Output formatting for CLI commands.

mod json;
mod plain;
mod table;

use clap::ValueEnum;
use zremote_client::types::ProjectSettings;
use zremote_client::{
    AgenticLoop, ClaudeTask, ConfigValue, DirectoryEntry, Host, KnowledgeBase, Memory, ModeInfo,
    Project, ProjectAction, SearchResult, ServerEvent, Session, WorktreeInfo,
};

use crate::GlobalOpts;

/// Output format selection.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Plain,
}

/// Trait for formatting CLI output.
pub trait Formatter {
    fn hosts(&self, hosts: &[Host]) -> String;
    fn host(&self, host: &Host) -> String;
    fn sessions(&self, sessions: &[Session]) -> String;
    fn session(&self, session: &Session) -> String;
    fn projects(&self, projects: &[Project]) -> String;
    fn project(&self, project: &Project) -> String;
    fn loops(&self, loops: &[AgenticLoop]) -> String;
    fn agentic_loop(&self, l: &AgenticLoop) -> String;
    fn tasks(&self, tasks: &[ClaudeTask]) -> String;
    fn task(&self, task: &ClaudeTask) -> String;
    fn memories(&self, memories: &[Memory]) -> String;
    fn memory(&self, memory: &Memory) -> String;
    fn config_value(&self, cv: &ConfigValue) -> String;
    fn settings(&self, settings: &ProjectSettings) -> String;
    fn actions(&self, actions: &[ProjectAction]) -> String;
    fn worktrees(&self, worktrees: &[WorktreeInfo]) -> String;
    fn knowledge_status(&self, kb: &KnowledgeBase) -> String;
    fn search_results(&self, results: &SearchResult) -> String;
    fn status_info(&self, mode: &ModeInfo, hosts: &[Host]) -> String;
    fn event(&self, event: &ServerEvent) -> String;
    fn directory_entries(&self, entries: &[DirectoryEntry]) -> String;
}

/// Create a formatter based on global options.
///
/// Auto-detects piped output: if stdout is not a TTY and --output wasn't
/// explicitly set, defaults to plain format.
pub fn create_formatter(opts: &GlobalOpts) -> Box<dyn Formatter> {
    let format = if !atty_stdout() && matches!(opts.output, OutputFormat::Table) {
        // Auto-switch to plain when piped (unless explicitly set)
        OutputFormat::Plain
    } else {
        opts.output
    };

    match format {
        OutputFormat::Table => Box::new(table::TableFormatter),
        OutputFormat::Json => Box::new(json::JsonFormatter),
        OutputFormat::Plain => Box::new(plain::PlainFormatter),
    }
}

fn atty_stdout() -> bool {
    crossterm::tty::IsTty::is_tty(&std::io::stdout())
}

/// Truncate a string to a maximum length, appending "..." if truncated.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max > 3 {
        format!("{}...", &s[..max - 3])
    } else {
        s[..max].to_string()
    }
}

/// Format an optional string, showing "-" for None.
pub fn opt(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("-")
}

/// Shorten a UUID to first 8 characters.
pub fn short_id(id: &str) -> &str {
    if id.len() >= 8 { &id[..8] } else { id }
}

/// Format a relative time from an ISO 8601 timestamp.
pub fn relative_time(ts: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return ts.to_string();
    };
    let now = chrono::Utc::now();
    let diff = now.signed_duration_since(dt);

    if diff.num_seconds() < 60 {
        format!("{}s ago", diff.num_seconds())
    } else if diff.num_minutes() < 60 {
        format!("{}m ago", diff.num_minutes())
    } else if diff.num_hours() < 24 {
        format!("{}h ago", diff.num_hours())
    } else {
        format!("{}d ago", diff.num_days())
    }
}
