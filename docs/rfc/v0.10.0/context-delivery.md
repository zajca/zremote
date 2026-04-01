# RFC: Real-time Context Delivery — Push Knowledge to Running Agents

**Status:** Implemented (2026-04-01)
**Date:** 2026-03-31
**Author:** zajca
**Parent:** [RFC v0.10.0 Agent Intelligence](README.md) (Phase 6)
**Depends on:** Phase 1 (Output Analyzer), Phase 4 (Project Intelligence)

---

## 1. Problem Statement

ZRemote's knowledge system currently operates **post-hoc**: memories are extracted from completed agentic loops, instructions are generated from accumulated memories, and CLAUDE.md is written as a static file. Once an AI agent session starts, the agent receives no further context updates regardless of what changes around it.

### Current knowledge flow

```
Loop completes
  -> KnowledgeServerMessage::ExtractMemory { transcript }
  -> OvClient::extract_memories()
  -> KnowledgeAgentMessage::MemoryExtracted
  -> Server stores in knowledge_memories table

Separately:
  GenerateInstructions -> writes CLAUDE.md (one-time, pre-session)
```

### Problems

1. **No mid-session context** -- If a memory is extracted from one loop while another loop is running on the same project, the running agent has no idea. The same applies to file changes, convention discoveries, or cross-worker learnings.

2. **No delivery mechanism** -- Even if we assembled context, there is no way to get it into a running agent session. The `PtySession::write()` method exists but nothing coordinates when and what to write.

3. **No token budgeting** -- Agent context windows are finite. Injecting unbounded context wastes tokens or gets truncated. There is no estimation of how much context an agent can absorb.

4. **No awareness of agent state** -- Injecting context while an agent is busy processing a tool call creates noise. Phase detection exists (Output Analyzer, Phase 1) but nothing consumes phase transitions for delivery scheduling.

### Existing infrastructure this builds on

| Component | Location | Relevance |
|-----------|----------|-----------|
| `OutputAnalyzer` | `agentic/analyzer.rs` | `AnalyzerPhase::Idle` / `NeedsInput` detection |
| `handle_analyzer_event()` | `connection/mod.rs:122-166` | Maps `AnalyzerEvent::PhaseChanged` to protocol messages |
| `KnowledgeManager` | `knowledge/mod.rs` | Orchestrates OV process, handles `KnowledgeServerMessage` |
| `MemoryRow` | `zremote-core/queries/knowledge.rs` | Stored memories per project |
| `CachedMemory` | `zremote-protocol/knowledge.rs` | Serializable memory with category and confidence |
| `SessionManager::write_to()` | `session.rs:98-112` | Writes bytes to PTY stdin |
| `PtySession::write()` | `pty.rs:124-128` | Low-level PTY write |
| `ProjectRow` | `zremote-core/queries/projects.rs` | Project metadata (path, type, git info) |

---

## 2. Goals

- **Assemble session context** from project data, memories, and conventions into a structured payload
- **Estimate token cost** of assembled context and trim to fit within a configurable budget
- **Defer delivery** when agent is busy; deliver when agent transitions to Idle or NeedsInput
- **Provider-aware PTY injection** -- detect running agent (via Output Analyzer Phase 1) and use the appropriate injection strategy: `/read` for Claude Code, `/add` for Aider, direct paste for others
- **Prepare for Channel Bridge** (Phase 7) -- design delivery via the `ContextTransport` trait (canonical definition; Channel Bridge's `ChannelTransport` implements it) so structured MCP delivery can replace PTY injection
- **Bounded scope** -- context delivery is per-session, not cross-session (cross-worker delivery is Phase 7's responsibility)

### Non-goals

- GUI changes for context delivery status (separate RFC)
- Cross-session context sharing (Phase 7: Channel Bridge)
- Automatic context refresh on a timer (event-driven only)
- Modifying the knowledge extraction pipeline (Phase 4 handles that)

---

## 3. Design

### 3.1 Architecture

```
Memory/project change event (from KnowledgeManager or project scanner)
  |
  v
NudgeAccumulator (per session, 2s debounce window)
  |-- merges concurrent triggers by priority
  |-- resets timer on each new change
  |
  v (debounce expires)
ContextAssembler::assemble(session_id) -> SessionContext
  |
  v
TokenBudget::trim(context, budget) -> SessionContext (trimmed)
  |  (ratio from ContentType: Code=3.0, NL=4.5, Mixed=3.5)
  |
  v
DeliveryCoordinator (in-memory HashMap<SessionId, DeferredNudge>)
  |-- if agent is Idle/NeedsInput -> deliver immediately
  |-- if agent is Busy -> store as DeferredNudge
  |                         |
  |                         v
  |                     AnalyzerEvent::PhaseChanged(Idle|NeedsInput)
  |                         |
  |                         v
  |                     deliver deferred nudge
  |
  v
ContextTransport (trait, canonical definition)
  |-- PtyTransport (default):
  |     |-- Claude Code: `/read <path>` + inotify confirm
  |     |-- Aider: `/add <path>` + inotify confirm
  |     |-- Codex/Gemini/unknown: direct paste with delimiters
  |-- ChannelTransport (Phase 7): send MCP notification
```

### 3.2 Core Types

```rust
/// Assembled context ready for delivery to a running agent session.
pub struct SessionContext {
    /// Project summary (name, path, type, languages, frameworks).
    pub project: ProjectSummary,
    /// Relevant memories for this project, sorted by confidence descending.
    pub memories: Vec<ContextMemory>,
    /// Project conventions extracted from Phase 4.
    pub conventions: Vec<String>,
    /// Reason this context was assembled (what changed).
    pub trigger: ContextTrigger,
    /// Estimated token count for the entire context payload.
    pub estimated_tokens: usize,
    /// Content type hint for token estimation accuracy.
    pub content_type: ContentType,
}

/// Content type hint for differentiated token estimation.
pub enum ContentType {
    /// Mostly code — uses ~3 chars/token ratio.
    Code,
    /// Mostly natural language — uses ~4.5 chars/token ratio.
    NaturalLanguage,
    /// Mixed content (default) — uses ~3.5 chars/token ratio.
    Mixed,
}

/// Per-provider injection strategy. The Output Analyzer (Phase 1) detects
/// the provider via `AgentInfo::name`, which maps to the appropriate strategy.
pub enum ProviderInjectionStrategy {
    /// Claude Code: write temp file, inject `/read <path>`.
    ClaudeCode,
    /// Aider: write temp file, inject `/add <path>`.
    Aider,
    /// Direct paste: no file-read command available. Paste content directly
    /// into PTY stdin wrapped in `--- ZRemote Context ---` delimiters.
    DirectPaste,
}

impl ProviderInjectionStrategy {
    /// Determine injection strategy from detected agent info.
    /// Falls back to `DirectPaste` for unknown providers.
    pub fn from_agent_info(agent: Option<&AgentInfo>) -> Self {
        match agent.map(|a| a.name.as_str()) {
            Some("Claude Code") => Self::ClaudeCode,
            Some(name) if name.starts_with("Aider") => Self::Aider,
            // Codex CLI, Gemini CLI, and unknown agents: direct paste
            _ => Self::DirectPaste,
        }
    }
}

/// Minimal project summary for context injection.
pub struct ProjectSummary {
    pub name: String,
    pub path: String,
    pub project_type: String,
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub git_branch: Option<String>,
}

/// A memory entry included in context delivery.
pub struct ContextMemory {
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub confidence: f64,
}

/// What triggered this context assembly.
pub enum ContextTrigger {
    /// New memories were extracted from a completed loop.
    MemoryExtracted { loop_id: AgenticLoopId, count: usize },
    /// Project conventions were updated (Phase 4 scan).
    ConventionsUpdated { project_path: String },
    /// Manual trigger from server/GUI.
    ManualPush,
}
```

### 3.3 Token Budget Trimming

Token estimation uses differentiated character-per-token ratios based on content type:

| Content type | Chars/token | Typical content |
|---|---|---|
| `Code` | ~3 | Source code, config files, structured data |
| `NaturalLanguage` | ~4.5 | Memories, descriptions, prose |
| `Mixed` (default) | ~3.5 | Typical context payloads with both |

These are estimations, not exact counts. Exact tokenization would require provider-specific tokenizers (tiktoken, Anthropic's tokenizer, etc.) which is out of scope for this phase. The `content_type` field on `SessionContext` selects the ratio. Users can override the ratio via config (`context_delivery.chars_per_token`).

```rust
pub struct TokenBudget {
    /// Maximum tokens for the entire context payload.
    pub max_tokens: usize,
    /// Override chars-per-token ratio. If None, derived from ContentType.
    pub chars_per_token_override: Option<f32>,
}

impl TokenBudget {
    /// Estimate token count for a string using the given content type.
    pub fn estimate_tokens(text: &str, content_type: ContentType) -> usize {
        let ratio = match content_type {
            ContentType::Code => 3.0_f32,
            ContentType::NaturalLanguage => 4.5,
            ContentType::Mixed => 3.5,
        };
        (text.len() as f32 / ratio).ceil() as usize
    }

    /// Estimate with explicit ratio override.
    pub fn estimate_tokens_with_ratio(text: &str, chars_per_token: f32) -> usize {
        (text.len() as f32 / chars_per_token).ceil() as usize
    }

    /// Trim context to fit within budget. Priority order (lowest trimmed first):
    /// 1. Drop conventions (lowest impact)
    /// 2. Drop memories by ascending confidence
    /// 3. Truncate project summary fields
    pub fn trim(&self, context: &mut SessionContext) {
        // ... implementation uses chars_per_token_override if set,
        // otherwise derives ratio from context.content_type
    }
}
```

Default budget: **4096 tokens** (~14KB of mixed text). Configurable per session.

Trimming priority (items removed first have lowest value):
1. Conventions from the end of the list (assumed lower-priority)
2. Memories with lowest confidence score
3. If still over budget, truncate the entire payload with a `[truncated]` marker

### 3.4 Nudge Accumulator and Deferred Delivery

Concurrent changes (multiple files saved, memory extraction completing while conventions update) are batched via a `NudgeAccumulator` per session. This prevents nudge storms from rapid sequential events.

```rust
/// Accumulates changes within a debounce window before assembling a nudge.
pub struct NudgeAccumulator {
    /// Pending triggers, merged by priority.
    pending_triggers: Vec<ContextTrigger>,
    /// Debounce handle — resets on each new change.
    debounce_handle: Option<tokio::task::JoinHandle<()>>,
    /// Debounce window (default: 2 seconds).
    debounce_duration: Duration,
}
```

Merge strategy for concurrent changes within the 2-second debounce window:
- Multiple file changes are merged into a single nudge
- Priority ordering: memory updates > file changes > convention updates
- If a nudge is pending and a new change arrives, the debounce timer resets and the new trigger is merged into the existing pending set
- Uses `tokio::time::sleep` debounce (not per-event delivery)

When the debounce window expires, the accumulator assembles a single `SessionContext` from all accumulated triggers and forwards it to the `DeliveryCoordinator`.

```rust
/// A nudge that was deferred because the agent was busy.
pub struct DeferredNudge {
    /// The assembled context to deliver.
    pub context: SessionContext,
    /// When this nudge was created.
    pub created_at: Instant,
    /// Number of times delivery was deferred (for logging/metrics).
    pub defer_count: u32,
}

/// Coordinates context delivery timing based on agent phase.
/// All state is in-memory — nudges are ephemeral and do not need persistence.
pub struct DeliveryCoordinator {
    /// Per-session deferred nudges (in-memory HashMap, not DB).
    /// Only the latest nudge per session is kept (newer supersedes older).
    pending: HashMap<SessionId, DeferredNudge>,
    /// Per-session nudge accumulators for debouncing concurrent changes.
    accumulators: HashMap<SessionId, NudgeAccumulator>,
    /// Delivery backend.
    backend: Box<dyn ContextTransport + Send>,
    /// Token budget configuration.
    budget: TokenBudget,
    /// Maximum age for deferred nudges before they are dropped (default: 5 min).
    max_nudge_age: Duration,
    /// Optional delivery audit log (configurable, disabled by default).
    /// When enabled, logs delivery events to DB for debugging.
    audit_log_enabled: bool,
}
```

Key behaviors:
- **Debounced accumulation** -- changes within 2-second window are merged into a single nudge via `NudgeAccumulator`
- **One pending nudge per session** -- if context changes again while a nudge is pending, the new context replaces the old one (it is strictly more recent)
- **In-memory storage** -- pending nudges are stored in a `HashMap<SessionId, DeferredNudge>`, not in the database. Nudges are ephemeral and restart-safe (losing a pending nudge on restart is acceptable)
- **Nudge expiry** -- nudges older than `max_nudge_age` (default 5 minutes) are dropped, since the context is likely stale
- **Phase-triggered delivery** -- when `AnalyzerEvent::PhaseChanged(Idle | NeedsInput)` fires for a session, the coordinator checks for a pending nudge and delivers it
- **Audit log** -- optionally logs delivery events (timestamp, session, trigger, status) to DB for debugging. Disabled by default, enabled via config (`context_delivery.audit_log = true`)

### 3.5 Context Transport Trait

This is the **canonical definition** of the `ContextTransport` trait. The Channel Bridge RFC's `ChannelTransport` (see [channel-bridge.md](channel-bridge.md) section 4.7) implements this trait. The trait signature here is authoritative — any changes must be reflected in both RFCs.

```rust
/// Transport for delivering context to a running agent session.
/// This is the canonical trait definition. Channel Bridge's ChannelTransport
/// and this RFC's PtyTransport both implement it.
pub trait ContextTransport: Send + Sync {
    /// Deliver context content to a session.
    /// Returns delivery status.
    async fn deliver(
        &self,
        session_id: &SessionId,
        content: &str,
    ) -> Result<DeliveryStatus, DeliveryError>;
}

/// Result of a delivery attempt.
pub enum DeliveryStatus {
    /// Content was delivered and confirmed (e.g., file access detected).
    Delivered,
    /// Content was sent but confirmation could not be verified
    /// (e.g., agent may be busy, no inotify event within timeout).
    Unconfirmed,
    /// Delivery failed permanently (e.g., PTY closed, channel disconnected).
    Failed(String),
}
```

### 3.6 PTY Injection Backend (Default)

The default backend writes context to a temp file and injects a provider-appropriate command into the PTY. The `ProviderInjectionStrategy` (determined from `AgentInfo` detected by Output Analyzer Phase 1) controls what is injected.

```rust
pub struct PtyTransport {
    /// Reference to the session manager for writing to PTY stdin.
    session_writer: SessionWriterHandle,
    /// Directory for temp context files.
    temp_dir: PathBuf,
    /// Detected provider strategy for this session.
    injection_strategy: ProviderInjectionStrategy,
}
```

Flow varies by provider:

**Claude Code** (`ProviderInjectionStrategy::ClaudeCode`):
1. Render `SessionContext` to markdown format
2. Write to temp file: `{temp_dir}/zremote-context-{session_id}-{timestamp}.md`
3. Inject `/read {temp_file_path}\n` into PTY stdin
4. Monitor temp file for read access (inotify) within 5 seconds as delivery confirmation
5. Schedule temp file cleanup after 60 seconds

**Aider** (`ProviderInjectionStrategy::Aider`):
1. Render `SessionContext` to markdown format
2. Write to temp file: `{temp_dir}/zremote-context-{session_id}-{timestamp}.md`
3. Inject `/add {temp_file_path}\n` into PTY stdin
4. Monitor temp file for read access (inotify) within 5 seconds as delivery confirmation
5. Schedule temp file cleanup after 60 seconds

**Direct Paste** (`ProviderInjectionStrategy::DirectPaste`) — used for Codex CLI, Gemini CLI, and unknown providers:
1. Render `SessionContext` to markdown format
2. Inject directly into PTY stdin, wrapped in delimiters:
   ```
   --- ZRemote Context ---
   {rendered markdown}
   --- End ZRemote Context ---
   ```
3. Delivery status is always `Unconfirmed` (no file access to monitor)

#### Delivery Confirmation

For file-based strategies (Claude Code, Aider), delivery confirmation uses inotify:
- After writing the temp file and injecting the command, set up an inotify watch on the temp file for `IN_ACCESS` events
- If the file is accessed within 5 seconds, status is `Delivered`
- If no access within 5 seconds, status is `Unconfirmed` — log a warning but do not retry (the agent may be busy processing or the command may be queued)
- For Channel Bridge transport (Phase 7): use tool call response as confirmation instead of inotify

#### File Watcher Resource Limits

Context delivery may watch project files for changes (to trigger convention updates). Resource limits:
- **Maximum watched files:** 100 (configurable via `context_delivery.max_watched_files`)
- When the limit is reached, excess files use **polling fallback**: check mtime every 30 seconds instead of inotify
- This prevents exhausting the system's inotify watch limit (`/proc/sys/fs/inotify/max_user_watches`, typically 65536 on Linux)
- Files are prioritized for inotify by: CLAUDE.md/config files first, then by frequency of recent changes

Markdown rendering format:
```markdown
# ZRemote Context Update

## Project: {name}
- Path: {path}
- Type: {project_type}
- Branch: {git_branch}

## Recent Memories
### [{category}] {key}
{content}

## Conventions
- {convention_1}
- {convention_2}

---
Trigger: {trigger_description}
```

### 3.7 Session Writer Handle

To avoid passing a mutable `SessionManager` reference into the delivery backend (which lives in a different async context), we use a channel-based handle:

```rust
/// A handle for writing to PTY sessions from the delivery coordinator.
/// Sends write requests through a channel that the connection loop processes.
pub struct SessionWriterHandle {
    tx: mpsc::Sender<SessionWriteRequest>,
}

pub struct SessionWriteRequest {
    pub session_id: SessionId,
    pub data: Vec<u8>,
}
```

The connection loop's main select handles `SessionWriteRequest` by calling `session_manager.write_to()`.

### 3.8 Integration with Connection Loop

The `DeliveryCoordinator` is created alongside the per-session `OutputAnalyzer` map in `run_connection()`. Two integration points:

1. **Phase change handler** (in `handle_analyzer_event`):
```rust
AnalyzerEvent::PhaseChanged(phase) => {
    // ... existing status update code ...

    // Check for deferred nudges
    if matches!(phase, AnalyzerPhase::Idle | AnalyzerPhase::NeedsInput) {
        delivery_coordinator.on_phase_idle(session_id);
    }
}
```

2. **Knowledge event handler** (new, triggered when memories are extracted):
```rust
// When KnowledgeAgentMessage::MemoryExtracted arrives for a project
// that has active sessions, assemble and deliver/defer context
delivery_coordinator.on_context_changed(session_id, trigger);
```

### 3.9 Context Assembly

```rust
impl ContextAssembler {
    /// Assemble context for a session from available data sources.
    ///
    /// - project: from ProjectRow (existing DB query)
    /// - memories: from knowledge_memories table (existing query)
    /// - conventions: from Phase 4 project scan (when available)
    pub fn assemble(
        project: &ProjectRow,
        memories: &[MemoryRow],
        conventions: &[String],
        trigger: ContextTrigger,
    ) -> SessionContext {
        let project_summary = ProjectSummary {
            name: project.name.clone(),
            path: project.path.clone(),
            project_type: project.project_type.clone(),
            languages: Vec::new(), // Populated when Phase 4 is available
            frameworks: Vec::new(), // Populated when Phase 4 is available
            git_branch: project.git_branch.clone(),
        };

        let context_memories: Vec<ContextMemory> = memories
            .iter()
            .filter(|m| m.confidence >= MIN_DELIVERY_CONFIDENCE)
            .map(|m| ContextMemory {
                key: m.key.clone(),
                content: m.content.clone(),
                category: parse_category(&m.category),
                confidence: m.confidence,
            })
            .collect();

        let mut context = SessionContext {
            project: project_summary,
            memories: context_memories,
            conventions: conventions.to_vec(),
            trigger,
            estimated_tokens: 0,
            content_type: ContentType::Mixed, // Default; override if content is known
        };

        context.estimated_tokens = TokenBudget::estimate_tokens(
            &context.render(),
            context.content_type,
        );
        context
    }
}
```

`MIN_DELIVERY_CONFIDENCE`: 0.7 (higher than the 0.6 threshold used for CLAUDE.md generation, since mid-session injection is more disruptive).

---

## 4. Files

### CREATE

| File | Description |
|------|-------------|
| `crates/zremote-agent/src/knowledge/context_delivery.rs` | Core module: `SessionContext`, `ContentType`, `ProviderInjectionStrategy`, `ContextAssembler`, `TokenBudget`, `DeliveryCoordinator`, `NudgeAccumulator`, `ContextTransport` trait, `DeliveryStatus`, `PtyTransport`, `SessionWriterHandle` |

### MODIFY

| File | Change |
|------|--------|
| `crates/zremote-agent/src/knowledge/mod.rs` | Add `pub mod context_delivery;`, wire `MemoryExtracted` events to coordinator |
| `crates/zremote-agent/src/connection/mod.rs` | Create `DeliveryCoordinator` in `run_connection()`, handle `SessionWriteRequest` channel, call `on_phase_idle()` from `handle_analyzer_event()` |

---

## 5. Implementation Phases

### Phase 6a: Core Types and Token Budget

1. Create `context_delivery.rs` with:
   - `SessionContext`, `ProjectSummary`, `ContextMemory`, `ContextTrigger`, `ContentType` structs
   - `ProviderInjectionStrategy` enum with `from_agent_info()` mapping
   - `TokenBudget` with differentiated `estimate_tokens()` (code/prose/mixed ratios) and `trim()`
   - `SessionContext::render()` for markdown output
   - `DeliveryStatus` enum (`Delivered`, `Unconfirmed`, `Failed`)
2. Unit tests for token estimation at each content type ratio, custom override, trimming at various budget levels, and provider strategy mapping

### Phase 6b: Context Assembly

1. `ContextAssembler::assemble()` -- builds `SessionContext` from project/memory data
2. `MIN_DELIVERY_CONFIDENCE` filtering
3. Unit tests with mock project/memory data

### Phase 6c: Delivery Transport and PTY Injection

1. `ContextTransport` trait (canonical definition, async, returns `DeliveryStatus`)
2. `PtyTransport` -- provider-aware injection:
   - Claude Code: `/read <path>` + inotify confirmation
   - Aider: `/add <path>` + inotify confirmation
   - Direct paste: content with `--- ZRemote Context ---` delimiters
3. `SessionWriterHandle` channel type
4. `DeliveryError` error type
5. File watcher resource limits (max 100 watched files, polling fallback)
6. Unit tests for markdown rendering, temp file lifecycle, per-provider injection format, delivery confirmation

### Phase 6d: Delivery Coordinator and Integration

1. `NudgeAccumulator` -- per-session debouncing with 2-second window, trigger merging with priority ordering
2. `DeliveryCoordinator` -- deferred nudge management (in-memory `HashMap`), phase-triggered delivery, optional audit log
3. `DeferredNudge` with expiry logic
4. Integration into `connection/mod.rs`:
   - Create coordinator in `run_connection()`
   - Wire phase changes to `on_phase_idle()`
   - Handle `SessionWriteRequest` in the main select loop
5. Wire `MemoryExtracted` events from `KnowledgeManager` to coordinator
6. Integration tests for the full flow: context change -> accumulate -> defer -> phase idle -> deliver (per provider)

---

## 6. Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| PTY injection disrupts agent flow | High | Only inject when agent is Idle (prompt detected). Never inject during Busy phase. Log every injection for debugging. |
| Provider injection strategy mismatch | Medium | `ProviderInjectionStrategy` auto-detected from `AgentInfo`. Falls back to `DirectPaste` (safe, works everywhere). Claude Code uses `/read`, Aider uses `/add`, others get direct paste with delimiters. |
| Token estimation inaccuracy | Low | Differentiated ratios (3.0 for code, 4.5 for prose, 3.5 mixed) with user-configurable override. Exact tokenization out of scope (would require provider-specific tokenizers). Budget is configurable. |
| Temp file cleanup races | Low | Unique filenames with timestamp. Cleanup after 60s via `tokio::spawn`. Stale files harmless (small, in temp dir). |
| Nudge storms from rapid changes | Medium | `NudgeAccumulator` debounces within 2-second window. One pending nudge per session (latest replaces). Nudge expiry at 5 minutes. |
| inotify watch exhaustion | Medium | Max 100 watched files (configurable). Overflow files use polling fallback (mtime check every 30s). Priority: config files first, then by change frequency. |
| SessionWriterHandle channel full | Low | Use `try_send` with bounded channel. If full, log warning and retry on next phase transition. Context is not lost (nudge remains pending). |
| Delivery confirmation false negative | Low | inotify may miss file access if agent reads file before watch is set up. Acceptable: `Unconfirmed` is logged but not acted upon. No retry avoids injection spam. |

---

## 7. Protocol Compatibility

This phase introduces **no protocol changes**. All new types are agent-internal:

- `SessionContext`, `ContextAssembler`, `TokenBudget`, `ContentType`, `ProviderInjectionStrategy` -- internal to agent process
- `DeliveryCoordinator`, `NudgeAccumulator` -- internal state machines
- `ContextTransport` trait, `PtyTransport` -- writes to local PTY, no network messages
- `DeliveryStatus` -- internal delivery result enum
- `SessionWriterHandle` -- internal mpsc channel

The only external effect is bytes written to the PTY stdin, which appear as normal user input from the session's perspective.

Future Channel Bridge integration (Phase 7) will add protocol messages, but that is scoped to the channel-bridge RFC.

---

## 8. Testing

### Unit Tests

| Test | Module | Description |
|------|--------|-------------|
| `token_estimate_empty` | `TokenBudget` | Empty string estimates to 0 tokens |
| `token_estimate_code_ratio` | `TokenBudget` | Code content uses 3.0 chars/token ratio |
| `token_estimate_prose_ratio` | `TokenBudget` | Natural language uses 4.5 chars/token ratio |
| `token_estimate_mixed_ratio` | `TokenBudget` | Mixed content uses 3.5 chars/token ratio |
| `token_estimate_custom_override` | `TokenBudget` | User-configured ratio overrides content type |
| `trim_within_budget` | `TokenBudget` | Context under budget is not modified |
| `trim_drops_conventions_first` | `TokenBudget` | Conventions removed before memories |
| `trim_drops_low_confidence_memories` | `TokenBudget` | Low-confidence memories removed first |
| `trim_truncates_as_last_resort` | `TokenBudget` | Entire payload truncated when nothing else fits |
| `assemble_filters_low_confidence` | `ContextAssembler` | Memories below 0.7 excluded |
| `assemble_sorts_by_confidence` | `ContextAssembler` | Highest confidence memories first |
| `assemble_empty_memories` | `ContextAssembler` | No memories produces valid context |
| `render_markdown_format` | `SessionContext` | Rendered output matches expected markdown structure |
| `render_with_trigger` | `SessionContext` | Trigger description appears in rendered output |
| `strategy_claude_code` | `ProviderInjectionStrategy` | Claude Code agent maps to `/read` strategy |
| `strategy_aider` | `ProviderInjectionStrategy` | Aider agent maps to `/add` strategy |
| `strategy_codex_direct_paste` | `ProviderInjectionStrategy` | Codex CLI maps to DirectPaste strategy |
| `strategy_gemini_direct_paste` | `ProviderInjectionStrategy` | Gemini CLI maps to DirectPaste strategy |
| `strategy_unknown_fallback` | `ProviderInjectionStrategy` | Unknown/None agent maps to DirectPaste |
| `accumulator_merges_within_window` | `NudgeAccumulator` | Multiple changes within 2s merged into one nudge |
| `accumulator_priority_ordering` | `NudgeAccumulator` | Memory updates prioritized over file/convention changes |
| `accumulator_debounce_resets` | `NudgeAccumulator` | New change resets the 2-second debounce timer |
| `deferred_nudge_replaces` | `DeliveryCoordinator` | Second nudge replaces first for same session |
| `deferred_nudge_expires` | `DeliveryCoordinator` | Nudge older than max_nudge_age is dropped |
| `on_phase_idle_delivers` | `DeliveryCoordinator` | Pending nudge delivered when phase transitions to Idle |
| `on_phase_busy_defers` | `DeliveryCoordinator` | Context change during Busy creates deferred nudge |
| `no_nudge_no_delivery` | `DeliveryCoordinator` | Phase transition without pending nudge is a no-op |
| `delivery_status_confirmed` | `PtyTransport` | File access within 5s returns `Delivered` |
| `delivery_status_unconfirmed` | `PtyTransport` | No file access within 5s returns `Unconfirmed` |
| `direct_paste_always_unconfirmed` | `PtyTransport` | DirectPaste strategy always returns `Unconfirmed` |

### Integration Tests

| Test | Description |
|------|-------------|
| `full_delivery_flow` | Assemble context -> defer (Busy) -> phase transition (Idle) -> verify PTY injection |
| `pty_injection_writes_file` | Verify temp file is created with correct markdown content |
| `pty_injection_claude_read_command` | Verify `/read <path>\n` is written to PTY stdin for Claude Code |
| `pty_injection_aider_add_command` | Verify `/add <path>\n` is written to PTY stdin for Aider |
| `pty_injection_codex_direct_paste` | Verify content pasted directly with delimiters for Codex CLI |
| `pty_injection_unknown_direct_paste` | Verify DirectPaste fallback for unknown provider |
| `cleanup_removes_temp_file` | Verify temp file is removed after cleanup delay |
| `writer_handle_channel` | Verify `SessionWriterHandle` sends write requests through channel |
| `accumulator_debounce_integration` | Rapid events within 2s produce single delivery |

### Manual / E2E Verification

1. Start local mode, launch Claude Code on a project with existing memories
2. Extract a new memory from a completed loop on the same project
3. Verify nudge is deferred while Claude Code is busy
4. Wait for Claude Code to return to prompt (Idle phase)
5. Verify `/read` command appears in terminal output
6. Verify temp file contains assembled context in markdown format
7. Verify temp file is cleaned up after ~60 seconds
