# RFC-001: Agentic Loops - Bug Fixes & UX Improvements

**Status:** Draft
**Date:** 2026-03-16
**Author:** Analysis by Claude

---

## 1. Problem Statement

The agentic loops feature is completely non-functional. When a user runs an agentic tool (e.g., `claude`) in a terminal session, the loop never appears in the UI. This is caused by 3 independent bugs that each break a different link in the detection-to-display chain.

### Reported Symptom
> "When I create a loop, I don't see it in the UI."

### Root Cause Summary

| # | Bug | Layer | Impact |
|---|-----|-------|--------|
| 1 | Process name `node` doesn't contain `claude` | Agent (detection) | Loop is never detected |
| 2 | Server doesn't broadcast `LoopDetected` event | Server (events) | Browser is never notified |
| 3 | Frontend listens for wrong event type names | Frontend (WebSocket) | All events are silently dropped |

Even fixing any single bug alone would not make the feature work - all three must be fixed together.

---

## 2. Current Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│ Agent (remote host)                                             │
│                                                                 │
│  PTY Session ──output──► AgenticLoopManager                     │
│       │                       │                                 │
│       │                 check_sessions() every 3s               │
│       │                       │                                 │
│       │              detect_agentic_tool()                      │
│       │              (process.name() matching)   ◄── BUG 1      │
│       │                       │                                 │
│       │                 LoopDetected msg                        │
│       │                       │                                 │
│       └───────────────────────┼── WebSocket ──────────────────┐ │
└───────────────────────────────┼───────────────────────────────┘ │
                                ▼                                 │
┌───────────────────────────────────────────────────────────────┐ │
│ Server                                                        │ │
│                                                               │ │
│  handle_agentic_message()                                     │ │
│       │                                                       │ │
│       ├── INSERT into DB ✓                                    │ │
│       ├── INSERT into DashMap ✓                               │ │
│       └── state.events.send() ✗  ◄── BUG 2 (missing)         │ │
│                                                               │ │
│  ServerEvent broadcast channel ──► /ws/events WebSocket       │ │
│       sends: "loop_status_changed"                            │ │
│       sends: "loop_ended"                                     │ │
│       sends: "tool_call_pending"                              │ │
└───────────────────────────────────────────────────────────────┘ │
                                                                  │
┌───────────────────────────────────────────────────────────────┐ │
│ Browser                                                       │ │
│                                                               │ │
│  useRealtimeUpdates hook (/ws/events)                         │ │
│       listens: "agentic_loop_detected"       ◄── BUG 3       │ │
│       listens: "agentic_loop_state_update"   (names don't    │ │
│       listens: "agentic_loop_ended"           match server)   │ │
│       listens: "agentic_loop_tool_call"                       │ │
│                                                               │ │
│  All agentic events → switch default → silently dropped       │ │
└───────────────────────────────────────────────────────────────┘
```

---

## 3. Detailed Bug Analysis

### 3.1 Bug 1: Process Detection Fails

**File:** `crates/myremote-agent/src/agentic/detector.rs:37-44`

**Current code:**
```rust
let name = process.name().to_string_lossy().to_lowercase();
for &(signature, tool_name) in KNOWN_TOOLS {
    if name.contains(signature) {
        return Some(DetectedTool { ... });
    }
}
```

**Problem:** On Linux, `process.name()` reads from `/proc/[pid]/comm`, which is the executable name, truncated to 15 chars. Claude Code is a Node.js application - the actual process is `node`, not `claude`. The process tree looks like:

```
bash (shell_pid)
  └── node (runs claude-code CLI)
       └── node (worker threads)
```

The string `"claude"` never appears in `"node"`, so `detect_agentic_tool()` always returns `None`.

**Evidence:** `process.name()` in sysinfo v0.34 returns the kernel's comm field. For Node.js apps, this is always `node` (or `node` truncated).

**Fix:** Also check `process.cmd()` which returns the full command line arguments array. The claude binary path or arguments will contain `"claude"`.

```rust
// Check process name first (fast path for native binaries)
let name = process.name().to_string_lossy().to_lowercase();
if name.contains(signature) {
    return Some(DetectedTool { ... });
}

// Check command line arguments (catches Node.js/Python wrapper tools)
let cmd_line = process.cmd().join(" ").to_lowercase();
if cmd_line.contains(signature) {
    return Some(DetectedTool { ... });
}
```

**Additionally:** The `LoopDetected` message is sent with empty `project_path` and `model` (manager.rs:90-92). We should populate `project_path` from the process's working directory via `process.cwd()`.

---

### 3.2 Bug 2: Missing Broadcast Event for LoopDetected

**File:** `crates/myremote-server/src/routes/agents.rs:822-864`

**Current code (LoopDetected handler):**
```rust
AgenticAgentMessage::LoopDetected { loop_id, session_id, ... } => {
    // INSERT into DB ✓
    sqlx::query("INSERT INTO agentic_loops ...").execute(&state.db).await;

    // INSERT into in-memory store ✓
    state.agentic_loops.insert(loop_id, AgenticLoopState { ... });

    // Log ✓
    tracing::info!("agentic loop detected");

    // Broadcast to browser? ✗ MISSING
    // No state.events.send() call here!
}
```

**Compare with LoopStateUpdate handler (line 907):**
```rust
AgenticAgentMessage::LoopStateUpdate { .. } => {
    // ... update DB and memory ...
    let _ = state.events.send(ServerEvent::LoopStatusChanged { ... });  // ✓ broadcasts
}
```

**Problem:** The `LoopDetected` handler simply doesn't emit any broadcast event. The browser WebSocket connection at `/ws/events` never receives notification that a new loop was created.

**Fix:**
1. Add a new `ServerEvent::LoopDetected` variant
2. Emit `state.events.send(ServerEvent::LoopDetected { ... })` at the end of the handler

---

### 3.3 Bug 3: Event Type Name Mismatch (Server vs Frontend)

**Server sends (state.rs:226-266):**
```
"loop_status_changed"   (LoopStatusChanged variant)
"loop_ended"            (LoopEnded variant)
"tool_call_pending"     (ToolCallPending variant)
```

**Frontend expects (useRealtimeUpdates.ts:81-117):**
```
"agentic_loop_detected"
"agentic_loop_state_update"
"agentic_loop_ended"
"agentic_loop_tool_call"
"agentic_loop_tool_result"
"agentic_loop_transcript"
"agentic_loop_metrics"
```

**No event types match.** All agentic events fall through the `switch` statement with no action.

**Additionally, payload shape mismatch:** Frontend expects `parsed.loop` (nested AgenticLoop object), but server sends flat fields (`loop_id`, `session_id`, `status`, etc.).

**Fix (recommended approach):** Update server-side `ServerEvent` enum:
- Rename serde tags to match frontend expectations
- Add missing variant types (tool_result, transcript, metrics)
- Include a nested `loop` / `tool_call` / `transcript_entry` field with full data

---

## 4. Additional Issues Found

### 4.1 Silent Error Swallowing (P1)

**File:** `web/src/hooks/useAgenticLoops.ts:17-18`
```typescript
catch {
    // Silently fail -- loops are supplementary info
}
```

**File:** `web/src/stores/agentic-store.ts` - `sendAction()` has no error handling. If the agent is offline, the user gets no feedback.

### 4.2 Approve/Reject Ignores Tool Call ID (P1)

**File:** `web/src/components/agentic/AgenticLoopPanel.tsx`
```typescript
const handleToolApprove = useCallback(
    (_toolCallId: string) => {  // toolCallId is IGNORED
        void useAgenticStore.getState().sendAction(loopId, "approve");
    },
    [loopId],
);
```

Approving one tool call approves whatever the underlying tool is currently waiting for, which may not correspond to the tool call the user clicked.

### 4.3 "Pause" Actually Sends Ctrl+C (P2)

**File:** `crates/myremote-agent/src/agentic/claude_code.rs`
```rust
UserAction::Pause | UserAction::Stop => vec![0x03],  // Ctrl+C = SIGINT
```

The "Pause" button label is misleading - it actually interrupts/kills the process.

### 4.4 Empty model and project_path (P2)

**File:** `crates/myremote-agent/src/agentic/manager.rs:90-92`
```rust
messages.push(AgenticAgentMessage::LoopDetected {
    project_path: String::new(),  // always empty
    model: String::new(),         // always empty
    ...
});
```

### 4.5 No Fallback Polling (P2)

**File:** `web/src/hooks/useAgenticLoops.ts`

The hook fetches loops once on mount and then only re-fetches when a custom DOM event fires. Since the WebSocket events never trigger this event (Bug 3), there's no periodic fallback. Even after fixing Bug 3, a polling fallback would improve reliability.

### 4.6 Closed Sessions Hide Loops (P2)

**File:** `web/src/components/sidebar/SessionItem.tsx`
```typescript
const { loops } = useAgenticLoops(
    session.status === "active" ? session.id : undefined,
);
```

When session is not "active", loops are never fetched. Combined with hiding closed sessions, all historical loop data becomes unreachable from the sidebar.

### 4.7 "Loading loop..." Forever on Error (P2)

**File:** `web/src/components/agentic/AgenticLoopPanel.tsx`

If `fetchLoop` fails, the component shows "Loading loop..." forever with no error state, no retry button, no timeout.

### 4.8 "lagged" Event Doesn't Re-fetch Loops (P2)

**File:** `web/src/hooks/useRealtimeUpdates.ts:76-80`

When the `"lagged"` event arrives (broadcast channel overflow), hosts/sessions/projects are re-fetched, but agentic loops are not.

---

## 5. Implementation Plan

### Phase 1: Fix Critical Bugs (P0)

#### Task 1.1: Fix process detection
- **File:** `crates/myremote-agent/src/agentic/detector.rs`
- Add `process.cmd()` matching alongside `process.name()` matching
- Add unit tests for cmd-based matching

#### Task 1.2: Fix project_path detection
- **File:** `crates/myremote-agent/src/agentic/manager.rs`
- Use `process.cwd()` from sysinfo to populate `project_path` in `LoopDetected`
- Pass `&self.system` to the detection path so process metadata is accessible

#### Task 1.3: Add ServerEvent variants and fix naming
- **File:** `crates/myremote-server/src/state.rs`
- Add `LoopInfo` serializable struct matching frontend `AgenticLoop` interface
- Add `ToolCallInfo` serializable struct matching frontend `ToolCall` interface
- Add `TranscriptInfo` serializable struct matching frontend `TranscriptEntry` interface
- Add new `ServerEvent` variants:
  - `LoopDetected` → `#[serde(rename = "agentic_loop_detected")]` with `loop: LoopInfo`
  - Rename `LoopStatusChanged` → `#[serde(rename = "agentic_loop_state_update")]` with `loop: LoopInfo`
  - Rename `LoopEnded` → `#[serde(rename = "agentic_loop_ended")]` with `loop: LoopInfo`
  - Rename `ToolCallPending` → `#[serde(rename = "agentic_loop_tool_call")]` with `tool_call: ToolCallInfo, loop_id: String`
  - Add `ToolCallResult` → `#[serde(rename = "agentic_loop_tool_result")]` with `tool_call: ToolCallInfo, loop_id: String`
  - Add `LoopTranscript` → `#[serde(rename = "agentic_loop_transcript")]` with `transcript_entry: TranscriptInfo, loop_id: String`
  - Add `LoopMetrics` → `#[serde(rename = "agentic_loop_metrics")]` with `loop: LoopInfo`
- Update existing tests for renamed variants

#### Task 1.4: Emit broadcast events for all agentic messages
- **File:** `crates/myremote-server/src/routes/agents.rs`
- Add `state.events.send(ServerEvent::LoopDetected { ... })` in `LoopDetected` handler
- Add broadcast in `LoopToolResult` handler
- Add broadcast in `LoopTranscript` handler
- Add broadcast in `LoopMetrics` handler
- Update existing `LoopStateUpdate` and `LoopEnded` to use renamed variants with `LoopInfo` data
- Helper function: `build_loop_info()` that constructs `LoopInfo` from DB row + in-memory state

#### Task 1.5: Update frontend event handling
- **File:** `web/src/hooks/useRealtimeUpdates.ts`
- Update `ServerEvent` interface to match new server payloads
- No event type name changes needed (server now matches)
- Add agentic loop re-fetch in `"lagged"` handler

### Phase 2: Error Handling & Reliability (P1)

#### Task 2.1: Add error handling to sendAction
- **File:** `web/src/stores/agentic-store.ts`
- Wrap `api.loops.action()` in try/catch
- Show toast on failure ("Host is offline", "Loop not found", etc.)

#### Task 2.2: Add error logging to useAgenticLoops
- **File:** `web/src/hooks/useAgenticLoops.ts`
- Replace silent catch with `console.warn`
- Add fallback periodic polling (every 15s) when session is active

#### Task 2.3: Add error state to AgenticLoopPanel
- **File:** `web/src/components/agentic/AgenticLoopPanel.tsx`
- Replace infinite "Loading loop..." with error state after fetch failure
- Add "Retry" button

### Phase 3: UX Improvements (P2) - Future

These are documented for future work, not part of this RFC:

- [ ] Toast notification when loop is detected
- [ ] Pass `toolCallId` through approve/reject flow
- [ ] Rename "Pause" to "Interrupt" or implement real pause
- [ ] "View all loops" page across sessions
- [ ] Manual loop creation fallback
- [ ] Show completed loops in sidebar (expandable section)
- [ ] Link Terminal tab to actual session terminal

---

## 6. Files to Modify

| File | Phase | Description |
|------|-------|-------------|
| `crates/myremote-agent/src/agentic/detector.rs` | 1.1 | Add cmd() matching |
| `crates/myremote-agent/src/agentic/manager.rs` | 1.2 | Populate project_path from cwd |
| `crates/myremote-server/src/state.rs` | 1.3 | Add/rename ServerEvent variants, add LoopInfo struct |
| `crates/myremote-server/src/routes/agents.rs` | 1.4 | Emit broadcast events for all agentic messages |
| `web/src/hooks/useRealtimeUpdates.ts` | 1.5 | Update event payload interface |
| `web/src/stores/agentic-store.ts` | 2.1 | Add error handling to sendAction |
| `web/src/hooks/useAgenticLoops.ts` | 2.2 | Error logging, fallback polling |
| `web/src/components/agentic/AgenticLoopPanel.tsx` | 2.3 | Error/retry state |

---

## 7. Verification Plan

### Automated Tests
```bash
cargo test --workspace              # Rust tests (update existing + new)
cargo clippy --workspace            # Lint check
cd web && bun run typecheck         # TypeScript check
cd web && bun run test              # Frontend tests
```

### Manual Integration Test
1. Start server: `MYREMOTE_TOKEN=secret cargo run -p myremote-server`
2. Start agent: `MYREMOTE_SERVER_URL=ws://localhost:3000/ws/agent MYREMOTE_TOKEN=secret cargo run -p myremote-agent`
3. Open web UI at `http://localhost:5173`
4. Create a terminal session on the connected host
5. Run `claude` in the terminal
6. **Verify:** Loop appears in sidebar within 3-6 seconds with correct tool name
7. **Verify:** Loop status updates in real-time (working → waiting_for_input)
8. **Verify:** Click on loop → AgenticLoopPanel loads with data
9. **Verify:** Approve/reject actions work and agent receives them
10. **Verify:** When claude exits, loop shows as completed
11. **Verify:** Refresh page → loop is still visible (fetched from DB)
12. **Verify:** If agent disconnects, action failure shows toast error

### Edge Cases to Test
- Start claude before opening the web UI → loop should appear on page load (REST API fetch)
- Multiple sessions with loops → each shows correctly
- Rapid start/stop of claude → no crashes, eventual consistency
- Network disconnect/reconnect → lagged event triggers re-fetch

---

## 8. Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| `process.cmd()` may be empty on some systems | Detection fails | Keep `process.name()` as primary, `cmd()` as fallback. Log when neither matches. |
| Renaming ServerEvent serde tags breaks Telegram bot | Telegram notifications fail | Check `telegram/notifications.rs` for event type usage. Update if needed. |
| Broadcast channel overflow under high event volume | Events lost | "lagged" handler + periodic polling as fallback |
| process.cwd() may require elevated permissions | Empty project_path | Graceful fallback to empty string (current behavior) |

---

## 9. Task Checklist

### Phase 1: Critical Bug Fixes
- [ ] 1.1 - Fix process detection: add `process.cmd()` matching in `detector.rs`
- [ ] 1.1 - Add tests for cmd-based detection
- [ ] 1.2 - Populate `project_path` from `process.cwd()` in `manager.rs`
- [ ] 1.3 - Add `LoopInfo`, `ToolCallInfo`, `TranscriptInfo` structs in `state.rs`
- [ ] 1.3 - Add `ServerEvent::LoopDetected` variant
- [ ] 1.3 - Rename existing event variants to match frontend naming
- [ ] 1.3 - Add missing event variants (tool_result, transcript, metrics)
- [ ] 1.3 - Update `state.rs` tests for renamed/new variants
- [ ] 1.4 - Emit broadcast in `LoopDetected` handler in `agents.rs`
- [ ] 1.4 - Emit broadcast in `LoopToolResult` handler
- [ ] 1.4 - Emit broadcast in `LoopTranscript` handler
- [ ] 1.4 - Emit broadcast in `LoopMetrics` handler
- [ ] 1.4 - Update existing broadcasts to use new variant names
- [ ] 1.4 - Add `build_loop_info()` helper function
- [ ] 1.5 - Update `ServerEvent` interface in `useRealtimeUpdates.ts`
- [ ] 1.5 - Add agentic re-fetch in "lagged" handler
- [ ] 1.5 - Verify all event types are handled
- [ ] Run `cargo test --workspace` - all pass
- [ ] Run `cargo clippy --workspace` - no errors
- [ ] Run `cd web && bun run typecheck` - no errors

### Phase 2: Error Handling
- [ ] 2.1 - Add try/catch + toast in `sendAction()` in `agentic-store.ts`
- [ ] 2.2 - Replace silent catch with `console.warn` in `useAgenticLoops.ts`
- [ ] 2.2 - Add fallback polling (15s interval) in `useAgenticLoops.ts`
- [ ] 2.3 - Add error/retry state to `AgenticLoopPanel.tsx`
- [ ] Run all tests again

### Integration Testing
- [ ] Manual test: full flow (start loop → see in UI → interact → loop ends)
- [ ] Test edge cases (page refresh, agent disconnect, rapid start/stop)
