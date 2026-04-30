# RFC-009: Activity Panel — Hooks as Primary Source, Real-Time Status

## Context

The Activity Panel (`crates/zremote-gui/src/views/activity_panel.rs`) was built to show a live, structured feed of Claude Code's execution: tool calls (Read / Edit / Write / Bash), shell commands, and agent_response nodes — alongside the terminal. In practice the body always shows "No activity yet" while the header (status, model, %, $) updates correctly.

### Root cause

Today the **only** source of `execution_nodes` is the PTY `OutputAnalyzer` (`crates/zremote-agent/src/agentic/analyzer.rs`):

- `NodeBuilder.on_tool_call()` is fed by regex matches on PTY output (`patterns::TOOL_CALL_RE`). CC's modern TUI uses Unicode box-drawing and ANSI overlays — most tool calls never match.
- `NodeBuilder.complete_if_building()` only runs from `AnalyzerEvent::PhaseChanged(Idle)` or OSC 133 prompt markers. When `SessionMapper::is_hook_mode` is true (the default whenever zremote hooks are installed), `connection/mod.rs:145` swallows `PhaseChanged` events. Nodes that *are* started never get drained.
- The hook handlers (`crates/zremote-agent/src/hooks/handler.rs`) parse `PreToolUse` / `PostToolUse` and emit only `LoopStateUpdate` — they never emit `AgenticAgentMessage::ExecutionNode`. The hook payload's `tool_use_id` is parsed but discarded.

### Goals

1. **Two-source pipeline** — CC hooks drive nodes when available, PTY analyzer is the fallback for non-CC agents and plain shell sessions.
2. **Real-time progress** — the moment CC starts a tool, the panel shows a "running" row. The moment the tool finishes, the row updates with output and exit code. No buffering, no waiting for completion.
3. **Crash-safe** — pending nodes survive agent restarts. State lives in the DB, not a process-memory map.
4. **One DB row per logical tool invocation** — INSERT on PreToolUse, UPDATE on PostToolUse. Correlation by `(session_id, tool_use_id)`.

### Source ↔ kind matrix

| Source | Kinds emitted | Active when |
|---|---|---|
| **CC hooks** (primary) | `tool_call` (with normalized tool name lowercased) | `SessionMapper::is_hook_mode(session)` |
| **PTY analyzer** (fallback) | `tool_call`, `agent_response` | `!is_hook_mode(session)` |
| **PTY OSC 133** (always-on) | `shell_command` | always (only fires in plain shell PTY anyway) |

## Architecture

```
                 ┌─────────────────────────────────────────┐
                 │           Claude Code TUI               │
                 └──────────────┬──────────────────────────┘
                                │
          ┌─────────────────────┼─────────────────────┐
          │ PTY bytes           │ Hook HTTP POSTs     │
          ▼                     │                     ▼
   ┌──────────────────┐         │             ┌─────────────────┐
   │ OutputAnalyzer   │         │             │  hooks/handler  │
   │ (fallback)       │         │             │                 │
   │  → NodeCompleted │         │             │ PreToolUse →    │
   │                  │         │             │   Opened        │
   │  if hook_mode &  │         │             │                 │
   │  kind ∈ tool/    │         │             │ PostToolUse →   │
   │  agent_response: │         │             │   Closed        │
   │  drop            │         │             │                 │
   │  else: synth     │         │             │ Stop / kill →   │
   │  tool_use_id,    │         │             │   SessionStop   │
   │  emit Opened+    │         │             │     (closes     │
   │  Closed pair     │         │             │      orphans)   │
   └──────┬───────────┘         │             └────────┬────────┘
          │                     │                      │
          ▼                     │                      ▼
   ┌─────────────────────────────────────────────────────────────┐
   │         AgenticAgentMessage variants (unified)              │
   │   ExecutionNodeOpened    (PreToolUse / PTY tool start)      │
   │   ExecutionNodeClosed    (PostToolUse / PTY tool finish)    │
   │   SessionExecutionStopped (Stop hook closes running rows)   │
   └────────────────────────┬────────────────────────────────────┘
                            │
                            ▼
              local/tasks.rs    OR    server/dispatch.rs
                            │
                            ▼
       ┌───────────────────────────────────────────────────┐
       │          execution_nodes (SQLite)                 │
       │  status TEXT NOT NULL                             │
       │  tool_use_id TEXT NOT NULL                        │
       │  status ∈ {running, completed, stopped, stale}    │
       └───────────────────────┬───────────────────────────┘
                               │
                               ▼
            ┌──────────────────────────────────────┐
            │    ServerEvent variants              │
            │  ExecutionNodeCreated  (INSERT done) │
            │  ExecutionNodeUpdated  (UPDATE done) │
            └──────────────────┬───────────────────┘
                               │
                               ▼
                  GUI: ActivityPanel
                  · Created → push_node(running spinner)
                  · Updated → find by node_id, mutate fields
                              (kind, output, exit, duration, status)
```

### Status values

| Status | Meaning | How it ends up here |
|---|---|---|
| `running` | Tool is executing right now | INSERT from `ExecutionNodeOpened` (PreToolUse or PTY tool start) |
| `completed` | Tool finished, output captured | UPDATE from `ExecutionNodeClosed{status: Completed}` |
| `stopped` | Session stopped (Stop hook, kill, manual abort) before PostToolUse arrived | UPDATE from `SessionExecutionStopped` for any rows still `running` |
| `stale` | Server-side TTL eviction — `running` for longer than 10 min, no PostToolUse, no Stop | Background sweeper |

### No backward compatibility

This is a clean cut. All deployed agents, servers, and GUIs must be upgraded together. The legacy `AgenticAgentMessage::ExecutionNode` variant is **deleted** in this RFC — both the hook path and the PTY analyzer path emit the same new message pair (`ExecutionNodeOpened` + `ExecutionNodeClosed`).

- DB migration drops nothing — old rows are wiped (see migration). Activity history before the upgrade is intentionally lost; it's already cosmetic with no real value.
- Protocol: legacy variants removed. Old agents talking to a new server, or new agents to an old server, will fail to parse messages and refuse the connection — desired, forces redeployment.
- GUI: `ServerEvent::ExecutionNodeCreated` reshaped with required fields. Old GUI builds will fail to parse — also desired.

## Phase breakdown

Three phases. Phase 1 (protocol/DB) and Phase 3 (GUI) can run in parallel after Phase 2 lands. Each phase has its own worktree.

---

### Phase 1: Protocol + DB schema

**Worktree**: `rfc-009-p1-protocol`

**Files**:

#### CREATE
- `crates/zremote-core/migrations/026_execution_node_status.sql`
  ```sql
  -- Wipe legacy rows. Activity history before the schema upgrade is discarded.
  DELETE FROM execution_nodes;

  ALTER TABLE execution_nodes ADD COLUMN status TEXT NOT NULL DEFAULT 'completed';
  ALTER TABLE execution_nodes ADD COLUMN tool_use_id TEXT NOT NULL DEFAULT '';

  -- Now drop the defaults (SQLite trick: column added with DEFAULT cannot
  -- have it removed; we accept the default in the schema and rely on the
  -- application layer to always supply explicit values going forward).
  CREATE UNIQUE INDEX idx_execution_nodes_tool_use_id
    ON execution_nodes (session_id, tool_use_id)
    WHERE tool_use_id != '';

  CREATE INDEX idx_execution_nodes_running
    ON execution_nodes (session_id, status)
    WHERE status = 'running';
  ```
  - The unique index on `(session_id, tool_use_id)` enforces idempotency: a duplicate PreToolUse hook becomes an `INSERT … ON CONFLICT DO NOTHING`.
  - The partial index on `status='running'` keeps the sweeper query cheap.

#### MODIFY

| File | Change |
|---|---|
| `crates/zremote-core/src/queries/execution_nodes.rs` | Replace `insert_execution_node` with `open_execution_node(... status='running' ...)`. Add `close_execution_node(session_id, tool_use_id, ...)` that UPDATEs by tool_use_id. Add `mark_session_running_as_stopped(session_id)`. Add `sweep_stale_running(ttl_secs) -> Vec<ExecutionNodeRow>`. Add `status` and `tool_use_id` columns to `ExecutionNodeRow`. Update list queries' SELECTs. |
| `crates/zremote-protocol/src/agentic.rs` | **Delete** `AgenticAgentMessage::ExecutionNode` variant. Add `ExecutionNodeOpened`, `ExecutionNodeClosed`, `SessionExecutionStopped`. |
| `crates/zremote-protocol/src/events.rs` | Reshape `ServerEvent::ExecutionNodeCreated` with required `status: NodeStatus`, `tool_use_id: String`. Add `ServerEvent::ExecutionNodeUpdated { session_id, host_id, node_id, tool_use_id, status, kind, output_summary, exit_code, duration_ms }`. Add `pub enum NodeStatus { Running, Completed, Stopped, Stale }` with `#[serde(rename_all = "snake_case")]`. |
| `crates/zremote-cli/src/commands/events.rs:77` | Replace mapping for old event with both new string mappings. |

#### Function signatures

```rust
// crates/zremote-core/src/queries/execution_nodes.rs

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ExecutionNodeRow {
    pub id: i64,
    pub session_id: String,
    pub loop_id: Option<String>,
    pub timestamp: i64,
    pub kind: String,
    pub input: Option<String>,
    pub output_summary: Option<String>,
    pub exit_code: Option<i32>,
    pub working_dir: String,
    pub duration_ms: i64,
    pub status: String,        // "running" | "completed" | "stopped" | "stale"
    pub tool_use_id: String,   // empty string for legacy / non-hook PTY path
                                // (we still always synthesize one, see Phase 2)
}

/// INSERT a new execution node in the `running` state. Idempotent on
/// `(session_id, tool_use_id)`. Returns the row id (existing or newly inserted).
#[allow(clippy::too_many_arguments)]
pub async fn open_execution_node(
    pool: &SqlitePool,
    session_id: &str,
    loop_id: Option<&str>,
    tool_use_id: &str,
    timestamp: i64,
    kind: &str,
    input: Option<&str>,
    working_dir: &str,
) -> Result<i64, AppError>;

/// UPDATE a running node to a terminal state by `(session_id, tool_use_id)`.
/// Returns the row if found, None if no matching running node exists.
pub async fn close_execution_node(
    pool: &SqlitePool,
    session_id: &str,
    tool_use_id: &str,
    kind: &str,
    output_summary: Option<&str>,
    exit_code: Option<i32>,
    duration_ms: i64,
    status: NodeStatus,        // Completed | Stopped | Stale
) -> Result<Option<ExecutionNodeRow>, AppError>;

/// Bulk close every still-running node in a session as `stopped`.
/// Returns the affected rows for broadcast.
pub async fn mark_session_running_as_stopped(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<ExecutionNodeRow>, AppError>;

/// Find all `running` nodes older than `ttl_secs`, mark them `stale`,
/// return them for broadcast.
pub async fn sweep_stale_running(
    pool: &SqlitePool,
    ttl_secs: i64,
) -> Result<Vec<ExecutionNodeRow>, AppError>;
```

```rust
// crates/zremote-protocol/src/agentic.rs

pub enum AgenticAgentMessage {
    // ... LoopDetected, LoopStateUpdate, LoopEnded, LoopMetricsUpdate ...
    // (legacy ExecutionNode variant DELETED)

    /// PreToolUse hook, or PTY analyzer at tool start.
    ExecutionNodeOpened {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_id: Option<AgenticLoopId>,
        tool_use_id: String,          // CC payload id, or "pty-{uuid}" for PTY path
        timestamp: i64,
        kind: String,                 // lowercase: "read", "edit", "bash", "agent_response", "shell_command"
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        working_dir: String,
    },

    /// PostToolUse hook, or PTY analyzer at tool finish.
    ExecutionNodeClosed {
        session_id: SessionId,
        tool_use_id: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        duration_ms: i64,
        status: NodeStatus,            // Completed | Stopped | Stale (Stale only via sweeper, not from agent)
    },

    /// Stop / StopFailure hook: close every running node for this session.
    SessionExecutionStopped {
        session_id: SessionId,
    },
}
```

```rust
// crates/zremote-protocol/src/events.rs

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Running,
    Completed,
    Stopped,
    Stale,
}

pub enum ServerEvent {
    // ... existing ...

    /// A new execution node was inserted (status='running' for hooks/PTY-start,
    /// can also arrive as 'completed' if the agent emits Opened+Closed back-to-back).
    ExecutionNodeCreated {
        session_id: String,
        host_id: String,
        node_id: i64,
        tool_use_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loop_id: Option<String>,
        timestamp: i64,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        working_dir: String,
        status: NodeStatus,
    },

    /// An existing execution node transitioned (running → completed/stopped/stale)
    /// or had its fields updated. GUI matches by node_id.
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
}
```

**Tests** (Phase 1):

| # | Test | File |
|---|---|---|
| 1 | Migration applies cleanly to a fresh DB and to one with legacy rows (legacy rows are wiped) | new test in `queries/execution_nodes.rs` |
| 2 | `open_execution_node` inserts a `running` row and returns id | `queries/execution_nodes.rs` |
| 3 | `open_execution_node` is idempotent on duplicate `(session_id, tool_use_id)` (returns existing id) | same |
| 4 | `close_execution_node` finds running node, transitions to `completed`, returns the row | same |
| 5 | `close_execution_node` with no matching tool_use_id returns `None` | same |
| 6 | `close_execution_node` does not transition rows already in `completed` (idempotent) | same |
| 7 | `mark_session_running_as_stopped` only touches rows with status='running' for the given session | same |
| 8 | `sweep_stale_running` ignores `completed`/`stopped` rows, picks up old `running` ones, returns rows | same |
| 9 | Unique index rejects duplicate `(session_id, tool_use_id)` pairs (open is idempotent) | same |
| 10 | Round-trip serialization for `ExecutionNodeOpened`, `ExecutionNodeClosed`, `SessionExecutionStopped` | `protocol/agentic.rs` |
| 11 | Round-trip for `ServerEvent::ExecutionNodeCreated` and `ExecutionNodeUpdated` | `protocol/events.rs` |
| 12 | `NodeStatus` serializes as snake_case (`"running"`, `"completed"`, `"stopped"`, `"stale"`) | `protocol/events.rs` |

**Reviewers**: `rust-reviewer` + `code-reviewer`. **Block merge** on missing tests / missing wiring.

---

### Phase 2: Agent hooks + suppression + sweeper

**Worktree**: `rfc-009-p2-agent`. Depends on Phase 1 merged.

**Files**:

#### MODIFY

| File | Change |
|---|---|
| `crates/zremote-agent/src/hooks/handler.rs` | PreToolUse → emit `ExecutionNodeOpened` (after existing `LoopStateUpdate`). PostToolUse → emit `ExecutionNodeClosed{status: Completed}`. Stop / StopFailure → emit `SessionExecutionStopped`. Empirical verification of `tool_use_id` and `tool_response` field names as the **first commit** in this worktree (see Open Questions). |
| `crates/zremote-agent/src/hooks/state.rs` | Plumb `agentic_tx` (mpsc sender for `AgenticAgentMessage`) into `HooksState`. |
| `crates/zremote-agent/src/connection/mod.rs:200` | Replace `AnalyzerEvent::NodeCompleted` arm: synthesize `tool_use_id = format!("pty-{}", Uuid::new_v4())`, emit `ExecutionNodeOpened` then immediately `ExecutionNodeClosed{status: Completed}`. Suppress entirely when `is_hook_mode(session)` AND `node.kind ∈ {"tool_call", "agent_response"}`. Keep `shell_command` unsuppressed. |
| `crates/zremote-agent/src/local/tasks.rs:287` | Same replacement + suppression. |
| `crates/zremote-server/src/routes/agents/dispatch.rs:1687` | Replace `ExecutionNode` arm with three new arms: `ExecutionNodeOpened` → INSERT + emit `ExecutionNodeCreated`. `ExecutionNodeClosed` → UPDATE + emit `ExecutionNodeUpdated`. `SessionExecutionStopped` → `mark_session_running_as_stopped` + emit one `ExecutionNodeUpdated` per affected row. |
| `crates/zremote-core/src/processing/agentic.rs:148` | Same three new arms for local-mode `AgenticProcessor`. |
| `crates/zremote-agent/src/local/mod.rs` (and server-mode startup) | Spawn a `Task<()>` that runs `sweep_stale_running(600)` every 60 s, emits `ExecutionNodeUpdated{status: Stale}` for each row. Stored on app state per `CLAUDE.md` async ownership rule. |
| `crates/zremote-agent/src/agentic/analyzer.rs` | Lowercase normalization: when the regex captures a tool name, store `tool.to_lowercase()` as `kind`. NodeBuilder gains a `synthetic_tool_use_id: String` field generated at `on_tool_call` / `on_phase_changed(Busy)` / `on_prompt_markers(command_starts)` and emitted on `CompletedNode`. |

#### Tool name & input formatting (in handler.rs)

```rust
// crates/zremote-agent/src/hooks/handler.rs

/// Pretty input string for an opening node. Falls back to compact JSON.
fn format_tool_input(tool_name: &str, tool_input: Option<&serde_json::Value>) -> Option<String> {
    let v = tool_input?;
    let s = match tool_name {
        "Read" | "Edit" | "Write" | "MultiEdit" =>
            v.get("file_path").and_then(Value::as_str).map(str::to_string),
        "Bash" =>
            v.get("command").and_then(Value::as_str).map(str::to_string),
        "Glob" | "Grep" =>
            v.get("pattern").and_then(Value::as_str).map(str::to_string),
        "Task" => {
            let agent = v.get("subagent_type").and_then(Value::as_str).unwrap_or("agent");
            let prompt = v.get("prompt").and_then(Value::as_str).unwrap_or("");
            Some(format!("{agent}: {}", truncate(prompt, 60)))
        }
        "WebFetch" =>
            v.get("url").and_then(Value::as_str).map(str::to_string),
        _ => Some(serde_json::to_string(v).unwrap_or_default()),
    };
    s.map(|s| truncate(&s, INPUT_CAP_BYTES))
}

fn format_tool_response(
    tool_response: Option<&serde_json::Value>,
    is_error: bool,
) -> Option<String> { /* see RFC narrative — handles is_error prefix, stdout/content/result fallbacks, byte-cap with ellipsis */ }

const INPUT_CAP_BYTES: usize = 1024;
const SUMMARY_CAP_BYTES: usize = 4096;
```

**Tests** (Phase 2):

| # | Test | File |
|---|---|---|
| 13 | `format_tool_input("Read", {"file_path":"/x"})` → `"/x"` | `handler.rs` |
| 14 | `format_tool_input("Bash", long command)` truncated to `INPUT_CAP_BYTES` with ellipsis | `handler.rs` |
| 15 | `format_tool_response` of large stdout truncated to `SUMMARY_CAP_BYTES` | `handler.rs` |
| 16 | `format_tool_response` with `is_error=true` prefixes "ERROR: " | `handler.rs` |
| 17 | PreToolUse handler with valid mapped session: `LoopStateUpdate` AND `ExecutionNodeOpened` both fire on `agentic_tx` | extend `hooks/server.rs` test infra at line 165 |
| 18 | PostToolUse handler emits `ExecutionNodeClosed{status: Completed}` with matching `tool_use_id` | same |
| 19 | Stop hook handler emits `SessionExecutionStopped` | same |
| 20 | `connection::handle_analyzer_event` with `NodeCompleted{kind:"tool_call"}` is suppressed when `is_hook_mode` is true | `connection/mod.rs` (new test) |
| 21 | Same with `kind:"shell_command"` is **not** suppressed | `connection/mod.rs` |
| 22 | PTY fallback path: `NodeCompleted` (not in hook_mode) emits `Opened` + `Closed{status: Completed}` with synthesized `tool_use_id` | `connection/mod.rs` |
| 23 | End-to-end: PreToolUse → row inserted with status='running' → PostToolUse → row updated to status='completed' | `local/routes/sessions.rs` (existing pattern at line 1700) |
| 24 | End-to-end: PreToolUse → Stop → row marked status='stopped' | same |
| 25 | Stale sweep: row with status='running' older than TTL → 'stale' + `ExecutionNodeUpdated` event | `processing/agentic.rs` |
| 26 | PreToolUse with unknown CC session_id (no mapping after 5 s retry) is dropped with `tracing::warn!` | `hooks/server.rs` |
| 27 | PreToolUse with missing `tool_use_id` is dropped with `tracing::warn!` | `hooks/server.rs` |
| 28 | Duplicate PreToolUse delivery (same `tool_use_id`) is idempotent — ON CONFLICT path returns existing id, no second `ExecutionNodeCreated` event | `processing/agentic.rs` |

**Reviewers**: `rust-reviewer` + `security-reviewer` (hooks endpoint is a network surface) + `code-reviewer`. **Block merge** on issues.

---

### Phase 3: GUI

**Worktree**: `rfc-009-p3-gui`. Depends on Phase 1 merged. Can run in parallel with Phase 2.

**Files**:

#### MODIFY

| File | Change |
|---|---|
| `crates/zremote-client/src/types.rs:496` | Add `status: NodeStatus` and `tool_use_id: String` (re-exported from protocol) to `ExecutionNode` client type. |
| `crates/zremote-gui/src/views/activity_panel.rs` | `ExecutionNodeItem` gains `status: NodeStatus` and `tool_use_id: String`. `kind_label` rewritten for lowercase: handle `"bash"`, `"read"`, `"edit"`, `"write"`, `"glob"`, `"grep"`, `"task"`, `"webfetch"`, `"todowrite"`, MCP names (`mcp__*`), `"agent_response"`, `"shell_command"`. Fallback: capitalize first letter. New method `update_node(node_id, status, kind, output, exit, duration)`. Render running rows with a spinning Lucide `Loader` icon and a subtle accent border; stopped rows with `CircleSlash`; stale rows with `AlertCircle`. Animation: GPUI `Animation` primitive on the loader icon only. |
| `crates/zremote-gui/src/views/main_view.rs:681` | Match `ServerEvent::ExecutionNodeCreated` (already handled — extend to forward `status`, `tool_use_id`). Add new arm `ServerEvent::ExecutionNodeUpdated` → `terminal.update(...).panel.update_execution_node(...)`. |
| `crates/zremote-gui/src/views/terminal_panel.rs:587` | Add `update_execution_node(...)` method that forwards to `ActivityPanel::update_node`. |
| `crates/zremote-gui/src/icons.rs` | Add `Loader` icon (Lucide `loader-2.svg`) and `CircleSlash`, `AlertCircle` if missing. Add SVGs to `assets/icons/`. |

#### Function signatures

```rust
// crates/zremote-gui/src/views/activity_panel.rs

use zremote_protocol::NodeStatus;  // re-exported from Phase 1

pub struct ExecutionNodeItem {
    pub node_id: i64,
    pub tool_use_id: String,
    pub timestamp: i64,
    pub exit_code: Option<i32>,
    pub status: NodeStatus,
    pub display_icon: Icon,
    pub display_label: String,
    pub display_duration: String,
    pub display_input: Option<String>,
    pub display_summary: Option<String>,
}

impl ActivityPanel {
    /// Update an existing node in place by `node_id`. Returns true if updated.
    pub fn update_node(
        &mut self,
        node_id: i64,
        status: NodeStatus,
        kind: &str,
        output_summary: Option<&str>,
        exit_code: Option<i32>,
        duration_ms: i64,
        cx: &mut Context<Self>,
    ) -> bool;
}
```

**Tests** (Phase 3):

| # | Test | File |
|---|---|---|
| 29 | `update_node` matches by `node_id` and mutates fields, returns true | `activity_panel.rs` |
| 30 | `update_node` no-op when no row matches, returns false | `activity_panel.rs` |
| 31 | `kind_label` for new lowercase tool names (`"bash"`, `"read"`, `"task"`, `"webfetch"`, MCP `"mcp__plugin__tool"`) | `activity_panel.rs` |
| 32 | `kind_label` fallback capitalizes unknown lowercase strings | `activity_panel.rs` |
| 33 | `ExecutionNodeItem` with `NodeStatus::Running` chooses spinner icon | `activity_panel.rs` |
| 34 | `ExecutionNodeItem` with `NodeStatus::Stopped` / `Stale` chooses distinct icons | `activity_panel.rs` |

**UX checklist** (mandatory per `CLAUDE.md` UX bar):
- Running indicator visible (spinner icon, accent border) — visual feedback < 100 ms after PreToolUse arrives
- Stopped/stale rows distinct from completed (different icon + color)
- Empty state still shows when no nodes exist
- No layout shift when a row transitions Running → Completed (icon swap is in-place; duration & summary expand the row)
- All colors via `theme::*()`, all icons via `icon(Icon::X)`, all sizing via `px()`
- Spinner animation does not re-render the entire panel each frame (use GPUI's `Animation` API on the icon node only)

**Reviewers**: `rust-reviewer` + UX review teammate (mandatory per `CLAUDE.md` for UI phases) + `code-reviewer`.

---

### Phase 4 (post-merge): verification

Per `CLAUDE.md` verification protocol:

1. Read full `git diff main...HEAD` of merged Phase 1+2+3.
2. Grep new code for `unwrap()` / `expect()` / `todo!()` / `unimplemented!()` — every occurrence justified.
3. Run full workspace tests: `nix develop --command cargo test --workspace`.
4. Confirm 34 tests from RFC test plan exist by name (grep).
5. End-to-end manual verification with `/visual-test`:
   - `nix develop && cargo run -p zremote -- gui --local`
   - Open a Claude Code session in the embedded terminal
   - Issue a tool (`@README.md`, ask CC to read a file)
   - **Confirm running spinner appears within 100 ms of CC starting the tool**
   - Confirm spinner replaced by check / X icon when tool finishes, with correct kind, input, summary, duration, exit code
   - Confirm only one row appears (no duplicate from PTY analyzer fallback)
   - Kill CC mid-tool-call (`pkill claude`) → row transitions Running → Stopped within 1 s of the Stop hook firing
   - Kill agent mid-tool-call (`pkill zremote-agent`) → restart agent → after 10 min the row transitions Running → Stale via the sweeper. Confirm GUI receives `ExecutionNodeUpdated` and the row updates without a manual reload.
   - Open a plain shell session (no CC) → run `ls`, `pwd` → `shell_command` nodes still appear (fallback path)

---

## Open questions (must be resolved before / during Phase 2)

### Q1: Subagent (Task tool) session_id — verify empirically *(blocking Phase 2)*

When CC spawns a subagent via the Task tool, every inner tool call fires its own PreToolUse / PostToolUse. **Open**: does the hook payload carry the parent session_id or a separate subagent session_id?

- If parent → key `(session_id, tool_use_id)` works as designed.
- If separate → `SessionMapper::try_resolve` returns `None` and we drop the hook. Subagent activity is invisible.

**Resolution**: first commit on `rfc-009-p2-agent` worktree adds `tracing::info!` of the raw hook JSON for every PreToolUse / PostToolUse during a subagent run, captures sample payloads, and updates this RFC + handler.rs accordingly. If subagent uses a separate session_id, we extend `SessionMapper` with a parent-lookup fallback.

### Q2: `tool_use_id` field name — verify

Test fixture in `crates/zremote-agent/src/hooks/server.rs:317` uses `tool_result`; current handler at `handler.rs:48` parses `tool_response`. Either CC changed the field or the test fixture is wrong. **Resolution**: same first commit on Phase 2 worktree — log raw payloads, update `HookPayload` to match real field names. Decision: drop hook + `tracing::warn!` if `tool_use_id` is `None`.

### Q3: Agentic loop registration race — handled by retry

PreToolUse may arrive before the PTY detector has registered `cc_session_id → SessionId`. **Resolution**: use `SessionMapper::resolve_loop_id` (5 s retry) for hook resolution, not `try_resolve`. CC's hook timeout is ~30 s, so 5 s is safe.

### Q4: Permission denied via Notification

When user denies a permission prompt, what does CC actually fire? **Best-known behavior**: Notification → Stop. The Stop hook will emit `SessionExecutionStopped`, which transitions the running node to `stopped`. UI shows the row as stopped (not stale). Acceptable.

If empirical testing in Phase 2 (Q1 / Q2 step) reveals CC fires PostToolUse with `is_error=true` instead, the existing `ExecutionNodeClosed` path handles that — `is_error` just goes into `output_summary` with an `ERROR:` prefix.

---

## Risk register

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R1 | Lost nodes on agent crash | **Resolved** | Pending state lives in DB (status='running'). Agent restart finds them via the sweeper. |
| R2 | Memory growth | Low | No in-memory state. DB rows are capped at 10,000 per session by `enforce_session_node_cap`. |
| R3 | Out-of-order hook delivery | Low | CC waits for PreToolUse response before launching the tool, so PreToolUse always lands first per `tool_use_id`. If somehow re-ordered, `close_execution_node` returns None for unknown id and we `tracing::warn!` — no data corruption. |
| R4 | Duplicate hook delivery | Low | INSERT is idempotent via the unique index on `(session_id, tool_use_id)` — second INSERT becomes `ON CONFLICT DO NOTHING` and returns the existing id. UPDATE is naturally idempotent. |
| R5 | Stale sweeper racing with a slow PostToolUse | Medium | TTL 10 min is well above any normal tool. If a tool genuinely takes >10 min (long `cargo build`), sweeper marks it stale — when PostToolUse finally arrives, `close_execution_node` finds no `running` row and we `tracing::warn!` + drop. **Acceptable false negative.** Tunable via config later. |
| R6 | Subagent session_id mismatch | High *(blocks)* | See Q1 — first commit on Phase 2 verifies empirically and updates RFC. |
| R7 | Sweeper task lifetime leak | Low | Stored as `Task<()>` field on app state per `CLAUDE.md` async ownership rule. Cancelled on shutdown via `CancellationToken`. |
| R8 | Coordinated rollout required | Accepted | All agents, server, and GUI built from the same commit must be deployed together. No graceful protocol fallback — old/new mix will refuse to talk. Per user direction, this is preferred over compat baggage. |
| R9 | Sweeper writes contention with hook handlers | Low | Sweeper runs every 60 s, single transaction, indexed UPDATE. Hook write rate is bounded by CC's tool call rate (typically <10/min). No realistic contention. |

---

## Out of scope

- Streaming partial output for long-running tools (live tail of `cargo build`).
- Capturing `Notification` / `Elicitation` hooks as nodes (those are status events, not actions).
- Aggregating subagent (Task tool) sub-tools into a tree under the parent — flat list for now.
- MCP tool name pretty-printing (`mcp__plugin_x__y` rendered as-is initially; cosmetic-only follow-up).

## Approval

Pending user sign-off before kicking off Phase 1.
