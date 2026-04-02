use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::sync::mpsc;
use zremote_protocol::SessionId;
use zremote_protocol::knowledge::MemoryCategory;

use crate::agentic::adapters::AgentInfo;

/// Maximum number of concurrent inotify watchers allowed.
const MAX_ACTIVE_WATCHERS: usize = 100;

/// Default timeout for inotify confirmation.
const DEFAULT_INOTIFY_TIMEOUT: Duration = Duration::from_secs(5);

/// Global counter of active inotify watchers.
static ACTIVE_WATCHERS: AtomicUsize = AtomicUsize::new(0);

/// RAII guard that decrements `ACTIVE_WATCHERS` on drop, preventing leaks
/// when a Future is cancelled between watcher setup and teardown.
struct WatcherGuard;

impl WatcherGuard {
    fn acquire() -> Option<Self> {
        let prev = ACTIVE_WATCHERS.fetch_add(1, Ordering::Relaxed);
        if prev >= MAX_ACTIVE_WATCHERS {
            ACTIVE_WATCHERS.fetch_sub(1, Ordering::Relaxed);
            None
        } else {
            Some(Self)
        }
    }
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        ACTIVE_WATCHERS.fetch_sub(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum confidence threshold for including memories in context delivery.
/// Higher than the 0.6 used for CLAUDE.md generation since mid-session
/// injection is more disruptive.
const MIN_DELIVERY_CONFIDENCE: f64 = 0.7;

/// Default maximum token budget for context payloads.
const DEFAULT_MAX_TOKENS: usize = 4096;

/// Default maximum age for deferred nudges before they are dropped.
const DEFAULT_MAX_NUDGE_AGE: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Content type hint for differentiated token estimation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    /// Mostly code -- uses ~3 chars/token ratio.
    Code,
    /// Mostly natural language -- uses ~4.5 chars/token ratio.
    NaturalLanguage,
    /// Mixed content (default) -- uses ~3.5 chars/token ratio.
    Mixed,
}

impl ContentType {
    /// Return the chars-per-token ratio for this content type.
    fn ratio(self) -> f32 {
        match self {
            Self::Code => 3.0,
            Self::NaturalLanguage => 4.5,
            Self::Mixed => 3.5,
        }
    }
}

/// What triggered this context assembly.
#[derive(Debug, Clone)]
pub enum ContextTrigger {
    /// New memories were extracted from a completed loop.
    MemoryExtracted { loop_id: uuid::Uuid, count: usize },
    /// Project conventions were updated.
    ConventionsUpdated { project_path: String },
    /// Manual trigger from server/GUI.
    ManualPush,
}

impl std::fmt::Display for ContextTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MemoryExtracted { count, .. } => {
                write!(f, "{count} new memories extracted")
            }
            Self::ConventionsUpdated { project_path } => {
                write!(f, "conventions updated for {project_path}")
            }
            Self::ManualPush => write!(f, "manual push"),
        }
    }
}

/// Minimal project summary for context injection.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub name: String,
    pub path: String,
    pub project_type: String,
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub git_branch: Option<String>,
}

/// A memory entry included in context delivery.
#[derive(Debug, Clone)]
pub struct ContextMemory {
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub confidence: f64,
}

/// Assembled context ready for delivery to a running agent session.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub project: ProjectSummary,
    pub memories: Vec<ContextMemory>,
    pub conventions: Vec<String>,
    pub trigger: ContextTrigger,
    pub estimated_tokens: usize,
    pub content_type: ContentType,
}

impl SessionContext {
    /// Render the context to markdown format for injection.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# ZRemote Context Update");
        let _ = writeln!(out);
        let _ = writeln!(out, "## Project: {}", self.project.name);
        let _ = writeln!(out, "- Path: {}", self.project.path);
        let _ = writeln!(out, "- Type: {}", self.project.project_type);
        if let Some(ref branch) = self.project.git_branch {
            let _ = writeln!(out, "- Branch: {branch}");
        }
        if !self.project.languages.is_empty() {
            let _ = writeln!(out, "- Languages: {}", self.project.languages.join(", "));
        }
        if !self.project.frameworks.is_empty() {
            let _ = writeln!(out, "- Frameworks: {}", self.project.frameworks.join(", "));
        }

        if !self.memories.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "## Recent Memories");
            for mem in &self.memories {
                let _ = writeln!(out, "### [{:?}] {}", mem.category, mem.key);
                let _ = writeln!(out, "{}", mem.content);
                let _ = writeln!(out);
            }
        }

        if !self.conventions.is_empty() {
            let _ = writeln!(out, "## Conventions");
            for conv in &self.conventions {
                let _ = writeln!(out, "- {conv}");
            }
            let _ = writeln!(out);
        }

        let _ = writeln!(out, "---");
        let _ = writeln!(out, "Trigger: {}", self.trigger);

        out
    }
}

// ---------------------------------------------------------------------------
// Token budget
// ---------------------------------------------------------------------------

/// Token budget configuration and estimation.
pub struct TokenBudget {
    /// Maximum tokens for the entire context payload.
    pub max_tokens: usize,
    /// Override chars-per-token ratio. If None, derived from ContentType.
    pub chars_per_token_override: Option<f32>,
}

impl TokenBudget {
    /// Create a new token budget with default settings.
    pub fn new() -> Self {
        Self {
            max_tokens: DEFAULT_MAX_TOKENS,
            chars_per_token_override: None,
        }
    }

    /// Estimate token count for a string using the given content type.
    #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
    pub fn estimate_tokens(text: &str, content_type: ContentType) -> usize {
        if text.is_empty() {
            return 0;
        }
        (text.len() as f32 / content_type.ratio()).ceil() as usize
    }

    /// Estimate with explicit ratio override.
    #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)]
    pub fn estimate_tokens_with_ratio(text: &str, chars_per_token: f32) -> usize {
        if text.is_empty() || chars_per_token <= 0.0 {
            return 0;
        }
        (text.len() as f32 / chars_per_token).ceil() as usize
    }

    /// Get the effective ratio for a content type, respecting the override.
    fn effective_ratio(&self, content_type: ContentType) -> f32 {
        self.chars_per_token_override
            .unwrap_or(content_type.ratio())
    }

    /// Estimate tokens for a context using this budget's settings.
    fn estimate_context_tokens(&self, rendered: &str, content_type: ContentType) -> usize {
        let ratio = self.effective_ratio(content_type);
        Self::estimate_tokens_with_ratio(rendered, ratio)
    }

    /// Trim context to fit within budget, returning a new trimmed copy.
    /// Priority order (lowest trimmed first):
    /// 1. Drop conventions (lowest impact)
    /// 2. Drop memories by ascending confidence
    /// 3. Clear all content as a last resort
    ///
    /// The original `SessionContext` is not modified.
    pub fn trim(&self, context: &SessionContext) -> SessionContext {
        let mut trimmed = context.clone();

        // Check if already within budget
        let rendered = trimmed.render();
        if self.estimate_context_tokens(&rendered, trimmed.content_type) <= self.max_tokens {
            return trimmed;
        }

        // Step 1: Drop conventions from the end
        while !trimmed.conventions.is_empty() {
            trimmed.conventions.pop();
            let rendered = trimmed.render();
            if self.estimate_context_tokens(&rendered, trimmed.content_type) <= self.max_tokens {
                trimmed.estimated_tokens =
                    self.estimate_context_tokens(&trimmed.render(), trimmed.content_type);
                return trimmed;
            }
        }

        // Step 2: Drop memories by ascending confidence (lowest first)
        trimmed.memories.sort_by(|a, b| {
            a.confidence
                .partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        while !trimmed.memories.is_empty() {
            trimmed.memories.remove(0);
            let rendered = trimmed.render();
            if self.estimate_context_tokens(&rendered, trimmed.content_type) <= self.max_tokens {
                // Re-sort by descending confidence for rendering
                trimmed.memories.sort_by(|a, b| {
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                trimmed.estimated_tokens =
                    self.estimate_context_tokens(&trimmed.render(), trimmed.content_type);
                return trimmed;
            }
        }

        // Step 3: As a last resort, clear all content to minimize the payload.
        trimmed.memories.clear();
        trimmed.conventions.clear();
        trimmed.estimated_tokens =
            self.estimate_context_tokens(&trimmed.render(), trimmed.content_type);
        trimmed
    }
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Provider injection strategy
// ---------------------------------------------------------------------------

/// Per-provider injection strategy. The Output Analyzer detects the provider
/// via `AgentInfo::name`, which maps to the appropriate strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderInjectionStrategy {
    /// Claude Code: write temp file, inject `/read <path>`.
    ClaudeCode,
    /// Aider: write temp file, inject `/add <path>`.
    Aider,
    /// Direct paste: paste content directly into PTY stdin with delimiters.
    DirectPaste,
}

impl ProviderInjectionStrategy {
    /// Determine injection strategy from detected agent info.
    /// Falls back to `DirectPaste` for unknown providers.
    /// Uses case-insensitive matching to handle variations in agent names
    /// (e.g. "Claude Code", "claude-code", "Aider v0.50", "aider").
    pub fn from_agent_info(agent: Option<&AgentInfo>) -> Self {
        let Some(agent) = agent else {
            return Self::DirectPaste;
        };
        let name_lower = agent.name.to_lowercase();
        if name_lower.contains("claude") {
            Self::ClaudeCode
        } else if name_lower.contains("aider") {
            Self::Aider
        } else {
            Self::DirectPaste
        }
    }

    /// Return the command format string for file-based injection.
    fn file_command(self, path: &str) -> Option<String> {
        match self {
            Self::ClaudeCode => Some(format!("/read {path}\n")),
            Self::Aider => Some(format!("/add {path}\n")),
            Self::DirectPaste => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Context transport
// ---------------------------------------------------------------------------

/// Result of a delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryStatus {
    /// Content was delivered and confirmed.
    Delivered,
    /// Content was delivered and file access was confirmed via inotify.
    Confirmed,
    /// Content was sent but confirmation could not be verified.
    Unconfirmed,
    /// Delivery failed permanently.
    Failed(String),
}

/// Errors during context delivery.
#[derive(Debug)]
pub enum DeliveryError {
    /// Session not found or PTY closed.
    SessionNotFound(SessionId),
    /// Failed to write temp file.
    IoError(std::io::Error),
    /// Channel send failed.
    ChannelClosed,
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionNotFound(id) => write!(f, "session {id} not found"),
            Self::IoError(e) => write!(f, "I/O error: {e}"),
            Self::ChannelClosed => write!(f, "write channel closed"),
        }
    }
}

impl std::error::Error for DeliveryError {}

/// Transport for delivering context to a running agent session.
pub trait ContextTransport: Send + Sync {
    /// Deliver context content to a session.
    fn deliver(
        &self,
        session_id: &SessionId,
        content: &str,
    ) -> impl std::future::Future<Output = Result<DeliveryStatus, DeliveryError>> + Send;
}

// ---------------------------------------------------------------------------
// Session writer handle
// ---------------------------------------------------------------------------

/// A write request for a PTY session, sent through a channel.
#[derive(Debug)]
pub struct SessionWriteRequest {
    pub session_id: SessionId,
    pub data: Vec<u8>,
}

/// A handle for writing to PTY sessions from the delivery coordinator.
#[derive(Clone)]
pub struct SessionWriterHandle {
    tx: mpsc::Sender<SessionWriteRequest>,
}

impl SessionWriterHandle {
    pub fn new(tx: mpsc::Sender<SessionWriteRequest>) -> Self {
        Self { tx }
    }

    /// Send a write request to the session.
    pub async fn write(&self, session_id: SessionId, data: Vec<u8>) -> Result<(), DeliveryError> {
        self.tx
            .send(SessionWriteRequest { session_id, data })
            .await
            .map_err(|_| DeliveryError::ChannelClosed)
    }
}

// ---------------------------------------------------------------------------
// PTY transport
// ---------------------------------------------------------------------------

/// PTY-based context delivery backend.
/// Writes context to temp files and injects provider-appropriate commands.
/// Uses `tempfile::TempDir` for automatic cleanup on drop.
pub struct PtyTransport {
    session_writer: SessionWriterHandle,
    /// Managed temp directory -- files are cleaned up when the transport is dropped.
    _temp_dir: TempDir,
    /// Path to the temp directory (kept separately since `TempDir::path()` borrows).
    temp_path: PathBuf,
}

impl PtyTransport {
    pub fn new(session_writer: SessionWriterHandle) -> Result<Self, std::io::Error> {
        let temp_dir = TempDir::with_prefix("zremote-context-")?;
        let temp_path = temp_dir.path().to_path_buf();
        Ok(Self {
            session_writer,
            _temp_dir: temp_dir,
            temp_path,
        })
    }

    /// Render content and inject via the appropriate strategy.
    pub async fn deliver_with_strategy(
        &self,
        session_id: &SessionId,
        content: &str,
        strategy: ProviderInjectionStrategy,
    ) -> Result<DeliveryStatus, DeliveryError> {
        match strategy {
            ProviderInjectionStrategy::ClaudeCode | ProviderInjectionStrategy::Aider => {
                self.deliver_via_file(session_id, content, strategy).await
            }
            ProviderInjectionStrategy::DirectPaste => {
                self.deliver_direct_paste(session_id, content).await
            }
        }
    }

    /// File-based delivery: write temp file, inject `/read` or `/add` command.
    /// Uses inotify to confirm file access by the target process.
    async fn deliver_via_file(
        &self,
        session_id: &SessionId,
        content: &str,
        strategy: ProviderInjectionStrategy,
    ) -> Result<DeliveryStatus, DeliveryError> {
        self.deliver_via_file_with_timeout(session_id, content, strategy, DEFAULT_INOTIFY_TIMEOUT)
            .await
    }

    /// Inner implementation with configurable timeout (for testing).
    async fn deliver_via_file_with_timeout(
        &self,
        session_id: &SessionId,
        content: &str,
        strategy: ProviderInjectionStrategy,
        inotify_timeout: Duration,
    ) -> Result<DeliveryStatus, DeliveryError> {
        let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
        let filename = format!("zremote-context-{session_id}-{timestamp}.md");
        let file_path = self.temp_path.join(&filename);

        // Write temp file first (directory already exists via TempDir).
        // The watcher is set up AFTER the write so our own fs::write() does
        // not trigger a spurious confirmation event.
        tokio::fs::write(&file_path, content)
            .await
            .map_err(DeliveryError::IoError)?;

        // Set up inotify watcher after writing, before sending the PTY command.
        // Any Access event from here on comes from the agent reading the file.
        // WatcherGuard ensures ACTIVE_WATCHERS is decremented even if the
        // Future is cancelled (e.g. by tokio::select! or timeout).
        let (watcher_rx, _guard) = match WatcherGuard::acquire() {
            None => {
                tracing::debug!("inotify watcher limit reached, skipping confirmation");
                (None, None)
            }
            Some(guard) => match setup_file_watcher(&file_path) {
                Ok(rx) => (Some(rx), Some(guard)),
                Err(e) => {
                    tracing::debug!("failed to set up inotify watcher: {e}, continuing without");
                    (None, None)
                }
            },
        };

        // Inject the command — after watcher is armed so we catch immediate reads.
        let path_str = file_path.to_string_lossy().to_string();
        if let Some(cmd) = strategy.file_command(&path_str) {
            self.session_writer
                .write(*session_id, cmd.into_bytes())
                .await?;
        }

        // Wait for inotify confirmation if watcher was set up.
        let Some(rx) = watcher_rx else {
            return Ok(DeliveryStatus::Unconfirmed);
        };

        let status =
            match tokio::task::spawn_blocking(move || rx.recv_timeout(inotify_timeout)).await {
                Ok(Ok(())) => DeliveryStatus::Confirmed,
                _ => DeliveryStatus::Unconfirmed,
            };

        // _guard is dropped here, decrementing ACTIVE_WATCHERS
        Ok(status)
    }

    /// Direct paste delivery: inject content with delimiters.
    async fn deliver_direct_paste(
        &self,
        session_id: &SessionId,
        content: &str,
    ) -> Result<DeliveryStatus, DeliveryError> {
        let payload = format!("--- ZRemote Context ---\n{content}\n--- End ZRemote Context ---\n");
        self.session_writer
            .write(*session_id, payload.into_bytes())
            .await?;
        Ok(DeliveryStatus::Unconfirmed)
    }
}

impl ContextTransport for PtyTransport {
    async fn deliver(
        &self,
        session_id: &SessionId,
        content: &str,
    ) -> Result<DeliveryStatus, DeliveryError> {
        // Default to DirectPaste when called via the trait (no agent info available).
        // Use `deliver_with_strategy` for provider-aware delivery.
        self.deliver_direct_paste(session_id, content).await
    }
}

// ---------------------------------------------------------------------------
// Inotify helper
// ---------------------------------------------------------------------------

/// Set up a file watcher that signals when the file is accessed or removed.
fn setup_file_watcher(
    path: &std::path::Path,
) -> Result<std::sync::mpsc::Receiver<()>, notify::Error> {
    use notify::{RecursiveMode, Watcher};

    let (tx, rx) = std::sync::mpsc::channel();
    let watched_path = path.to_path_buf();

    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let matches = event.paths.iter().any(|p| p == &watched_path);
                if matches {
                    use notify::EventKind;
                    // Confirm on any Access (agent opened/read the file) or
                    // Remove (agent deleted it after reading). The file is
                    // written before the watcher is armed, so we never see
                    // the write's own Close(Write) event here.
                    match event.kind {
                        EventKind::Access(_) | EventKind::Remove(_) => {
                            let _ = tx.send(());
                        }
                        _ => {}
                    }
                }
            }
        })?;

    let watch_target = path.parent().unwrap_or(path);
    watcher.watch(watch_target, RecursiveMode::NonRecursive)?;

    // Keep watcher alive on a background thread for at most the inotify
    // timeout duration (5s). Previous implementation used 30s which was
    // wasteful. The thread parks for the timeout then drops the watcher.
    std::thread::spawn(move || {
        let _watcher = watcher;
        std::thread::park_timeout(DEFAULT_INOTIFY_TIMEOUT);
    });

    Ok(rx)
}

// ---------------------------------------------------------------------------
// Context assembler
// ---------------------------------------------------------------------------

/// Assembles `SessionContext` from project data and memories.
pub struct ContextAssembler;

impl ContextAssembler {
    /// Assemble context for a session from available data sources.
    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        project_name: &str,
        project_path: &str,
        project_type: &str,
        git_branch: Option<&str>,
        frameworks: &[String],
        memories: &[ContextMemoryInput],
        conventions: &[String],
        trigger: ContextTrigger,
    ) -> SessionContext {
        let project_summary = ProjectSummary {
            name: project_name.to_string(),
            path: project_path.to_string(),
            project_type: project_type.to_string(),
            languages: Vec::new(),
            frameworks: frameworks.to_vec(),
            git_branch: git_branch.map(String::from),
        };

        let mut context_memories: Vec<ContextMemory> = memories
            .iter()
            .filter(|m| m.confidence >= MIN_DELIVERY_CONFIDENCE)
            .map(|m| ContextMemory {
                key: m.key.clone(),
                content: m.content.clone(),
                category: m.category,
                confidence: m.confidence,
            })
            .collect();

        // Sort by confidence descending
        context_memories.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut context = SessionContext {
            project: project_summary,
            memories: context_memories,
            conventions: conventions.to_vec(),
            trigger,
            estimated_tokens: 0,
            content_type: ContentType::Mixed,
        };

        context.estimated_tokens =
            TokenBudget::estimate_tokens(&context.render(), context.content_type);
        context
    }
}

/// Input memory data for assembly (decoupled from DB row type).
#[derive(Debug, Clone)]
pub struct ContextMemoryInput {
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub confidence: f64,
}

// ---------------------------------------------------------------------------
// Nudge accumulator (with 2-second debounce)
// ---------------------------------------------------------------------------

/// Debounce duration: triggers must settle for this long before delivery.
const NUDGE_DEBOUNCE: Duration = Duration::from_secs(2);

/// Nudge accumulator with debounce: stores the latest trigger per session
/// and requires a 2-second settle period before the trigger is considered ready.
/// Each new `push()` resets the debounce timer.
pub struct NudgeAccumulator {
    pending: Option<ContextTrigger>,
    last_push: Option<Instant>,
}

impl NudgeAccumulator {
    pub fn new() -> Self {
        Self {
            pending: None,
            last_push: None,
        }
    }

    /// Record a new trigger, replacing any existing pending one and resetting
    /// the debounce timer.
    pub fn push(&mut self, trigger: ContextTrigger) {
        self.pending = Some(trigger);
        self.last_push = Some(Instant::now());
    }

    /// Take the pending trigger, if any.
    pub fn take(&mut self) -> Option<ContextTrigger> {
        let trigger = self.pending.take();
        if trigger.is_some() {
            self.last_push = None;
        }
        trigger
    }

    /// Whether there is a pending trigger.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Whether the pending trigger has settled past the debounce window
    /// (2 seconds since last push).
    pub fn is_ready(&self) -> bool {
        match (&self.pending, &self.last_push) {
            (Some(_), Some(t)) => t.elapsed() >= NUDGE_DEBOUNCE,
            _ => false,
        }
    }
}

impl Default for NudgeAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Deferred nudge
// ---------------------------------------------------------------------------

/// A nudge that was deferred because the agent was busy.
#[derive(Debug)]
pub struct DeferredNudge {
    /// The assembled context to deliver.
    pub context: SessionContext,
    /// When this nudge was created.
    pub created_at: Instant,
    /// Number of times delivery was deferred.
    pub defer_count: u32,
}

impl DeferredNudge {
    pub fn new(context: SessionContext) -> Self {
        Self {
            context,
            created_at: Instant::now(),
            defer_count: 0,
        }
    }

    /// Whether this nudge has expired.
    pub fn is_expired(&self, max_age: Duration) -> bool {
        self.created_at.elapsed() > max_age
    }
}

// ---------------------------------------------------------------------------
// Delivery coordinator
// ---------------------------------------------------------------------------

/// Coordinates context delivery timing based on agent phase.
pub struct DeliveryCoordinator {
    /// Per-session deferred nudges.
    pending: HashMap<SessionId, DeferredNudge>,
    /// Per-session nudge accumulators.
    accumulators: HashMap<SessionId, NudgeAccumulator>,
    /// Token budget configuration.
    budget: TokenBudget,
    /// Maximum age for deferred nudges.
    max_nudge_age: Duration,
}

impl DeliveryCoordinator {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            accumulators: HashMap::new(),
            budget: TokenBudget::new(),
            max_nudge_age: DEFAULT_MAX_NUDGE_AGE,
        }
    }

    /// Record a context change for a session. If the agent is not idle,
    /// the context is deferred as a pending nudge.
    pub fn on_context_changed(&mut self, session_id: SessionId, context: SessionContext) {
        let trimmed = self.budget.trim(&context);

        // Replace any existing pending nudge
        self.pending.insert(session_id, DeferredNudge::new(trimmed));
    }

    /// Called when a session transitions to Idle or `NeedsInput`.
    /// Returns the rendered content string ready for PTY injection,
    /// or `None` if there is no pending nudge, it has expired, or the
    /// accumulator debounce has not yet settled.
    pub fn on_phase_idle(&mut self, session_id: &SessionId) -> Option<String> {
        // Check accumulator debounce: if there is a pending accumulator trigger
        // that hasn't settled, skip delivery this cycle.
        if self
            .accumulators
            .get(session_id)
            .is_some_and(|acc| acc.has_pending() && !acc.is_ready())
        {
            return None;
        }

        let nudge = self.pending.remove(session_id)?;
        if nudge.is_expired(self.max_nudge_age) {
            tracing::debug!(
                session = %session_id,
                age_secs = nudge.created_at.elapsed().as_secs(),
                "dropping expired nudge"
            );
            return None;
        }
        Some(nudge.context.render())
    }

    /// Check if there is a pending nudge for a session.
    pub fn has_pending(&self, session_id: &SessionId) -> bool {
        self.pending.contains_key(session_id)
    }

    /// Remove all state for a session (cleanup on close).
    pub fn remove_session(&mut self, session_id: &SessionId) {
        self.pending.remove(session_id);
        self.accumulators.remove(session_id);
    }

    /// Get mutable reference to the accumulator for a session.
    pub fn accumulator(&mut self, session_id: &SessionId) -> &mut NudgeAccumulator {
        self.accumulators.entry(*session_id).or_default()
    }
}

impl Default for DeliveryCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Context change event (for KnowledgeManager -> connection loop integration)
// ---------------------------------------------------------------------------

/// Event emitted by `KnowledgeManager` when new context is available.
/// The connection loop (or local mode PTY output loop) listens for these
/// and feeds them into the `DeliveryCoordinator`.
#[derive(Debug, Clone)]
pub enum ContextChangeEvent {
    /// New memories were extracted for a loop.
    /// The connection loop resolves `loop_id` to a `session_id` via
    /// `AgenticLoopManager::session_id_for_loop()`.
    MemoriesExtracted {
        loop_id: uuid::Uuid,
        memories: Vec<ContextMemoryInput>,
        project_path: String,
    },
    /// A watched project file changed on disk (e.g. CLAUDE.md, package.json).
    ProjectFileChanged {
        project_path: String,
        changed_file: String,
    },
}

/// Parse a category string into a `MemoryCategory`, falling back to `Convention`.
pub fn parse_category(s: &str) -> MemoryCategory {
    match s {
        "pattern" => MemoryCategory::Pattern,
        "decision" => MemoryCategory::Decision,
        "pitfall" => MemoryCategory::Pitfall,
        "preference" => MemoryCategory::Preference,
        "architecture" => MemoryCategory::Architecture,
        "convention" => MemoryCategory::Convention,
        _ => MemoryCategory::Convention,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_project_summary() -> ProjectSummary {
        ProjectSummary {
            name: "test-project".to_string(),
            path: "/home/user/project".to_string(),
            project_type: "rust".to_string(),
            languages: vec!["Rust".to_string()],
            frameworks: vec!["Axum".to_string()],
            git_branch: Some("main".to_string()),
        }
    }

    fn make_memory(key: &str, confidence: f64) -> ContextMemory {
        ContextMemory {
            key: key.to_string(),
            content: format!("Memory content for {key}"),
            category: MemoryCategory::Pattern,
            confidence,
        }
    }

    fn make_context(memories: Vec<ContextMemory>, conventions: Vec<String>) -> SessionContext {
        SessionContext {
            project: make_project_summary(),
            memories,
            conventions,
            trigger: ContextTrigger::ManualPush,
            estimated_tokens: 0,
            content_type: ContentType::Mixed,
        }
    }

    // -- TokenBudget tests --

    #[test]
    fn token_estimate_empty() {
        assert_eq!(TokenBudget::estimate_tokens("", ContentType::Code), 0);
        assert_eq!(
            TokenBudget::estimate_tokens("", ContentType::NaturalLanguage),
            0
        );
        assert_eq!(TokenBudget::estimate_tokens("", ContentType::Mixed), 0);
    }

    #[test]
    fn token_estimate_code_ratio() {
        // 30 chars at 3.0 chars/token = 10 tokens
        let text = "a".repeat(30);
        assert_eq!(TokenBudget::estimate_tokens(&text, ContentType::Code), 10);
    }

    #[test]
    fn token_estimate_prose_ratio() {
        // 45 chars at 4.5 chars/token = 10 tokens
        let text = "a".repeat(45);
        assert_eq!(
            TokenBudget::estimate_tokens(&text, ContentType::NaturalLanguage),
            10
        );
    }

    #[test]
    fn token_estimate_mixed_ratio() {
        // 35 chars at 3.5 chars/token = 10 tokens
        let text = "a".repeat(35);
        assert_eq!(TokenBudget::estimate_tokens(&text, ContentType::Mixed), 10);
    }

    #[test]
    fn token_estimate_rounds_up() {
        // 31 chars at 3.0 = 10.33 -> ceil -> 11
        let text = "a".repeat(31);
        assert_eq!(TokenBudget::estimate_tokens(&text, ContentType::Code), 11);
    }

    #[test]
    fn token_estimate_custom_override() {
        // 20 chars at 2.0 chars/token = 10
        let text = "a".repeat(20);
        assert_eq!(TokenBudget::estimate_tokens_with_ratio(&text, 2.0), 10);
    }

    #[test]
    fn trim_within_budget() {
        let ctx = make_context(vec![make_memory("m1", 0.9)], vec!["conv1".to_string()]);
        let budget = TokenBudget {
            max_tokens: 10000,
            chars_per_token_override: None,
        };
        let trimmed = budget.trim(&ctx);
        assert_eq!(trimmed.memories.len(), ctx.memories.len());
        assert_eq!(trimmed.conventions.len(), ctx.conventions.len());
    }

    #[test]
    fn trim_does_not_mutate_original() {
        let ctx = make_context(
            vec![make_memory("m1", 0.9)],
            vec![
                "convention A".to_string(),
                "convention B".to_string(),
                "convention C".to_string(),
            ],
        );
        let budget = TokenBudget {
            max_tokens: 75,
            chars_per_token_override: None,
        };
        let _trimmed = budget.trim(&ctx);
        // Original must be unchanged
        assert_eq!(ctx.conventions.len(), 3);
        assert_eq!(ctx.memories.len(), 1);
    }

    #[test]
    fn trim_drops_conventions_first() {
        let ctx = make_context(
            vec![make_memory("m1", 0.9)],
            vec![
                "convention A".to_string(),
                "convention B".to_string(),
                "convention C".to_string(),
            ],
        );
        // Estimate: full render is roughly header(~200) + memory(~60) + 3 conventions(~40) = ~300 chars
        // At 3.5 chars/token = ~86 tokens. Budget 75 forces trimming conventions but keeps memory.
        let budget = TokenBudget {
            max_tokens: 75,
            chars_per_token_override: None,
        };
        let trimmed = budget.trim(&ctx);
        // Conventions should be trimmed (at least partially) before memories
        assert!(trimmed.conventions.len() < ctx.conventions.len());
    }

    #[test]
    fn trim_drops_low_confidence_memories() {
        let ctx = make_context(
            vec![
                make_memory("high", 0.95),
                make_memory("medium", 0.80),
                make_memory("low", 0.71),
            ],
            Vec::new(),
        );
        let budget = TokenBudget {
            max_tokens: 40,
            chars_per_token_override: None,
        };
        let ctx = budget.trim(&ctx);
        // Lower confidence memories should be dropped first
        if !ctx.memories.is_empty() {
            assert!(ctx.memories[0].confidence >= 0.80);
        }
    }

    #[test]
    fn trim_truncates_as_last_resort() {
        let ctx = make_context(vec![make_memory("m1", 0.95)], vec!["conv1".to_string()]);
        let budget = TokenBudget {
            max_tokens: 5,
            chars_per_token_override: None,
        };
        let trimmed = budget.trim(&ctx);
        // After aggressive trimming, memories and conventions should be cleared
        assert!(trimmed.memories.is_empty());
        assert!(trimmed.conventions.is_empty());
    }

    // -- ProviderInjectionStrategy tests --

    #[test]
    fn strategy_claude_code() {
        let agent = AgentInfo {
            name: "Claude Code".to_string(),
            provider: "anthropic".to_string(),
            model: None,
            confidence: 1.0,
        };
        assert_eq!(
            ProviderInjectionStrategy::from_agent_info(Some(&agent)),
            ProviderInjectionStrategy::ClaudeCode
        );
    }

    #[test]
    fn strategy_aider() {
        let agent = AgentInfo {
            name: "Aider v0.50".to_string(),
            provider: "openai".to_string(),
            model: None,
            confidence: 1.0,
        };
        assert_eq!(
            ProviderInjectionStrategy::from_agent_info(Some(&agent)),
            ProviderInjectionStrategy::Aider
        );
    }

    #[test]
    fn strategy_codex_direct_paste() {
        let agent = AgentInfo {
            name: "Codex CLI".to_string(),
            provider: "openai".to_string(),
            model: None,
            confidence: 1.0,
        };
        assert_eq!(
            ProviderInjectionStrategy::from_agent_info(Some(&agent)),
            ProviderInjectionStrategy::DirectPaste
        );
    }

    #[test]
    fn strategy_gemini_direct_paste() {
        let agent = AgentInfo {
            name: "Gemini CLI".to_string(),
            provider: "google".to_string(),
            model: None,
            confidence: 1.0,
        };
        assert_eq!(
            ProviderInjectionStrategy::from_agent_info(Some(&agent)),
            ProviderInjectionStrategy::DirectPaste
        );
    }

    #[test]
    fn strategy_unknown_fallback() {
        assert_eq!(
            ProviderInjectionStrategy::from_agent_info(None),
            ProviderInjectionStrategy::DirectPaste
        );
    }

    // -- SessionContext render tests --

    #[test]
    fn render_markdown_format() {
        let ctx = make_context(
            vec![make_memory("error-handling", 0.9)],
            vec!["Use Result types".to_string()],
        );
        let rendered = ctx.render();
        assert!(rendered.contains("# ZRemote Context Update"));
        assert!(rendered.contains("## Project: test-project"));
        assert!(rendered.contains("- Path: /home/user/project"));
        assert!(rendered.contains("- Type: rust"));
        assert!(rendered.contains("- Branch: main"));
        assert!(rendered.contains("## Recent Memories"));
        assert!(rendered.contains("error-handling"));
        assert!(rendered.contains("## Conventions"));
        assert!(rendered.contains("Use Result types"));
    }

    #[test]
    fn render_with_trigger() {
        let ctx = make_context(Vec::new(), Vec::new());
        let rendered = ctx.render();
        assert!(rendered.contains("Trigger: manual push"));
    }

    #[test]
    fn render_memory_extracted_trigger() {
        let mut ctx = make_context(Vec::new(), Vec::new());
        ctx.trigger = ContextTrigger::MemoryExtracted {
            loop_id: uuid::Uuid::new_v4(),
            count: 3,
        };
        let rendered = ctx.render();
        assert!(rendered.contains("3 new memories extracted"));
    }

    // -- ContextAssembler tests --

    #[test]
    fn assemble_filters_low_confidence() {
        let memories = vec![
            ContextMemoryInput {
                key: "high".to_string(),
                content: "high confidence".to_string(),
                category: MemoryCategory::Pattern,
                confidence: 0.9,
            },
            ContextMemoryInput {
                key: "low".to_string(),
                content: "low confidence".to_string(),
                category: MemoryCategory::Pitfall,
                confidence: 0.5,
            },
        ];
        let ctx = ContextAssembler::assemble(
            "test",
            "/path",
            "rust",
            Some("main"),
            &[],
            &memories,
            &[],
            ContextTrigger::ManualPush,
        );
        assert_eq!(ctx.memories.len(), 1);
        assert_eq!(ctx.memories[0].key, "high");
    }

    #[test]
    fn assemble_sorts_by_confidence() {
        let memories = vec![
            ContextMemoryInput {
                key: "medium".to_string(),
                content: "medium".to_string(),
                category: MemoryCategory::Pattern,
                confidence: 0.8,
            },
            ContextMemoryInput {
                key: "high".to_string(),
                content: "high".to_string(),
                category: MemoryCategory::Decision,
                confidence: 0.95,
            },
            ContextMemoryInput {
                key: "low".to_string(),
                content: "low".to_string(),
                category: MemoryCategory::Pitfall,
                confidence: 0.75,
            },
        ];
        let ctx = ContextAssembler::assemble(
            "test",
            "/path",
            "rust",
            None,
            &[],
            &memories,
            &[],
            ContextTrigger::ManualPush,
        );
        assert_eq!(ctx.memories.len(), 3);
        assert_eq!(ctx.memories[0].key, "high");
        assert_eq!(ctx.memories[1].key, "medium");
        assert_eq!(ctx.memories[2].key, "low");
    }

    #[test]
    fn assemble_empty_memories() {
        let ctx = ContextAssembler::assemble(
            "test",
            "/path",
            "rust",
            Some("main"),
            &[],
            &[],
            &["Use clippy".to_string()],
            ContextTrigger::ManualPush,
        );
        assert!(ctx.memories.is_empty());
        assert_eq!(ctx.conventions.len(), 1);
        assert!(ctx.estimated_tokens > 0);
    }

    #[test]
    fn assemble_includes_frameworks() {
        let ctx = ContextAssembler::assemble(
            "test",
            "/path",
            "rust",
            None,
            &["Axum".to_string(), "Tokio".to_string()],
            &[],
            &[],
            ContextTrigger::ManualPush,
        );
        assert_eq!(ctx.project.frameworks.len(), 2);
    }

    // -- NudgeAccumulator tests --

    #[test]
    fn accumulator_push_replaces() {
        let mut acc = NudgeAccumulator::new();
        assert!(!acc.has_pending());

        acc.push(ContextTrigger::ManualPush);
        assert!(acc.has_pending());

        acc.push(ContextTrigger::ConventionsUpdated {
            project_path: "/test".to_string(),
        });
        assert!(acc.has_pending());

        let trigger = acc.take().unwrap();
        assert!(matches!(trigger, ContextTrigger::ConventionsUpdated { .. }));
        assert!(!acc.has_pending());
    }

    #[test]
    fn accumulator_take_returns_none_when_empty() {
        let mut acc = NudgeAccumulator::new();
        assert!(acc.take().is_none());
    }

    // -- DeliveryCoordinator tests --

    #[test]
    fn deferred_nudge_replaces() {
        let mut coord = DeliveryCoordinator::new();
        let sid = uuid::Uuid::new_v4();

        let ctx1 = make_context(vec![make_memory("m1", 0.9)], Vec::new());
        coord.on_context_changed(sid, ctx1);
        assert!(coord.has_pending(&sid));

        let ctx2 = make_context(vec![make_memory("m2", 0.95)], Vec::new());
        coord.on_context_changed(sid, ctx2);

        let delivered = coord.on_phase_idle(&sid).unwrap();
        // Should have the second context's memory (rendered as string)
        assert!(delivered.contains("m2"));
    }

    #[test]
    fn deferred_nudge_expires() {
        let mut coord = DeliveryCoordinator::new();
        // Use a very short max age
        coord.max_nudge_age = Duration::from_nanos(1);
        let sid = uuid::Uuid::new_v4();

        let ctx = make_context(vec![make_memory("m1", 0.9)], Vec::new());
        coord.on_context_changed(sid, ctx);

        // Wait for expiry
        std::thread::sleep(Duration::from_millis(1));

        assert!(coord.on_phase_idle(&sid).is_none());
    }

    #[test]
    fn on_phase_idle_delivers() {
        let mut coord = DeliveryCoordinator::new();
        let sid = uuid::Uuid::new_v4();

        let ctx = make_context(vec![make_memory("m1", 0.9)], Vec::new());
        coord.on_context_changed(sid, ctx);

        let delivered = coord.on_phase_idle(&sid);
        assert!(delivered.is_some());
        // After delivery, no more pending
        assert!(!coord.has_pending(&sid));
    }

    #[test]
    fn no_nudge_no_delivery() {
        let mut coord = DeliveryCoordinator::new();
        let sid = uuid::Uuid::new_v4();
        assert!(coord.on_phase_idle(&sid).is_none());
    }

    #[test]
    fn remove_session_cleans_up() {
        let mut coord = DeliveryCoordinator::new();
        let sid = uuid::Uuid::new_v4();

        let ctx = make_context(vec![make_memory("m1", 0.9)], Vec::new());
        coord.on_context_changed(sid, ctx);
        coord.accumulator(&sid).push(ContextTrigger::ManualPush);

        coord.remove_session(&sid);
        assert!(!coord.has_pending(&sid));
    }

    // -- PtyTransport tests --

    #[tokio::test]
    async fn pty_transport_direct_paste() {
        let (tx, mut rx) = mpsc::channel(16);
        let writer = SessionWriterHandle::new(tx);
        let transport = PtyTransport::new(writer).unwrap();
        let sid = uuid::Uuid::new_v4();

        let result = transport
            .deliver_with_strategy(&sid, "test content", ProviderInjectionStrategy::DirectPaste)
            .await
            .unwrap();

        assert_eq!(result, DeliveryStatus::Unconfirmed);

        let req = rx.recv().await.unwrap();
        let data = String::from_utf8(req.data).unwrap();
        assert!(data.contains("--- ZRemote Context ---"));
        assert!(data.contains("test content"));
        assert!(data.contains("--- End ZRemote Context ---"));
    }

    #[tokio::test]
    async fn pty_transport_file_based_claude() {
        let (tx, mut rx) = mpsc::channel(16);
        let writer = SessionWriterHandle::new(tx);
        let transport = PtyTransport::new(writer).unwrap();
        let sid = uuid::Uuid::new_v4();

        let result = transport
            .deliver_with_strategy(&sid, "test content", ProviderInjectionStrategy::ClaudeCode)
            .await
            .unwrap();

        assert_eq!(result, DeliveryStatus::Unconfirmed);

        let req = rx.recv().await.unwrap();
        let data = String::from_utf8(req.data).unwrap();
        assert!(data.starts_with("/read "));
        assert!(data.contains("zremote-context-"));
        assert!(data.ends_with('\n'));
    }

    #[tokio::test]
    async fn pty_transport_file_based_aider() {
        let (tx, mut rx) = mpsc::channel(16);
        let writer = SessionWriterHandle::new(tx);
        let transport = PtyTransport::new(writer).unwrap();
        let sid = uuid::Uuid::new_v4();

        let result = transport
            .deliver_with_strategy(&sid, "test content", ProviderInjectionStrategy::Aider)
            .await
            .unwrap();

        assert_eq!(result, DeliveryStatus::Unconfirmed);

        let req = rx.recv().await.unwrap();
        let data = String::from_utf8(req.data).unwrap();
        assert!(data.starts_with("/add "));
        assert!(data.contains("zremote-context-"));
    }

    #[tokio::test]
    async fn writer_handle_channel() {
        let (tx, mut rx) = mpsc::channel(16);
        let handle = SessionWriterHandle::new(tx);
        let sid = uuid::Uuid::new_v4();

        handle.write(sid, b"hello".to_vec()).await.unwrap();

        let req = rx.recv().await.unwrap();
        assert_eq!(req.session_id, sid);
        assert_eq!(req.data, b"hello");
    }

    #[tokio::test]
    async fn writer_handle_closed_channel() {
        let (tx, rx) = mpsc::channel::<SessionWriteRequest>(1);
        drop(rx);
        let handle = SessionWriterHandle::new(tx);
        let sid = uuid::Uuid::new_v4();

        let result = handle.write(sid, b"hello".to_vec()).await;
        assert!(result.is_err());
    }

    // -- inotify confirmation tests --

    #[tokio::test]
    async fn inotify_confirms_on_file_access() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test-confirm.md");
        std::fs::write(&file_path, "initial").unwrap();

        let rx = setup_file_watcher(&file_path).unwrap();
        ACTIVE_WATCHERS.fetch_add(1, Ordering::Relaxed);

        let path_clone = file_path.clone();
        let reader = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            // Modify the file to trigger an inotify event
            std::fs::write(&path_clone, "modified").unwrap();
        });

        let result =
            tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5))).await;

        ACTIVE_WATCHERS.fetch_sub(1, Ordering::Relaxed);
        reader.join().unwrap();

        assert!(result.is_ok(), "spawn_blocking should not panic");
        assert!(
            result.unwrap().is_ok(),
            "should receive confirmation signal"
        );
    }

    #[tokio::test]
    async fn inotify_unconfirmed_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test-timeout.md");
        std::fs::write(&file_path, "initial").unwrap();

        let rx = setup_file_watcher(&file_path).unwrap();
        ACTIVE_WATCHERS.fetch_add(1, Ordering::Relaxed);

        let result =
            tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_millis(100))).await;

        ACTIVE_WATCHERS.fetch_sub(1, Ordering::Relaxed);

        assert!(result.is_ok(), "spawn_blocking should not panic");
        assert!(
            result.unwrap().is_err(),
            "should timeout without file access"
        );
    }

    #[tokio::test]
    async fn inotify_fallback_on_watcher_limit() {
        let (tx, mut rx) = mpsc::channel(16);
        let writer = SessionWriterHandle::new(tx);
        let transport = PtyTransport::new(writer).unwrap();
        let sid = uuid::Uuid::new_v4();

        let prev = ACTIVE_WATCHERS.swap(MAX_ACTIVE_WATCHERS, Ordering::Relaxed);

        let result = transport
            .deliver_via_file_with_timeout(
                &sid,
                "test",
                ProviderInjectionStrategy::ClaudeCode,
                Duration::from_millis(100),
            )
            .await
            .unwrap();

        ACTIVE_WATCHERS.store(prev, Ordering::Relaxed);

        assert_eq!(result, DeliveryStatus::Unconfirmed);
        let _req = rx.recv().await.unwrap();
    }

    // -- parse_category tests --

    #[test]
    fn parse_category_known() {
        assert_eq!(parse_category("pattern"), MemoryCategory::Pattern);
        assert_eq!(parse_category("decision"), MemoryCategory::Decision);
        assert_eq!(parse_category("pitfall"), MemoryCategory::Pitfall);
        assert_eq!(parse_category("preference"), MemoryCategory::Preference);
        assert_eq!(parse_category("architecture"), MemoryCategory::Architecture);
        assert_eq!(parse_category("convention"), MemoryCategory::Convention);
    }

    #[test]
    fn parse_category_unknown_falls_back() {
        assert_eq!(parse_category("unknown"), MemoryCategory::Convention);
        assert_eq!(parse_category(""), MemoryCategory::Convention);
    }

    // -- ContextChangeEvent tests --

    #[test]
    fn context_change_event_memories_extracted() {
        let event = ContextChangeEvent::MemoriesExtracted {
            loop_id: uuid::Uuid::new_v4(),
            memories: vec![ContextMemoryInput {
                key: "test".to_string(),
                content: "test content".to_string(),
                category: MemoryCategory::Pattern,
                confidence: 0.9,
            }],
            project_path: "/home/user/project".to_string(),
        };
        // Verify the event can be cloned and debug-printed
        let cloned = event.clone();
        let debug = format!("{cloned:?}");
        assert!(debug.contains("MemoriesExtracted"));
    }

    // -- Full delivery flow tests --

    #[test]
    fn full_delivery_flow() {
        let memories = vec![
            ContextMemoryInput {
                key: "error-handling".to_string(),
                content: "Always use Result types for fallible operations".to_string(),
                category: MemoryCategory::Pattern,
                confidence: 0.9,
            },
            ContextMemoryInput {
                key: "db-conventions".to_string(),
                content: "Use sqlx for all database queries".to_string(),
                category: MemoryCategory::Convention,
                confidence: 0.85,
            },
            ContextMemoryInput {
                key: "low-confidence".to_string(),
                content: "Maybe use println for debugging".to_string(),
                category: MemoryCategory::Pitfall,
                confidence: 0.5,
            },
        ];

        let ctx = ContextAssembler::assemble(
            "zremote",
            "/home/user/zremote",
            "rust",
            Some("main"),
            &["Axum".to_string(), "Tokio".to_string()],
            &memories,
            &[
                "Use clippy".to_string(),
                "No unwrap in production".to_string(),
            ],
            ContextTrigger::MemoryExtracted {
                loop_id: uuid::Uuid::new_v4(),
                count: 2,
            },
        );

        // Low confidence memory should be filtered out
        assert_eq!(ctx.memories.len(), 2);
        assert!(
            ctx.memories
                .iter()
                .all(|m| m.confidence >= MIN_DELIVERY_CONFIDENCE)
        );

        // Render should produce valid markdown
        let rendered = ctx.render();
        assert!(!rendered.is_empty());
        assert!(rendered.contains("# ZRemote Context Update"));
        assert!(rendered.contains("## Project: zremote"));
        assert!(rendered.contains("error-handling"));
        assert!(rendered.contains("db-conventions"));
        assert!(!rendered.contains("low-confidence"));
        assert!(rendered.contains("## Conventions"));
        assert!(rendered.contains("Use clippy"));
        assert!(rendered.contains("Trigger:"));

        // Token estimate should be positive
        assert!(ctx.estimated_tokens > 0);
    }

    #[test]
    fn cleanup_removes_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test-context.md");
        std::fs::write(&file_path, "test content").unwrap();
        assert!(file_path.exists());

        let dir_path = dir.path().to_path_buf();
        drop(dir);
        assert!(
            !dir_path.exists(),
            "TempDir drop should remove directory and its contents"
        );
    }

    // -- NudgeAccumulator integration --

    #[test]
    fn accumulator_debounce_behavior() {
        let mut acc = NudgeAccumulator::new();
        assert!(!acc.has_pending());
        assert!(!acc.is_ready());

        // Push replaces previous trigger and resets debounce timer
        acc.push(ContextTrigger::ManualPush);
        assert!(acc.has_pending());
        assert!(
            !acc.is_ready(),
            "should not be ready immediately after push"
        );

        acc.push(ContextTrigger::ConventionsUpdated {
            project_path: "/test".to_string(),
        });
        assert!(acc.has_pending());
        assert!(
            !acc.is_ready(),
            "should not be ready immediately after second push"
        );

        // Wait for debounce window to pass (2s + margin)
        std::thread::sleep(Duration::from_millis(2100));
        assert!(acc.is_ready(), "should be ready after debounce period");

        // Take returns the latest trigger and clears timer
        let trigger = acc.take().unwrap();
        assert!(matches!(trigger, ContextTrigger::ConventionsUpdated { .. }));
        assert!(!acc.has_pending());
        assert!(!acc.is_ready());

        // Take on empty returns None
        assert!(acc.take().is_none());
    }

    // -- DeliveryCoordinator end-to-end --

    #[test]
    fn delivery_coordinator_full_flow() {
        let mut coord = DeliveryCoordinator::new();
        let sid = uuid::Uuid::new_v4();

        // No pending nudge -> no delivery
        assert!(coord.on_phase_idle(&sid).is_none());

        // Set up context with memories and conventions
        let ctx = make_context(
            vec![make_memory("m1", 0.9), make_memory("m2", 0.85)],
            vec!["Use clippy".to_string()],
        );
        coord.on_context_changed(sid, ctx);
        assert!(coord.has_pending(&sid));

        // on_phase_idle delivers and clears pending
        let rendered = coord.on_phase_idle(&sid).unwrap();
        assert!(rendered.contains("m1"));
        assert!(rendered.contains("m2"));
        assert!(rendered.contains("Use clippy"));
        assert!(!coord.has_pending(&sid));

        // Second idle call returns None (already delivered)
        assert!(coord.on_phase_idle(&sid).is_none());

        // New context replaces previous (even if first was delivered)
        let ctx2 = make_context(vec![make_memory("m3", 0.92)], Vec::new());
        coord.on_context_changed(sid, ctx2);
        let rendered2 = coord.on_phase_idle(&sid).unwrap();
        assert!(rendered2.contains("m3"));
        assert!(!rendered2.contains("m1"));
    }
}
