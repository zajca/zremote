# Phase 4: Agentic Loop Support

**Goal:** Detect and monitor AI agentic tools (Claude Code, Codex, etc.) running in terminal sessions. Provide real-time control panel with approve/reject actions, tool call queue, transcript view, context usage tracking, and cost estimation.

**Dependencies:** Phase 2 (PTY sessions), Phase 3 (UI foundation)

---

## 4.1 Protocol Extensions for Agentic Messages

**Files:** `crates/myremote-protocol/src/lib.rs` (consider splitting into `lib.rs`, `terminal.rs`, `agentic.rs`)

- [ ] Add `AgenticLoopId = Uuid` type alias
- [ ] New `AgentMessage` variants:
  - `AgenticLoopDetected { loop_id: AgenticLoopId, session_id: SessionId, project_path: String, tool_name: String, model: String }`
  - `AgenticLoopStateUpdate { loop_id: AgenticLoopId, status: AgenticStatus, current_step: Option<String>, context_usage_pct: f32, total_tokens: u64, estimated_cost_usd: f64, pending_tool_calls: u32 }`
  - `AgenticLoopToolCall { loop_id: AgenticLoopId, tool_call_id: Uuid, tool_name: String, arguments_json: String, status: ToolCallStatus }`
  - `AgenticLoopToolResult { loop_id: AgenticLoopId, tool_call_id: Uuid, result_preview: String, duration_ms: u64 }`
  - `AgenticLoopTranscript { loop_id: AgenticLoopId, role: TranscriptRole, content: String, tool_call_id: Option<Uuid>, timestamp: DateTime<Utc> }`
  - `AgenticLoopMetrics { loop_id: AgenticLoopId, tokens_in: u64, tokens_out: u64, model: String, context_used: u64, context_max: u64, estimated_cost_usd: f64 }`
  - `AgenticLoopEnded { loop_id: AgenticLoopId, reason: String, summary: Option<String> }`
- [ ] New `ServerMessage` variants:
  - `AgenticLoopUserAction { loop_id: AgenticLoopId, action: UserAction, payload: Option<String> }`
  - `PermissionRulesUpdate { rules: Vec<PermissionRule> }`
- [ ] Supporting types:
  - `AgenticStatus` enum: `Working`, `WaitingForInput`, `Paused`, `Error`, `Completed`
  - `ToolCallStatus` enum: `Pending`, `Approved`, `Rejected`, `Running`, `Completed`, `Failed`
  - `UserAction` enum: `Approve`, `Reject`, `ProvideInput`, `Pause`, `Resume`, `Stop`
  - `PermissionRule` struct: `{ tool_pattern: String, action: PermissionAction }` where `PermissionAction` = `AutoApprove | Ask | Deny`
  - `TranscriptRole` enum: `Assistant`, `User`, `Tool`, `System`
- [ ] Serialization tests for all new types

---

## 4.2 Agent: Agentic Loop Detection

**Files:** `crates/myremote-agent/src/agentic/{mod.rs, detector.rs, manager.rs, claude_code.rs, types.rs}`

- [ ] `detector.rs` -- Process tree inspection:
  - Periodically check child processes of PTY shell (every 2-3s)
  - Known tool signatures: `claude` (Claude Code), `codex` (OpenAI Codex), `gemini-cli`, `aider`
  - Use `sysinfo` crate or `/proc` filesystem to inspect process names
  - Return detected tool name + PID
- [ ] `types.rs` -- Internal event types:
  - `AgenticEvent` enum: `Detected`, `StatusChanged`, `ToolCallDetected`, `ToolCallResolved`, `TranscriptEntry`, `MetricsUpdate`, `Ended`
- [ ] `AgenticToolAdapter` trait:
  - `fn detect(process_name: &str, output: &[u8]) -> bool` -- does this output look like it comes from this tool?
  - `fn parse_event(output: &[u8]) -> Vec<AgenticEvent>` -- extract structured events from terminal output
  - `fn translate_action(action: UserAction) -> Vec<u8>` -- convert user action to PTY input bytes ("y\n" for approve, etc.)
  - `fn name() -> &'static str`
- [ ] `claude_code.rs` -- Claude Code adapter:
  - Detect: look for Claude Code prompts, tool call patterns in output
  - Parse: extract tool names, arguments, permission prompts, context usage, errors
  - State machine: Idle -> Working -> WaitingForApproval -> Working -> Completed
  - Consider: if Claude Code supports `--output-format stream-json`, use that as primary data source (much more reliable than terminal parsing)
  - Translate actions: "y\n" for approve, "n\n" for reject, typed text + "\n" for input
- [ ] `manager.rs` -- Agentic loop manager:
  - Per-session: when detector identifies agentic process, create `AgenticLoopId`
  - Send `AgentMessage::AgenticLoopDetected` to server
  - Forward parsed events as appropriate `AgentMessage` variants
  - Handle `ServerMessage::AgenticLoopUserAction`: translate via adapter, write to PTY
  - Track active loops per session, cleanup on session close
- [ ] Terminal output parsing confidence: never block terminal output while parsing, graceful fallback if parsing fails

---

## 4.3 Server: Agentic State & API

**Files:** `crates/myremote-server/src/state/agentic.rs`, `routes/agentic.rs`, `migrations/002_agentic.sql`

- [ ] Add `dashmap` crate to server dependencies
- [ ] `002_agentic.sql` migration:
  ```sql
  CREATE TABLE agentic_loops (
      id TEXT PRIMARY KEY,
      session_id TEXT NOT NULL REFERENCES sessions(id),
      project_path TEXT,
      tool_name TEXT NOT NULL,
      model TEXT,
      status TEXT NOT NULL DEFAULT 'working',
      started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
      ended_at TEXT,
      total_tokens_in INTEGER DEFAULT 0,
      total_tokens_out INTEGER DEFAULT 0,
      estimated_cost_usd REAL DEFAULT 0.0,
      end_reason TEXT,
      summary TEXT
  );

  CREATE TABLE tool_calls (
      id TEXT PRIMARY KEY,
      loop_id TEXT NOT NULL REFERENCES agentic_loops(id) ON DELETE CASCADE,
      tool_name TEXT NOT NULL,
      arguments_json TEXT,
      status TEXT NOT NULL DEFAULT 'pending',
      result_preview TEXT,
      duration_ms INTEGER,
      created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
      resolved_at TEXT
  );

  CREATE TABLE transcript_entries (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      loop_id TEXT NOT NULL REFERENCES agentic_loops(id) ON DELETE CASCADE,
      role TEXT NOT NULL,
      content TEXT NOT NULL,
      tool_call_id TEXT,
      timestamp TEXT NOT NULL
  );

  CREATE INDEX idx_agentic_loops_session_id ON agentic_loops(session_id);
  CREATE INDEX idx_tool_calls_loop_id ON tool_calls(loop_id);
  CREATE INDEX idx_transcript_entries_loop_id ON transcript_entries(loop_id);
  ```
- [ ] In-memory agentic state:
  - `AgenticLoopState` struct: `loop_id`, `session_id`, `status`, `pending_tool_calls: VecDeque<ToolCall>`, `metrics`, `browser_subscribers: Vec<Sender>`
  - `DashMap<AgenticLoopId, AgenticLoopState>` in `AppState`
- [ ] Handle agent messages in WS handler:
  - `AgenticLoopDetected` -> insert into DB + DashMap
  - `AgenticLoopStateUpdate` -> update DashMap + DB status
  - `AgenticLoopToolCall` -> insert tool_calls DB + DashMap queue
  - `AgenticLoopToolResult` -> update tool_calls DB + DashMap
  - `AgenticLoopTranscript` -> insert transcript_entries DB + DashMap
  - `AgenticLoopMetrics` -> update DashMap + DB token counts
  - `AgenticLoopEnded` -> update DB + DashMap, notify subscribers
  - Forward all events to subscribed browser WS connections
- [ ] REST API:
  - `GET /api/loops` -- list loops (filterable by host_id, session_id, status)
  - `GET /api/loops/:id` -- full loop state (from DashMap if active, DB if ended)
  - `GET /api/loops/:id/tools` -- tool calls for loop
  - `GET /api/loops/:id/transcript` -- transcript entries
  - `POST /api/loops/:id/action` -- user action `{ "action": "approve|reject|...", "payload"?: "..." }`
    - Relay as `ServerMessage::AgenticLoopUserAction` to agent
  - `GET /api/loops/:id/metrics` -- current metrics
- [ ] Browser WS extension: support subscribing to specific loop IDs for real-time updates

---

## 4.4 Frontend: Agentic Loop Control Panel

**Files:** `web/src/components/agentic/{AgenticLoopPanel,AgenticActionBar,ToolCallQueue,TranscriptView,ContextUsageBar,CostTracker}.tsx`, `web/src/types/agentic.ts`, `web/src/stores/agentic-store.ts`

- [ ] Install `zustand` for state management
- [ ] `types/agentic.ts` -- TypeScript interfaces:
  - `AgenticLoop`, `ToolCall`, `TranscriptEntry`, `AgenticMetrics`
  - `AgenticStatus`, `ToolCallStatus`, `UserAction`, `TranscriptRole` enums
- [ ] `stores/agentic-store.ts` (zustand):
  - State: `activeLoops: Map<string, AgenticLoop>`, `toolCalls: Map<string, ToolCall[]>`, `transcripts: Map<string, TranscriptEntry[]>`
  - Actions: `updateLoop`, `addToolCall`, `updateToolCall`, `addTranscript`, `removeLoop`
  - Subscribe to real-time events from `/ws/events`
- [ ] `AgenticLoopPanel` -- main panel:
  - Header: tool icon, model name, status badge (colored), duration timer (live)
  - `AgenticActionBar` below header
  - Tab switcher: Terminal (1), Tool Queue (2), Transcript (3) -- switch with number keys
  - Content area renders selected tab
- [ ] `AgenticActionBar`:
  - Approve button (green, `Enter` key) -- only when `WaitingForInput`
  - Reject button (red, `Esc` key) -- only when `WaitingForInput`
  - Provide Input button (`I` key) -> opens text input modal
  - Pause/Resume toggle (`P` key) -- when Working/Paused
  - Stop button (`Shift+S`) -- always available, requires confirm dialog
  - Buttons enable/disable based on loop status
  - Optimistic UI: show "Approving..." immediately, confirm on server ack (<50ms feel)
- [ ] `ContextUsageBar`:
  - Horizontal progress bar
  - Color transitions: green (0-70%), yellow (70-85%), red (85-100%)
  - Label: "45,231 / 100,000 tokens (45%)"
  - Warning icon at 85%+
- [ ] `CostTracker`:
  - Display: "$0.42 | 12.3k in / 3.1k out | Claude Sonnet 4"
  - Human-readable token counts (k/M suffix)
  - Updates in real-time without flickering (use CSS transitions)
- [ ] `ToolCallQueue`:
  - "Pending" section at top (highlighted bg, pulsing border if waiting):
    - Each item: tool name, arguments preview (collapsible JSON), status badge
    - Inline Approve/Reject buttons per item
    - Keyboard: arrows navigate, Enter approve, Esc reject
  - "History" section below (scrollable):
    - Completed/failed items with duration, result preview
  - Virtual scrolling for >100 items (`@tanstack/react-virtual` -- Phase 7, basic list for now)
- [ ] `TranscriptView`:
  - Chat-like layout:
    - Assistant messages: left-aligned, dark bg
    - User messages: right-aligned, accent bg
    - Tool use/result: monospace, collapsible sections
    - System messages: centered, muted text
  - Auto-scroll to bottom with "scroll to latest" button when scrolled up
  - Syntax highlighting for code blocks (basic, no heavy library yet)
- [ ] Sidebar integration:
  - "Agentic Loops" section under each host with count badge
  - Pulsing animation on `WaitingForInput` status
  - Badge showing pending tool call count
- [ ] Keyboard shortcuts:
  - `A` or `Enter` -> approve
  - `R` or `Backspace` -> reject
  - `I` -> provide input
  - `P` -> pause/resume
  - `Shift+S` -> stop (with confirm)
  - `1`/`2`/`3` -> switch tabs

---

## 4.5 Permission Management

**Files:** `crates/myremote-agent/src/agentic/permissions.rs`, `crates/myremote-server/src/routes/permissions.rs`, `web/src/components/settings/PermissionRulesEditor.tsx`

- [ ] DB migration (add to 002_agentic.sql):
  ```sql
  CREATE TABLE permission_rules (
      id TEXT PRIMARY KEY,
      scope TEXT NOT NULL DEFAULT 'global',  -- 'global' | 'host:{id}' | 'project:{id}'
      tool_pattern TEXT NOT NULL,            -- glob pattern, e.g., "Read", "Bash*", "*"
      action TEXT NOT NULL DEFAULT 'ask'     -- 'auto_approve' | 'ask' | 'deny'
  );
  ```
- [ ] Agent permission engine (`permissions.rs`):
  - Receive rules from server via `ServerMessage::PermissionRulesUpdate`
  - Match incoming tool calls against rules (glob pattern matching)
  - Auto-approve: silently approve, send result
  - Ask: forward to server for user decision (default for everything)
  - Deny: silently reject with message
  - Default is ALWAYS "ask" -- never auto-approve without explicit rule
- [ ] REST API:
  - `GET /api/permissions` -- list all rules
  - `PUT /api/permissions` -- upsert rule `{ scope, tool_pattern, action }`
  - `DELETE /api/permissions/:id` -- remove rule
- [ ] `PermissionRulesEditor` component:
  - Table of rules: tool pattern, action dropdown (auto_approve/ask/deny), scope, delete button
  - "Add Rule" form at bottom
  - Changes save immediately (POST on change)
  - Part of Settings page

---

## Verification Checklist

1. [ ] Start Claude Code in a terminal session -> agent detects agentic loop -> control panel appears
2. [ ] Tool call detected -> appears in Tool Queue with pending status
3. [ ] Click Approve -> tool call approved -> agent sends "y" to PTY -> tool executes
4. [ ] Click Reject -> tool call rejected -> agent sends "n" to PTY
5. [ ] Context usage bar updates as tokens are consumed
6. [ ] Cost tracker shows running cost estimate
7. [ ] Transcript view shows conversation flow in real-time
8. [ ] Keyboard shortcuts work: Enter approve, Esc reject, P pause, etc.
9. [ ] Loop ends -> status changes to Completed, data persisted in DB
10. [ ] Permission rule set to "auto_approve" for "Read" -> Read tool calls auto-approved without prompt

## Review Notes

- Terminal output parsing is inherently fragile -- detector MUST have confidence score and graceful fallback
- AgenticToolAdapter trait allows adding Codex/Aider support by implementing the trait only
- Permission auto-approve defaults to "ask" for everything -- never silently allow destructive tools
- DashMap for in-memory state -- verify no deadlocks in concurrent access
- Action bar responds <50ms to keypress via optimistic UI updates
- Pulsing sidebar animation for WaitingForInput must be impossible to miss
- Agent acts as adapter layer: server only understands generic agentic events
- Consider structured output channel (--output-format stream-json) as primary data source when available
