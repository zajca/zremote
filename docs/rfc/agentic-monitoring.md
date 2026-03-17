# RFC: Agentic Session Monitoring (Claude Code View)

## Context

ZRemote has an "agentic session monitoring" feature designed to provide real-time visibility into AI coding tools (Claude Code, Codex, Gemini CLI, Aider) running inside PTY sessions. The system has three layers: Agent (detects and reports), Server (stores and broadcasts), Browser (displays).

**Problem**: The server pipeline, REST API, WebSocket events, database schema, and all frontend UI components are fully implemented and functional. However, the agent never sends structured data -- tool calls, transcripts, and metrics never arrive. The UI exists but remains empty. The feature is effectively non-functional.

**Goal**: Make the agentic monitoring feature fully operational by implementing a hooks-based data extraction system on the agent side, and unify the terminal + agentic views in the browser.

---

## Current State Analysis

### Working Components

| Layer | Component | Status |
|-------|-----------|--------|
| Agent | Process detection (BFS, 3s interval) | Working - detects CC/Codex/Gemini/Aider by process name |
| Agent | Basic status detection | Working - pattern matching: "Thinking"->Working, "Allow"->WaitingForApproval, "Done!"->Completed |
| Agent | User action injection | Working - approve=`y\n`, reject=`n\n`, stop=`Ctrl+C` via PTY |
| Agent | LoopDetected/LoopStateUpdate/LoopEnded messages | Working - sent via WS |
| Server | All 7 AgenticAgentMessage handlers | Working - DB persistence, in-memory store, event broadcast |
| Server | REST API (/api/loops, /tools, /transcript, /metrics, /action) | Working - queries and mutations |
| Server | Analytics + FTS5 transcript search | Working - aggregations, full-text search |
| Server | Event broadcasting (broadcast channel, /ws/events) | Working - LoopStatusChanged, ToolCallPending, LoopEnded events |
| Browser | AgenticLoopPanel, ToolCallQueue, TranscriptView | Working - renders data, keyboard shortcuts |
| Browser | CostTracker, ContextUsageBar, AgenticActionBar | Working - displays metrics, action controls |
| Browser | Zustand store + WebSocket event handling | Working - real-time updates |
| Browser | Sidebar loop indicators + navigation | Working - shows active loops per session |

### Broken Components

| Layer | Component | Issue |
|-------|-----------|-------|
| Agent | LoopToolCall message | **Never sent** - parser never generates ToolCall events |
| Agent | LoopToolResult message | **Never sent** - parser never generates ToolResult events |
| Agent | LoopTranscript message | **Never sent** - parser never generates Transcript events |
| Agent | LoopMetrics message | **Never sent** - all metrics hardcoded to 0 |
| Agent | Permission rules processing | **Ignored** - received but not applied ("Phase 4.5" comment) |
| Agent | Terminal output parsing | **Fragile** - simple substring matching, breaks on CC format changes |
| Browser | Split terminal + agentic view | **Not implemented** - separate pages, no side-by-side |

### Root Cause

The agent's `ClaudeCodeAdapter` (`crates/zremote-agent/src/agentic/claude_code.rs`) parses raw ANSI terminal output using simple string patterns. This approach cannot extract structured data (tool names, arguments, results, token counts, costs) because Claude Code's terminal output is human-readable formatted text, not structured data.

### Key Files

**Agent (data source - needs changes):**
- `crates/zremote-agent/src/agentic/claude_code.rs` - terminal parser, only generates StatusChanged/Ended
- `crates/zremote-agent/src/agentic/manager.rs` - loop lifecycle, event -> protocol message translation
- `crates/zremote-agent/src/agentic/detector.rs` - process tree BFS detection
- `crates/zremote-agent/src/agentic/types.rs` - AgenticEvent enum (ToolCall/Transcript/Metrics variants exist but unused)
- `crates/zremote-agent/src/connection.rs` - WS lifecycle, message routing, sender task with agentic channel (64 cap)

**Protocol (shared types):**
- `crates/zremote-protocol/src/agentic.rs` - AgenticAgentMessage (7 variants), AgenticServerMessage (2 variants)

**Server (no changes needed for Phases 1-3):**
- `crates/zremote-server/src/routes/agents.rs` - WS handler, handles all agentic messages
- `crates/zremote-server/src/routes/agentic.rs` - REST endpoints
- `crates/zremote-server/src/state.rs` - AgenticLoopStore (DashMap), ServerEvent enum

**Browser (no changes needed for Phases 1-3):**
- `web/src/components/agentic/` - all UI components (6 files)
- `web/src/stores/agentic-store.ts` - zustand store
- `web/src/hooks/useRealtimeUpdates.ts` - WebSocket event dispatch
- `web/src/pages/SessionPage.tsx` - terminal-only (rewrite in Phase 4)

---

## Solution: Claude Code Hooks + HTTP Sidecar

### Claude Code Integration Points

Claude Code supports **hooks** configured in `.claude/settings.json`:

| Hook Event | Trigger | Payload (stdin JSON) |
|------------|---------|---------------------|
| `PreToolUse` | Before each tool call | `tool_name`, `tool_input`, `tool_use_id` |
| `PostToolUse` | After tool call completes | `tool_name`, `tool_input`, `tool_response` |
| `PermissionRequest` | Permission dialog shown | `tool_name`, `tool_input`, can return `allow`/`deny` |
| `Stop` | Claude finishes responding | `session_id`, `transcript_path`, `stop_hook_active` |
| `Notification` | Alert/notification | `message` |
| `SubagentStart` | Subagent spawned | subagent metadata |
| `SubagentStop` | Subagent finished | subagent metadata |

All hooks receive base JSON: `{ session_id, transcript_path, cwd, hook_event_name }`.

**Transcript JSONL files** at `~/.claude/projects/[encoded-path]/[session-uuid].jsonl` contain:
- Full messages with `content` arrays (text, tool_use, tool_result blocks)
- Token usage per API call: `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`
- Model info, session metadata

### Architecture

```
Claude Code process (in PTY session managed by zremote)
  |
  +--> Hook fires (PreToolUse / PostToolUse / Stop / PermissionRequest)
  |      |
  |      +--> Hook script reads JSON from stdin
  |      +--> curl POST to agent HTTP sidecar (127.0.0.1:PORT/hooks)
  |
  +--> Terminal output (ANSI text)
         |
         +--> Existing PTY relay to server (unchanged)
         +--> Existing pattern matching (kept as fallback)

Agent (zremote-agent)
  |
  +--> HTTP sidecar receives structured hook data
  +--> Maps CC session_id -> zremote loop_id
  +--> Translates to AgenticAgentMessage
  +--> Sends via existing agentic_tx channel -> WebSocket -> Server

Server (zremote-server) -- NO CHANGES NEEDED
  |
  +--> Existing handlers persist to DB, update in-memory store
  +--> Existing broadcast sends events to browser clients

Browser (React) -- NO CHANGES NEEDED for data flow
  |
  +--> Existing WebSocket event handlers update zustand store
  +--> Existing UI components render tool calls, transcripts, metrics
```

### Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Hook deployment scope | Global `~/.claude/settings.json` | Zero per-project config; agent filters events by known PTY sessions |
| Hook script technology | Shell script + curl | Universal, no dependencies, trivial implementation |
| Permission blocking | Long-poll HTTP with 55s timeout | Simple, deterministic; falls back to terminal prompt on timeout |
| Transcript data source | Parse JSONL file on Stop hook | Simple batch processing; hooks already provide real-time tool call data |
| Split view library | `react-resizable-panels` or CSS grid | Lightweight, well-maintained |
| Multi-tool strategy | Hooks for CC (full), terminal parsing for others (basic) | Hooks are CC-specific; other tools lack equivalent APIs |

### Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| CC hook timeout kills PermissionRequest blocking | 55s timeout (under CC's 60s limit), fallback to terminal prompt |
| CC changes hook JSON schema | Defensive JSON parsing, version detection, graceful degradation |
| Hook install modifies user's settings.json | Merge-style update (preserve existing hooks), backup file, `managed by zremote` markers |
| Multiple CC instances on same machine | Disambiguate via `cwd` + PID matching to known PTY sessions |
| CC not installed or old version without hooks | Fall back to existing terminal parsing (current behavior preserved) |
| Agent HTTP port conflicts | Bind to `127.0.0.1:0` (OS-assigned port), write to `~/.zremote/hooks-port` |

---

## Implementation Plan

### Phase 1: Agent HTTP Sidecar + Tool Call Monitoring

**Goal**: Agent starts HTTP listener. Hook scripts POST tool call data to it. Agent emits `LoopToolCall`/`LoopToolResult` via existing WS channel. Server + frontend automatically display tool calls.

**Create:**
- `crates/zremote-agent/src/hooks/mod.rs` - module exports
- `crates/zremote-agent/src/hooks/server.rs` - axum HTTP server on `127.0.0.1:0`, writes port to `~/.zremote/hooks-port`
- `crates/zremote-agent/src/hooks/handler.rs` - POST /hooks endpoint, dispatches by `hook_event_name`
- `crates/zremote-agent/src/hooks/mapper.rs` - CC `session_id` <-> zremote `loop_id` mapping (via cwd + PTY PID)
- `crates/zremote-agent/src/hooks/installer.rs` - generates/updates `~/.claude/settings.json` with hook commands

**Modify:**
- `crates/zremote-agent/src/main.rs` - start hooks server alongside WS connection
- `crates/zremote-agent/src/connection.rs` - pass `agentic_tx` sender to hooks server, so hook handler can emit messages
- `crates/zremote-agent/src/agentic/manager.rs` - expose method to register CC session_id -> loop_id mapping when LoopDetected fires
- `crates/zremote-agent/Cargo.toml` - add deps if needed

**Hook script** (generated by installer, stored in `~/.zremote/hooks/`):
```bash
#!/bin/sh
PORT=$(cat ~/.zremote/hooks-port 2>/dev/null) || exit 0
curl -s -X POST "http://127.0.0.1:$PORT/hooks" \
  -H "Content-Type: application/json" \
  -d "$(cat -)" >/dev/null 2>&1
exit 0
```

**Data flow:**
- `PreToolUse` hook -> `LoopToolCall { tool_call_id: tool_use_id, tool_name, arguments_json: tool_input, status: Pending }`
- `PostToolUse` hook -> `LoopToolResult { tool_call_id: tool_use_id, result_preview: tool_response[..500], duration_ms }`

**Verification:** Start CC in a ZRemote PTY session. Open ToolCallQueue in browser. Verify tool calls appear in real-time with names + arguments.

---

### Phase 2: Transcript + Metrics via JSONL Parsing

**Goal**: Extract conversation transcript and token/cost metrics from CC transcript JSONL files. CostTracker and TranscriptView display real data.

**Create:**
- `crates/zremote-agent/src/hooks/transcript.rs` - JSONL parser, extracts messages + token counts
- `crates/zremote-agent/src/hooks/metrics.rs` - token aggregation + cost calculation (model pricing table)

**Modify:**
- `crates/zremote-agent/src/hooks/handler.rs` - handle `Stop` hook event: parse transcript, emit LoopTranscript + LoopMetrics
- `crates/zremote-agent/src/hooks/mapper.rs` - store `transcript_path` per loop, track read offset for incremental parsing

**Data flow:**
1. `Stop` hook fires -> agent receives `{ transcript_path, session_id, ... }`
2. Parse JSONL from last-read offset to end of file
3. For each assistant message -> `LoopTranscript { role: "assistant", content }`
4. For each user message -> `LoopTranscript { role: "user", content }`
5. For each tool_result -> `LoopTranscript { role: "tool", content, tool_call_id }`
6. Aggregate all token fields -> `LoopMetrics { tokens_in, tokens_out, model, context_used, context_max, cost }`

**Cost calculation:**
- Hardcode pricing for known models (claude-sonnet-4, claude-opus-4, claude-haiku-4.5, etc.)
- `cost = input_tokens * input_price + output_tokens * output_price + cache_read * cache_price`
- Fallback: if model unknown, report tokens without cost

**Verification:** After CC completes a turn, verify TranscriptView shows conversation entries and CostTracker shows non-zero token counts and cost.

---

### Phase 3: Permission Control + Auto-Approve Rules

**Goal**: Intercept CC permission requests. User can approve/deny from ZRemote UI. Auto-approve rules applied by agent.

**Create:**
- `crates/zremote-agent/src/hooks/permission.rs` - PermissionRequest handler with blocking HTTP response pattern

**Modify:**
- `crates/zremote-agent/src/hooks/handler.rs` - route `PermissionRequest` hook to permission handler
- `crates/zremote-agent/src/connection.rs` - handle `PermissionRulesUpdate` from server, store rules in agent state
- `crates/zremote-agent/src/agentic/manager.rs` - permission rule matching logic (glob pattern against tool_name)

**Data flow:**
1. CC shows permission dialog -> `PermissionRequest` hook fires -> POST to agent
2. Agent checks stored permission rules against `tool_name`:
   - AutoApprove match -> respond immediately with `{"decision": "allow"}`, exit 0
   - Deny match -> respond with exit 2 (block)
   - Ask/no match -> continue to step 3
3. Agent emits `LoopToolCall { status: Pending }` to server -> server broadcasts `ToolCallPending`
4. Agent holds HTTP response, waits for `UserAction` via WS (up to 55s)
5. User clicks Approve/Reject in browser -> POST /api/loops/:id/action -> server -> WS -> agent
6. Agent responds to held HTTP request: approve -> `{"decision": "allow"}`, reject -> exit 2
7. On 55s timeout -> exit 0 (pass-through, CC shows terminal prompt as fallback)

**Permission rules:**
- `PermissionRule { scope, tool_pattern, action }` - already defined in protocol
- Agent stores `Vec<PermissionRule>` in memory, updated when server sends `PermissionRulesUpdate`
- Matching: glob pattern (e.g., `Bash*`, `Read`, `*`) against incoming tool_name

**Verification:** Start CC, trigger a tool that requires permission. Verify prompt appears in ZRemote UI. Click approve. Verify CC proceeds without terminal interaction.

---

### Phase 4: Split Terminal + Agentic View

**Goal**: Show terminal and agentic panel side-by-side when CC session is detected. Three view modes: terminal, agentic, split.

**Modify:**
- `web/src/pages/SessionPage.tsx` - rewrite from terminal-only to support 3 view modes with resizable panes
- `web/src/App.tsx` - possibly simplify routing (embed AgenticLoopPanel instead of separate /loops/:loopId page)
- `web/src/stores/agentic-store.ts` - add helper: get active loop_id for a given session_id

**Create:**
- `web/src/hooks/useSessionLoop.ts` - hook that returns the active loop (if any) for the current session

**Layout:**
```
+----------------------------------------------+
| Session Header (host, shell, status)         |
+----------------------------------------------+
| [Terminal] [Split] [Agentic]  (view toggle)  |
+-------------------+--------------------------+
| Terminal (xterm)  | AgenticLoopPanel         |
| full PTY I/O      | - ToolCallQueue          |
|                   | - TranscriptView         |
|                   | - CostTracker            |
|                   | - ContextUsageBar        |
|                   | - AgenticActionBar       |
+-------------------+--------------------------+
```

**Behavior:**
- Default: terminal-only mode (current behavior)
- Auto-switch to split when `agentic_loop_detected` event arrives for the current session
- User can manually toggle modes via buttons or keyboard shortcuts (`Ctrl+1`/`Ctrl+2`/`Ctrl+3`)
- Panes resizable via drag handle
- Persist preferred mode in localStorage
- When loop ends, optionally switch back to terminal-only

**Verification:** Open session page. Start CC in that session. Verify view auto-switches to split mode with both terminal and agentic panel visible.

---

### Phase 5: Multi-Tool Enhanced Adapters

**Goal**: Improve monitoring quality for non-CC tools (Codex, Gemini CLI, Aider) which lack hooks support.

**Create:**
- `crates/zremote-agent/src/agentic/codex.rs` - Codex-specific terminal output patterns
- `crates/zremote-agent/src/agentic/gemini.rs` - Gemini CLI-specific patterns
- `crates/zremote-agent/src/agentic/aider.rs` - Aider-specific patterns (Aider has `--watch` mode and some structured output)

**Modify:**
- `crates/zremote-agent/src/agentic/claude_code.rs` - improve existing patterns, keep as fallback when hooks unavailable
- `crates/zremote-agent/src/agentic/mod.rs` - register new adapters in factory

**Support tiers:**
- **Tier 1 (full)**: Claude Code via hooks - tool calls, transcripts, metrics, permissions
- **Tier 2 (basic)**: Others via terminal parsing - status detection, user actions (approve/reject/stop)

---

### Phase 6: Analytics Polish

**Goal**: Enhance analytics dashboards now that real data flows through the system.

**Depends on:** Phases 1-3 (need real data in DB tables)

**Enhancements:**
- Tool usage breakdown chart (which tools used most, by project/model)
- Per-model cost comparison
- Session timeline view (terminal events interleaved with tool calls)
- Cost trends over time

**No backend changes needed** - existing analytics endpoints already support these queries. Frontend-only work.

---

## Task List

### Phase 1: HTTP Sidecar + Tool Calls
- [ ] Create `hooks/mod.rs` module structure
- [ ] Implement HTTP sidecar server (`hooks/server.rs`) - bind 127.0.0.1:0, write port file
- [ ] Implement hook event handler (`hooks/handler.rs`) - parse hook JSON, dispatch by event type
- [ ] Implement session ID mapper (`hooks/mapper.rs`) - CC session_id <-> zremote loop_id
- [ ] Implement hook installer (`hooks/installer.rs`) - generate scripts, update ~/.claude/settings.json
- [ ] Modify `main.rs` to start hooks server
- [ ] Modify `connection.rs` to pass agentic_tx to hooks server
- [ ] Modify `manager.rs` to register CC session mapping on LoopDetected
- [ ] Handle PreToolUse -> LoopToolCall translation
- [ ] Handle PostToolUse -> LoopToolResult translation
- [ ] Write tests for hook JSON parsing
- [ ] Write tests for session ID mapping logic
- [ ] Integration test: mock hook POST -> verify WS message emitted

### Phase 2: Transcript + Metrics
- [ ] Implement JSONL transcript parser (`hooks/transcript.rs`)
- [ ] Implement token aggregation + cost calculator (`hooks/metrics.rs`)
- [ ] Handle Stop hook -> parse transcript file -> emit LoopTranscript messages
- [ ] Handle Stop hook -> aggregate tokens -> emit LoopMetrics message
- [ ] Track read offset per transcript file for incremental parsing
- [ ] Define model pricing table (claude-sonnet-4, claude-opus-4, etc.)
- [ ] Write tests for JSONL parsing (various message formats)
- [ ] Write tests for cost calculation

### Phase 3: Permission Control
- [ ] Implement PermissionRequest handler with blocking response (`hooks/permission.rs`)
- [ ] Implement permission rule matching (glob pattern vs tool_name)
- [ ] Wire PermissionRulesUpdate message in `connection.rs` (currently ignored)
- [ ] Store permission rules in agent state
- [ ] Implement 55s timeout with pass-through fallback
- [ ] Wire UserAction from WS to unblock held HTTP response
- [ ] Write tests for rule matching logic
- [ ] Write tests for timeout behavior

### Phase 4: Split View
- [ ] Create `useSessionLoop` hook
- [ ] Rewrite SessionPage with 3 view modes (terminal/split/agentic)
- [ ] Implement resizable pane layout
- [ ] Auto-switch to split on loop detection
- [ ] Add keyboard shortcuts (Ctrl+1/2/3)
- [ ] Persist view mode preference in localStorage
- [ ] Update routing if needed (embed vs separate page)

### Phase 5: Multi-Tool Adapters
- [ ] Create CodexAdapter with Codex-specific terminal patterns
- [ ] Create GeminiAdapter with Gemini CLI-specific patterns
- [ ] Create AiderAdapter with Aider-specific patterns
- [ ] Improve ClaudeCodeAdapter fallback patterns
- [ ] Register adapters in factory/manager

### Phase 6: Analytics Polish
- [ ] Add tool usage breakdown chart component
- [ ] Add per-model cost comparison view
- [ ] Add session timeline view
- [ ] Add cost trend chart
