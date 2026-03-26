# RFC: PTY Sessions Survive WebSocket Reconnects

## Problem

When the WebSocket connection between agent and server drops (server restart, network blip, timeout), **all PTY sessions are killed immediately**. This happens because `run_connection()` in `connection.rs` owns the `SessionManager` and the `pty_output_rx` channel receiver. When the function exits on disconnect:

1. `pty_output_rx` (the receiver) is dropped
2. All PTY reader tasks hold `output_tx` sender clones — `blocking_send()` immediately fails
3. All sessions terminate simultaneously with `exit_code: None`
4. For daemon/tmux: `detach_all()` is called but reader tasks are already dead

Observed: 5 sessions killed within 36ms on a single server restart.

## Architecture (current)

```
run_agent() [main.rs]
  └── loop {                          ← reconnect loop
        run_connection()              ← OWNS SessionManager + pty_output channel
          ├── SessionManager::new()
          ├── mpsc::channel (pty_output_tx, pty_output_rx)
          ├── AgenticLoopManager::new()
          ├── SessionMapper::new()
          ├── HooksServer::new()      ← binds random port, writes port file
          ├── discover_existing()
          ├── main select! loop
          └── cleanup: detach_all() / close_all()
      }                               ← function returns → receiver drops → sessions die
                                         HooksServer orphaned with dead channels
```

## Architecture (proposed)

```
run_agent() [main.rs]
  ├── SessionManager::new()           ← HOISTED: survives reconnects
  ├── mpsc::channel (pty_output)      ← HOISTED: receiver never drops during reconnect
  ├── AgenticLoopManager::new()       ← HOISTED: agentic state preserved
  ├── SessionMapper::new()            ← HOISTED: CC session mappings preserved
  └── loop {                          ← reconnect loop
        run_connection(
          &mut session_manager,       ← borrowed
          &mut pty_output_rx,         ← borrowed
          &mut agentic_manager,       ← borrowed
          &session_mapper,            ← Clone (Arc<RwLock<>> internally)
        )
          ├── outbound_tx/rx          ← per-connection (tied to WS sender task)
          ├── agentic_tx/rx           ← per-connection (tied to WS sender task)
          ├── HooksServer             ← per-connection, with local shutdown signal
          │                              (drops when run_connection exits → server stops)
          ├── re-announce sessions    ← tells server about surviving sessions
          ├── discover_existing()     ← finds daemon/tmux from previous lifecycle (skips already tracked)
          ├── main select! loop
          └── cleanup ONLY on shutdown (not on disconnect)
      }
  └── final cleanup: detach_all() / close_all()
```

## Files to modify

### 1. `crates/zremote-agent/src/session.rs`

**ADD** `output_tx()` getter:
```rust
pub fn output_tx(&self) -> &mpsc::Sender<PtyOutput> {
    &self.output_tx
}
```

**ADD** `active_session_info()` for re-announcing sessions after reconnect:
```rust
pub fn active_session_info(&self) -> Vec<(SessionId, String, u32)> {
    self.sessions.iter().map(|(id, backend)| {
        let pid = match backend { ... };
        let shell = get_process_name(pid);
        (*id, shell, pid)
    }).collect()
}
```

**MODIFY** `discover_existing()`: Skip sessions already tracked in `self.sessions` to prevent duplicate reader tasks on reconnect.

### 2. `crates/zremote-agent/src/main.rs`

**MODIFY** `run_agent()`: Create persistent state before the reconnect loop:
- `mpsc::channel::<PtyOutput>(256)` — PTY output channel
- `SessionManager::new()` — session lifecycle
- `AgenticLoopManager::new()` — agentic loop detection state
- `SessionMapper::new()` — CC session-to-loop mappings

Pass by `&mut` / `Clone` into `run_connection()`. Final cleanup after loop exits.

### 3. `crates/zremote-agent/src/connection.rs`

**MODIFY** `run_connection()` signature: Accept borrowed `&mut SessionManager`, `&mut Receiver<PtyOutput>`, `&mut AgenticLoopManager`, `SessionMapper`. Remove `backend` parameter. Remove internal creation of hoisted state.

**ADD** per-connection HooksServer lifecycle: Create a local `watch::channel(false)` for HooksServer shutdown. When `run_connection` exits, the sender drops → `wait_for_shutdown`'s `rx.changed().await` returns `Err` → HooksServer gracefully stops. This prevents orphaned HooksServer instances across reconnects.

**MODIFY** session re-announce: Handle both reconnect (sessions in manager) and fresh start (discover from filesystem). Merge, deduplicate, send single `SessionsRecovered`.

**MODIFY** cleanup (line 646): Only run `detach_all()`/`close_all()` when `*shutdown.borrow() == true`.

### 4. `crates/zremote-agent/src/hooks/server.rs`

No changes needed — `wait_for_shutdown` already handles sender-dropped correctly (returns when `rx.changed().await` fails).

### 5. No changes needed

- **Protocol**: `SessionsRecovered` already exists and handles re-announcement
- **Server**: `cleanup_agent()` suspends sessions on disconnect; `SessionsRecovered` handler resumes them
- **GUI**: Handles session suspension/resumption via server events
- **pty.rs / tmux.rs / daemon/**: Reader tasks unchanged — they hold `output_tx` clones which now stay valid

## Review findings (architect + rust reviewer)

### Resolved in this RFC

| Issue | Resolution |
|-------|-----------|
| SessionMapper must be hoisted | Hoisted — `Clone` with `Arc<RwLock<>>`, shared state preserved |
| HooksServer orphaned on reconnect | Per-connection shutdown via `watch::channel` sender drop |
| `discover_existing()` overwrites tracked sessions | Dedup guard — skip sessions already in manager |

### Accepted behavior (not bugs)

| Issue | Rationale |
|-------|-----------|
| `blocking_send` blocks during disconnect (cap 256) | PTY readers stall, shell pauses (XOFF). Output resumes on reconnect. This is inherent to bounded channels and acceptable — the alternative (unbounded) risks OOM during long disconnects. |
| Plain PTY sessions now survive reconnects | New benefit — previously impossible. Server re-announces them via `SessionsRecovered`. |
| `get_process_name()` sync call in async context | Only called once per session during reconnect announce. Negligible impact. |

### Out of scope (pre-existing)

- `std::thread::sleep(200ms)` in `handle_session_create` blocks async executor
- `ProjectScanner` debounce state loss on reconnect (causes redundant scan, harmless)
- `KnowledgeManager` process lifecycle on reconnect

## Edge cases

| Scenario | Behavior |
|----------|----------|
| Plain PTY child dies during disconnect | EOF buffered in channel (cap 256), processed on reconnect, `SessionClosed` sent |
| Channel fills during long disconnect | PTY readers block on `blocking_send`. Shell pauses (XOFF). Output resumes on reconnect |
| Agent process killed (not just WS) | `discover_existing()` finds daemon/tmux sessions on restart |
| Multiple rapid reconnects | Safe — `&mut` borrow, no concurrent access |
| Session created, WS drops before ACK | Session alive in manager, re-announced on reconnect |
| Hooks fired during reconnect gap | HooksServer stopped (sender dropped). Hook fails gracefully. New server starts on reconnect |
| discover_existing() with hoisted sessions | Skips already-tracked sessions, only discovers truly new ones |

## Risk assessment

- **Low risk**: Core change is moving ownership up one level. Session management logic unchanged.
- **Protocol compatible**: No new message types or fields.
- **Backward compatible**: Server behavior unchanged. Agent reconnects faster with sessions already alive.

## Verification

1. `cargo check -p zremote-agent`
2. `cargo test --workspace`
3. `cargo clippy --workspace`
4. Manual: connect agent, open terminals, kill server, restart → sessions survive
