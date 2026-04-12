# RFC-006: Async Task Ownership Convention

**Status:** Implemented
**Issue:** #34
**Date:** 2026-04-11

## Context

GPUI's `Task<()>` type implements `Drop` to cancel the underlying future. When a struct field holds a `Task<()>`, the task is automatically cancelled when the struct is dropped. This is the correct pattern for long-running async work tied to an entity's lifetime.

The codebase had several long-running async tasks that called `.detach()` instead of being stored as fields. While each had proper exit conditions (channel close, entity drop check), `.detach()` means the task can outlive its parent entity if the exit condition races with the drop.

## Audit Results

### Converted (4 tasks)

**TerminalPanel** (`terminal_panel.rs`):
1. **PTY reader** (`start_output_reader`) — `cx.spawn().detach()` → stored as `pty_reader_task: Option<Task<()>>`

**MainView** (`main_view.rs`):
4. **Event polling loop** (`start_event_polling`) — `cx.spawn().detach()` → stored as `_event_poller: Task<()>`
5. **Loop reconciliation** (`start_loop_reconciliation`) — `cx.spawn().detach()` → stored as `_loop_reconciler: Task<()>`
6. **Toast tick timer** (`start_toast_tick`) — `cx.spawn().detach()` → stored as `_toast_ticker: Task<()>`

### Already correct (not touched)

- **Resize debounce** in TerminalPanel — `tokio_handle.spawn()`, exits via channel close on drop. Must stay on tokio runtime (uses `tokio::time::timeout`). **Do not move to `cx.background_spawn()`.**
- `pending_waiting_notifications: HashMap<String, Task<()>>` in MainView — already owned
- Short fire-and-forget `.detach()` calls in sidebar.rs, agent_profiles_tab.rs — single API calls, correct
- `cx.subscribe().detach()` calls — GPUI manages subscription lifetime
- `lib.rs:155` exit timer — fire-and-forget by design

## Convention (added to CLAUDE.md)

1. Every long-running GPUI async task → `Task<()>` field (or `Option<Task<()>>`)
2. No `.detach()` for polling loops, WebSocket listeners, timers
3. `.detach()` OK for: fire-and-forget short work, `cx.subscribe()` results
4. Use `cx.spawn` for state-touching tasks, `cx.background_spawn` for CPU/IO
5. Replacing a `Task<()>` field cancels the previous task automatically
6. **Never move tokio tasks to `cx.background_spawn()`** — GPUI's executor lacks tokio runtime context, so `tokio::time::*`, `tokio::select!`, etc. will panic or hang. Tasks using tokio APIs must stay on `tokio_handle.spawn()` and exit via channel close.

## Risk Assessment

**Low risk.** No behavior change — all converted tasks had proper exit conditions already. The only difference is that tasks now cancel deterministically on struct drop instead of racing with their exit condition.
