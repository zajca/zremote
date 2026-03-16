# RFC: Persistent Terminal Sessions via tmux

## 1. Problem Statement

When the myremote-agent process stops (crash, update, restart), all terminal sessions are immediately killed. This happens because:

1. `PtySession::drop()` calls `self.kill()` on the child process
2. `session_manager.close_all()` is called at end of every connection lifecycle
3. Server's `cleanup_agent()` marks all sessions as `closed` and removes them from memory

This kills running Claude Code sessions, destroys user work mid-operation, and makes updates/restarts disruptive. Sessions must survive agent restarts.

## 2. Goals

1. **Session persistence** -- Terminal sessions survive agent crashes, restarts, and updates
2. **Automatic recovery** -- Agent discovers surviving sessions on reconnect without user intervention
3. **Seamless browser experience** -- Browser shows "suspended" status during downtime, resumes when agent reconnects
4. **Zero configuration** -- Works out of the box when tmux is installed, no env vars needed
5. **Graceful fallback** -- Falls back to raw PTY when tmux is unavailable (original behavior)

## 3. Non-Goals

- No manual tmux session management from the UI
- No configurable suspension timeout (all sessions persist until agent reconnects or 24h stale cleanup)
- No support for screen/dtach as alternative backends
- No cross-host session migration

## 4. Design Decisions

### 4.1 Why tmux

- Battle-tested (15+ years) session persistence -- this is literally what tmux was built for
- Programmatic CLI: `new-session`, `send-keys`, `capture-pane`, `resize-window`, `pipe-pane`
- Process tree stays walkable for agentic loop detection (shell PID is child of tmux)
- Available in every Linux distro, trivially installable
- Dedicated socket (`tmux -L myremote`) isolates from user's own tmux sessions

### 4.2 Why not alternatives

- **Custom PTY daemon**: Reinventing tmux but worse, months of work for a fraction of the reliability
- **/proc/fd reattachment**: Not possible -- kernel closes PTY pair when master FD holder exits
- **Containers**: Overkill isolation, breaks process tree detection
- **Input replay**: Non-deterministic, doesn't preserve running processes

### 4.3 I/O strategy

- **Write**: Open `/dev/pts/N` (pane TTY) directly for raw byte I/O -- avoids `send-keys` encoding issues
- **Read**: FIFO via `pipe-pane` -> async reader task (same `spawn_blocking` pattern as PTY, 4KB buffer)
- **Resize**: `tmux resize-window`
- **Kill**: `tmux kill-session` (explicit user close)
- **Drop**: Only detach reader + close FIFO, do NOT kill tmux session (that's the whole point)

## 5. Architecture Change

```
Before:  Agent --owns--> portable-pty --owns--> shell
         Agent dies => PTY dies => shell dies

After:   Agent --communicates--> tmux server --owns--> shell
         Agent dies => tmux + shell survive
         Agent restarts => discovers tmux sessions => reattaches
```

## 6. Implementation

### 6.1 Agent: TmuxSession backend (`tmux.rs`)

New file implementing `TmuxSession` struct with the same interface as `PtySession`:

- `spawn()` -- Creates tmux session, gets pane TTY/PID, creates FIFO, sets up pipe-pane, spawns reader
- `reattach()` -- Verifies existing session, recreates FIFO/pipe-pane, reconnects reader
- `write()` -- Raw bytes to `/dev/pts/N`
- `resize()` -- `tmux resize-window`
- `kill()` -- `tmux kill-session` + FIFO cleanup
- `detach()` -- Stops reader/pipe-pane without killing tmux session
- `discover_sessions()` -- Lists `myremote-*` sessions, reattaches each
- `cleanup_stale()` -- Kills sessions older than 24 hours

Drop implementation only detaches (never kills), preserving persistence.

### 6.2 Agent: SessionBackend enum (`session.rs`)

```rust
enum SessionBackend {
    Pty(PtySession),
    Tmux(TmuxSession),
}
```

`SessionManager` dispatches to the appropriate backend based on `use_tmux` flag (set at startup from `detect_tmux()`). New methods:

- `discover_existing()` -- Recovers tmux sessions from prior agent lifecycle
- `detach_all()` -- Detaches tmux sessions (survive), kills PTY sessions (shutdown path)

### 6.3 Agent: Connection lifecycle (`connection.rs`)

- `Register` message includes `supports_persistent_sessions: bool`
- After registration, calls `discover_existing()` and sends `SessionsRecovered` to server
- On shutdown: `detach_all()` instead of `close_all()` when tmux is enabled

### 6.4 Protocol: New messages (`terminal.rs`)

```rust
struct RecoveredSession {
    session_id: SessionId,
    shell: String,
    pid: u32,
}

// Added to AgentMessage:
SessionsRecovered { sessions: Vec<RecoveredSession> }

// Added to Register:
supports_persistent_sessions: bool  // #[serde(default)] for backward compat
```

### 6.5 Server: Suspension logic (`agents.rs`)

`cleanup_agent()` behavior depends on `supports_persistent_sessions`:

**Persistent agent disconnects:**
- Sessions marked as `suspended` (not `closed`) in DB and memory
- Scrollback buffer preserved in `SessionStore`
- Browsers receive `SessionSuspended` message (yellow overlay)
- Agentic loops left intact (not cleaned up)
- `SessionSuspended` events emitted

**Agent reconnects with `SessionsRecovered`:**
- Recovered sessions: `suspended` -> `active`, browsers receive `SessionResumed`
- Unrecovered sessions: marked `closed`, removed from memory

**Non-persistent agent (no tmux):**
- Original behavior: all sessions closed, agentic loops cleaned up

### 6.6 Server: Terminal relay (`terminal.rs`)

- Browser can connect to `suspended` sessions (not just `active`/`creating`)
- If session is suspended on connect, browser immediately receives `SessionSuspended`

### 6.7 Server: Database migration

```sql
ALTER TABLE sessions ADD COLUMN suspended_at TEXT;
ALTER TABLE sessions ADD COLUMN tmux_name TEXT;
```

### 6.8 Frontend

- `Session.status` union: added `"suspended"`
- `Terminal.tsx`: Handles `session_suspended`/`session_resumed` WS messages, shows overlay, blocks input
- `SessionItem.tsx`: Pause icon + warning badge for suspended sessions

## 7. Session State Machine

```
creating --> active --> closed
               |          ^
               v          |
           suspended -----+
               |
               v
             active  (after SessionsRecovered)
```

## 8. Isolation

| Concern | Solution |
|---|---|
| User's tmux sessions | Dedicated socket: `tmux -L myremote` |
| File permissions | Per-UID FIFO dir: `/tmp/myremote-tmux-{uid}/` |
| Session naming | `myremote-{uuid}` prefix (parseable, collision-free) |
| Stale sessions | Auto-cleanup at agent startup (>24h) |
| Orphaned FIFOs | Cleaned when no matching tmux session exists |

## 9. Files Changed

| File | Change |
|---|---|
| `crates/myremote-agent/src/tmux.rs` | **NEW** -- TmuxSession implementation (709 lines) |
| `crates/myremote-agent/src/session.rs` | SessionBackend enum, discover_existing(), detach_all() |
| `crates/myremote-agent/src/config.rs` | detect_tmux() |
| `crates/myremote-agent/src/connection.rs` | Recovery after registration, conditional close |
| `crates/myremote-agent/src/main.rs` | tmux detection, module declaration |
| `crates/myremote-protocol/src/terminal.rs` | SessionsRecovered, RecoveredSession, Register extension |
| `crates/myremote-server/src/routes/agents.rs` | Suspend/resume logic, SessionsRecovered handler |
| `crates/myremote-server/src/routes/terminal.rs` | Suspended session browser connection |
| `crates/myremote-server/src/state.rs` | SessionSuspended/SessionResumed events, persistent flag |
| `crates/myremote-server/src/main.rs` | Updated register() call sites in tests |
| `crates/myremote-server/migrations/011_persistent_sessions.sql` | **NEW** -- suspended_at, tmux_name |
| `web/src/lib/api.ts` | suspended status type |
| `web/src/components/Terminal.tsx` | Suspension overlay, input blocking |
| `web/src/components/sidebar/SessionItem.tsx` | Pause icon, warning badge |

## 10. Testing

All existing tests pass (443 Rust tests, TypeScript typecheck clean, clippy clean).

New tests added:
- Protocol: `sessions_recovered_roundtrip`, `register_without_persistent_sessions_deserializes`
- Agent: `use_tmux_returns_configured_value`, `discover_existing_returns_empty_when_tmux_disabled`, `detach_all_on_empty_manager_is_noop`, `detect_tmux_returns_bool`
- Agent tmux: 7 unit tests for helper functions (socket config, FIFO paths, session naming, UID)
- Server state: `browser_message_session_suspended_serialization`, `browser_message_session_resumed_serialization`, ServerEvent roundtrip coverage

Manual verification:
1. Start agent with tmux -> sessions spawn inside tmux
2. Kill agent (`kill -9`) -> tmux sessions survive
3. Restart agent -> sessions auto-recovered, server shows active
4. Browser terminal continues (scrollback preserved)
5. Claude Code session survives restart, agentic detection resumes within 3s
6. Without tmux -> graceful fallback to raw PTY
7. User close -> tmux session killed properly
