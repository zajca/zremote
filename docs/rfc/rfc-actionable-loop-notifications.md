# RFC: Actionable Agentic Loop Notifications (GPUI)

## Context & Problem

When Claude Code (or other agentic tools) transitions to `WaitingForInput`, `Completed`, or `Error` status, the GPUI desktop app only updates the sidebar state and terminal panel indicator silently. No toast or native OS notification is triggered. The user has no way to know CC needs attention unless they're actively looking at the app window.

Additionally, the hook payload's `message` field (containing the actual prompt text from Claude Code) is received by the agent but **discarded** when creating the `LoopStateUpdate` protocol message — the GUI never sees what CC is actually asking.

**Goal**: Show actionable toast notifications with Yes/No buttons for `WaitingForInput` and informational toasts for `Completed`/`Error`, plus native OS notifications when the window is unfocused. The user should be able to approve/deny CC prompts directly from a toast without switching to the terminal.

## Current State

### What exists
- **Toast system** (`zremote-gui/src/views/toast.rs`): Passive toasts with severity levels (Info/Success/Warning/Error), auto-dismiss (4-8s), max 5 visible. No action buttons, no click handlers, no dismiss API.
- **Native OS notifications** (`zremote-gui/src/notifications.rs`): Uses `notify-rust` crate, text-only, no actions. Only sent when window is NOT focused.
- **Agentic loop events**: `LoopStatusChanged` carries `LoopInfo` with status enum (`Working`/`WaitingForInput`/`Error`/`Completed`), loop_id, session_id, tool_name, task_name, tokens, cost.
- **Hook payload**: When CC triggers `WaitingForInput`, the agent receives `HookPayload` with `message` field containing the prompt text — but it's **not forwarded** through the protocol.
- **Terminal input**: WebSocket accepts `{"type": "input", "data": "yes\n"}` → writes directly to PTY. `InputSender` (flume channel) is accessible from `TerminalHandle`.

### What's missing
- `LoopStatusChanged` does NOT trigger any toast or native notification in the GUI
- Only `ClaudeTaskStarted`/`ClaudeTaskEnded` and `WorktreeError` trigger notifications
- No prompt/message text flows from agent to GUI
- Toasts have no interactive elements (buttons, click handlers, dismiss)

## Architecture

```
Claude Code Hook
  └─ POST /hooks/notification/{idle,permission}
     └─ HookPayload { session_id, message, ... }
        └─ Agent: send_waiting_for_input()
           └─ AgenticAgentMessage::LoopStateUpdate {
                loop_id, status: WaitingForInput,
                task_name, prompt_message  ← NEW
              }
              └─ Server: AgenticProcessor
                 └─ ServerEvent::LoopStatusChanged {
                      loop_info: LoopInfo { ..., prompt_message }  ← NEW
                    }
                    └─ WebSocket → GUI Client → MainView
                       ├─ Persistent actionable toast [Yes] [No] [X]
                       ├─ Native OS notification (if unfocused)
                       └─ Auto-dismiss when status → Working or LoopEnded
```

### Response flow (user clicks "Yes" in toast)
```
Toast [Yes] button click
  └─ ToastAction callback
     └─ InputSender.send(b"yes\n")
        └─ flume channel → TerminalSession → WebSocket
           └─ Agent: PTY write_all(b"yes\n")
              └─ Claude Code reads input, continues
                 └─ Hook: status → Working
                    └─ LoopStatusChanged { Working }
                       └─ MainView: auto-dismiss toast
```

## Design Decisions

### D1: `prompt_message` is transient — no DB storage
The prompt text is only meaningful at the moment the event fires. Storing it in the `agentic_loops` table would require a migration and adds complexity for no benefit. If a client misses the WS event, they won't see the prompt text — acceptable since they can still see the `WaitingForInput` status.

### D2: Yes/No as default action buttons
Covers the most common CC prompts (permission approvals, y/n confirmations). For free-form prompts (where the user needs to type a response), the user must switch to the terminal. The toast still serves as a notification in this case.

### D3: Native OS notifications stay text-only
Linux desktop notification actions (`notify-rust` `.action()`) are unreliable across desktop environments (GNOME, KDE, Sway all behave differently). The in-app toast is the reliable interaction mechanism. Native notifications serve as attention-grabbers only.

### D4: Persistent toasts for WaitingForInput
WaitingForInput toasts must not auto-dismiss — they persist until the user acts (clicks a button) or CC resumes on its own (status changes to `Working` or loop ends). This is controlled by a `persistent` flag on `Toast`.

### D5: `Rc<Cell<Option<FnOnce>>>` pattern for GPUI callbacks
GPUI's `on_click` requires `Fn` (callable multiple times), but toast actions are `FnOnce` (fire once). Using `Rc<Cell<Option<Box<dyn FnOnce(...)>>>>` allows `.take()` on first click, no-op on subsequent clicks. Single-threaded safe in GPUI.

### D6: Actions only for current session's terminal
If the WaitingForInput event is for a different session than the currently open terminal, the toast shows as info-only (no action buttons) since we can't send input to a terminal that isn't connected. The notification still alerts the user.

### D7: `Urgency::Critical` for WaitingForInput native notifications
Ensures the notification appears prominently on Linux even in Do Not Disturb mode (depending on DE).

## Technical Design

### Phase 1: Protocol — Forward prompt message

#### `crates/zremote-protocol/src/agentic.rs`
Add optional field to `LoopStateUpdate`:
```rust
LoopStateUpdate {
    loop_id: AgenticLoopId,
    status: AgenticStatus,
    task_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_message: Option<String>,  // NEW
},
```

#### `crates/zremote-protocol/src/events.rs`
Add to `LoopInfo`:
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub prompt_message: Option<String>,  // NEW — transient, not stored in DB
```

#### `crates/zremote-agent/src/hooks/handler.rs`
In `send_waiting_for_input()`: pass `payload.message.clone()` into the protocol message.

#### `crates/zremote-core/src/processing/agentic.rs`
Thread `prompt_message` through `handle_loop_state_update`. Overlay transient message onto `LoopInfo` fetched from DB before broadcasting.

### Phase 2: Actionable Toast System

#### `crates/zremote-gui/src/views/toast.rs`

New types:
```rust
pub struct ToastAction {
    pub label: String,
    pub icon: Option<Icon>,
    callback: Rc<Cell<Option<Box<dyn FnOnce(&mut Window, &mut App)>>>>,
}

impl ToastAction {
    pub fn new(
        label: impl Into<String>,
        icon: Option<Icon>,
        callback: impl FnOnce(&mut Window, &mut App) + 'static,
    ) -> Self { ... }

    /// Take the callback (returns None on subsequent calls).
    pub fn take_callback(&self) -> Option<Box<dyn FnOnce(&mut Window, &mut App)>> {
        self.callback.take()
    }
}
```

Extended `Toast`:
```rust
pub struct Toast {
    pub id: u64,
    pub message: String,
    pub level: ToastLevel,
    pub icon: Option<Icon>,
    pub created_at: Instant,
    pub actions: Vec<ToastAction>,  // NEW — empty for regular toasts
    pub persistent: bool,           // NEW — no auto-dismiss
}
```

New methods on `ToastContainer`:
- `push_actionable(message, level, icon, actions, persistent) -> u64` — returns toast ID
- `dismiss(id: u64)` — removes toast by ID

Render changes:
- After message text, render action buttons (11px text, rounded, themed)
- Add "X" dismiss button for persistent toasts
- Action click: execute callback via `.take()`, then dismiss toast

### Phase 3: Wire LoopStatusChanged to Notifications

#### `crates/zremote-gui/src/views/main_view.rs`

New field:
```rust
waiting_input_toasts: HashMap<String, u64>,  // loop_id → toast_id
```

In `handle_server_event()`, after existing loop forwarding (lines 350-374):

1. **`WaitingForInput`**: Build message from `prompt_message` or fallback. Create Yes/No `ToastAction`s via `InputSender`. Push persistent toast. Store in `waiting_input_toasts`.
2. **`Working`** (CC resumed): Dismiss active toast for this loop.
3. **`LoopEnded`**: Dismiss active toast. Show completion/error toast. Native notification.

#### `crates/zremote-gui/src/views/terminal_panel.rs`
Expose `pub fn input_sender(&self) -> InputSender`.

### Phase 4: Native Notification Enhancement

#### `crates/zremote-gui/src/notifications.rs`
Add optional urgency override parameter to `send_native()`. Use `Urgency::Critical` for WaitingForInput.

## File Change Summary

| File | Change | Phase |
|------|--------|-------|
| `crates/zremote-protocol/src/agentic.rs` | Add `prompt_message` to `LoopStateUpdate` | 1 |
| `crates/zremote-protocol/src/events.rs` | Add `prompt_message` to `LoopInfo` | 1 |
| `crates/zremote-agent/src/hooks/handler.rs` | Pass `payload.message` through | 1 |
| `crates/zremote-core/src/processing/agentic.rs` | Thread `prompt_message` to event broadcast | 1 |
| `crates/zremote-gui/src/views/toast.rs` | Actionable toasts: actions, persistent, dismiss | 2 |
| `crates/zremote-gui/src/views/main_view.rs` | Notification logic for loop state changes | 3 |
| `crates/zremote-gui/src/views/terminal_panel.rs` | Expose `input_sender()` | 3 |
| `crates/zremote-gui/src/notifications.rs` | Urgency override for critical notifications | 4 |

## Edge Cases

- **Multiple WaitingForInput for same loop**: New toast replaces old (dismiss previous, show new)
- **Race condition** (user responds in terminal before clicking toast): CC resumes → `Working` event → toast auto-dismissed. Harmless — extra `"yes\n"` would just be echoed after CC already continued.
- **Session mismatch**: Toast shows info-only (no action buttons) when WaitingForInput is for a different session than the currently viewed terminal
- **Reconnect after missed event**: Sidebar reconciliation (5s periodic) catches up status. Toast won't appear for missed events — acceptable since the prompt moment has passed.

## Verification

1. `cargo test --workspace` — all protocol roundtrip tests pass with new field
2. `cargo clippy --workspace` — no warnings
3. Manual test flow:
   - Run CC in a terminal session
   - Trigger a permission prompt (e.g., tool call requiring approval)
   - Verify: persistent toast appears with prompt text + Yes/No buttons
   - Click "Yes" → input sent to terminal, CC continues, toast dismissed
   - Unfocus window → verify native OS notification appears
   - Let CC finish → verify completion toast + native notification
4. `/visual-test` — verify toast rendering with action buttons
