# RFC: Tmux Pane Awareness & Multi-Pane Terminal Tabs

## 1. Problem Statement

When Claude Code runs inside a zremote terminal session (tmux-backed) and spawns teammates via `TeamCreate`, tmux splits the pane. All tmux commands in `TmuxSession` target the **session name** (`-t zremote-{uuid}`). When tmux resolves a session name as a target, it defaults to the **active pane**, which shifts to the new split after `split-window`:

- `send-keys -t session_name` sends input to the teammate pane, not the original shell
- `pipe-pane -t session_name` captures output from the wrong pane
- `resize-window -t session_name` resizes the entire window, not individual panes

Result: the UI shows output but input goes nowhere (or to the wrong pane). The session appears broken.

Additionally, users have no visibility into what teammates are doing in their split panes.

## 2. Goals

1. **Pane-stable I/O** -- Input always reaches the original shell pane, output always comes from it, regardless of how many panes are split
2. **Multi-pane visibility** -- All tmux panes in a session are visible in the browser UI as tabs
3. **Per-pane interaction** -- User can switch between pane tabs and send input to any pane
4. **Zero configuration** -- Works automatically when panes are split by Claude Code or manually
5. **Backward compatible** -- Existing single-pane sessions work exactly as before

## 3. Non-Goals

- No creating/splitting panes from the UI (panes are created by processes inside tmux)
- No pane layout visualization (no visual split view, just tabs)
- No pane arrangement control (no drag-to-reorder, no resize between panes)
- No cross-session pane moves

## 4. Design Decisions

### 4.1 Pane ID targeting (Phase 1)

tmux assigns globally unique pane IDs in `%N` format (e.g., `%0`, `%5`). These IDs are stable -- they do not change when panes are split, reordered, or when the active pane changes. Using `%N` as the `-t` target instead of the session name makes all operations immune to active-pane shifts.

Captured immediately after `new-session` via `tmux list-panes -t {session} -F "#{pane_id}"`.

For reattach/recovery: target `{session}:0.0` (window 0, pane 0) which is always the original shell, since zremote creates sessions with a single window and pane. tmux does not reindex panes.

### 4.2 resize-pane vs resize-window

With splits, `resize-window` affects all panes and tmux redistributes space unpredictably. Since the browser terminal shows a single pane's content, `resize-pane -t %N` is correct -- it sizes exactly the pane being displayed.

### 4.3 Pane detection: polling

New panes are detected via periodic `tmux list-panes` polling (every 3 seconds, piggybacking on the existing `check_sessions` interval). Alternatives considered:

- **tmux hooks** (`after-split-window`): Requires tmux 2.4+, complex lifecycle management, race conditions
- **inotify on /proc**: Platform-specific, fragile
- **Polling**: Simple, reliable, 3s latency is acceptable for tab appearance

### 4.4 Per-pane output: separate FIFO per pane

Each detected pane gets its own `pipe-pane` + FIFO + reader task, following the same pattern as the main pane. Output is tagged with `pane_id` and routed to per-pane scrollback buffers.

### 4.5 Frontend: hidden-but-mounted terminals

Each pane tab has its own xterm.js instance, always mounted but hidden via `display:none` when not active. This preserves terminal state (cursor, screen buffer, scroll position). Switching tabs is instant with no data reload.

Tab bar appears only when >1 pane exists. Single-pane sessions look exactly as before.

### 4.6 Output channel type change

The output channel changes from `(SessionId, Vec<u8>)` to a `PtyOutput` struct:

```rust
pub struct PtyOutput {
    pub session_id: SessionId,
    pub pane_id: Option<String>,  // None = main/only pane
    pub data: Vec<u8>,
}
```

For PTY sessions (non-tmux), `pane_id` is always `None`. This is a mechanical change across 5 files.

---

## 5. Technical Design

### Phase 1: Pane-aware targeting

**Single file: `crates/zremote-agent/src/tmux.rs`**

#### 5.1.1 TmuxSession struct change

```rust
pub struct TmuxSession {
    session_id: SessionId,
    tmux_name: String,
    pane_id: String,          // NEW: e.g. "%5"
    fifo_path: PathBuf,
    reader_handle: JoinHandle<()>,
    pid: u32,
}
```

#### 5.1.2 New helper: `get_pane_id`

```rust
fn get_pane_id(target: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // tmux list-panes -t {target} -F "#{pane_id}"
    // Validate result starts with '%'
}
```

#### 5.1.3 Command targeting changes

| Method | Before | After |
|--------|--------|-------|
| `spawn` | `get_pane_pid(&tmux_name)`, `setup_pipe_pane(&tmux_name, ...)` | `get_pane_id(&tmux_name)` first, then `get_pane_pid(&pane_id)`, `setup_pipe_pane(&pane_id, ...)` |
| `reattach` | `get_pane_pid(&tmux_name)`, `pipe-pane -t tmux_name` | `get_pane_id("{tmux_name}:0.0")`, `get_pane_pid(&pane_id)`, `pipe-pane -t pane_id` |
| `write` | `send-keys -t tmux_name -H` | `send-keys -t pane_id -H` |
| `resize` | `resize-window -t tmux_name` | `resize-pane -t pane_id` |
| `detach` | `pipe-pane -t tmux_name` (stop) | `pipe-pane -t pane_id` (stop) |
| `Drop` | `pipe-pane -t tmux_name` (stop) | `pipe-pane -t pane_id` (stop) |
| `kill` | `kill-session -t tmux_name` | **No change** (session-level) |
| `try_wait` | `has-session -t tmux_name` | **No change** (session-level) |

#### 5.1.4 Tracing

Add `pane_id` field to spawn, reattach, and detach log lines.

---

### Phase 2: Multi-pane tabs

#### 5.2.1 Pane monitoring (`crates/zremote-agent/src/tmux.rs`)

New types:

```rust
pub struct PaneInfo {
    pub pane_id: String,
    pub pid: u32,
    pub index: u16,
    pub is_active: bool,
}

pub enum PaneChange {
    Added(PaneInfo),
    Removed(String),  // pane_id
}

struct ExtraPaneHandle {
    pane_id: String,
    fifo_path: PathBuf,
    reader_handle: JoinHandle<()>,
}
```

New methods on `TmuxSession`:

```rust
/// List all panes in this session.
pub fn list_panes(&self) -> Vec<PaneInfo>;

/// Detect pane changes, setup/teardown extra pane I/O. Returns changes.
pub fn sync_panes(&mut self) -> Vec<PaneChange>;

/// Write to a specific pane (main or extra).
pub fn write_to_pane(&mut self, pane_id: &str, data: &[u8]) -> std::io::Result<()>;

/// Resize a specific pane.
pub fn resize_pane(&self, pane_id: &str, cols: u16, rows: u16) -> Result<...>;
```

New field: `extra_panes: Vec<ExtraPaneHandle>`, `known_pane_ids: HashSet<String>`.

Extra pane FIFOs stored in same directory with naming: `{session_id}-{pane_id_stripped}.fifo` (e.g., `abc-5.fifo` for pane `%5`).

#### 5.2.2 Output channel (`crates/zremote-agent/src/session.rs`)

```rust
pub struct PtyOutput {
    pub session_id: SessionId,
    pub pane_id: Option<String>,
    pub data: Vec<u8>,
}
```

Channel type: `mpsc::Sender<PtyOutput>` (was `mpsc::Sender<(SessionId, Vec<u8>)>`).

New method on `SessionManager`:
```rust
pub fn write_to_pane(&mut self, session_id: &SessionId, pane_id: &str, data: &[u8]) -> Result<...>;
```

**Files changed (mechanical):**
- `crates/zremote-agent/src/pty.rs` -- reader sends `PtyOutput { pane_id: None, .. }`
- `crates/zremote-agent/src/tmux.rs` -- reader sends `PtyOutput { pane_id: Some(..), .. }`
- `crates/zremote-agent/src/connection.rs` -- output loop receives `PtyOutput`
- `crates/zremote-agent/src/local/mod.rs` -- output loop receives `PtyOutput`

#### 5.2.3 Browser messages (`crates/zremote-core/src/state.rs`)

```rust
BrowserMessage::Output {
    #[serde(skip_serializing_if = "Option::is_none")]
    pane_id: Option<String>,
    #[serde(with = "base64_serde")]
    data: Vec<u8>,
}
// NEW:
BrowserMessage::PaneAdded { pane_id: String, index: u16 }
BrowserMessage::PaneRemoved { pane_id: String }
```

Per-pane scrollback in `SessionState`:
```rust
pub struct SessionState {
    // existing fields (scrollback becomes main-pane scrollback)...
    pub pane_scrollbacks: HashMap<String, (VecDeque<Vec<u8>>, usize)>,  // pane_id -> (chunks, size)
}
```

#### 5.2.4 Terminal WebSocket (`crates/zremote-agent/src/local/routes/terminal.rs`)

Extended input messages:
```rust
BrowserInput::Input { pane_id: Option<String>, data: String }
BrowserInput::Resize { pane_id: Option<String>, cols: u16, rows: u16 }
```

Input routing: if `pane_id` is Some, call `mgr.write_to_pane()`. If None, write to main pane. Backward compatible -- old clients omit `pane_id`, it deserializes as `None`.

On WebSocket connect: send per-pane scrollback (main first, then extras with PaneAdded + scrollback per pane).

#### 5.2.5 Output loop (`crates/zremote-agent/src/local/mod.rs`)

In `process_pty_output`:
- Route to per-pane scrollback based on `pane_id`
- Tag `BrowserMessage::Output` with `pane_id`

In `check_sessions` (3s interval):
- Call `tmux_session.sync_panes()` for each tmux-backed session
- Broadcast `PaneAdded`/`PaneRemoved` to all browser senders for that session

#### 5.2.6 Frontend types (`web/src/types/terminal.ts`)

```typescript
// NEW file
export interface PaneInfo {
  pane_id: string;
  index: number;
}

// Extended WsMessage in Terminal.tsx
interface WsMessage {
  type: "output" | "session_closed" | "session_suspended" | "session_resumed"
      | "error" | "scrollback_start" | "scrollback_end"
      | "pane_added" | "pane_removed";  // NEW
  pane_id?: string;   // NEW - on output, pane_added, pane_removed
  index?: number;     // NEW - on pane_added
  data?: string;
  exit_code?: number | null;
  message?: string;
}
```

#### 5.2.7 Terminal component (`web/src/components/Terminal.tsx`)

Props change:
```typescript
interface TerminalProps {
  sessionId: string;
  paneId?: string;        // NEW - undefined = main pane
  onPaneEvent?: (event: PaneEvent) => void;  // NEW - bubble pane changes up
}
```

Input/resize messages include `pane_id` when set:
```typescript
wsRef.current.send(JSON.stringify({ type: "input", pane_id: paneId, data }));
```

Handle `pane_added`/`pane_removed` by calling `onPaneEvent`.

**Important**: Only the main-pane Terminal instance manages the WebSocket connection. Extra pane terminals receive output forwarded from the main terminal's WS handler (via shared state or callback). This avoids multiple WS connections per session.

Alternative: each pane could have its own WS connection to `/ws/terminal/{sessionId}?pane={paneId}`. Simpler per-component but more server resources. Going with single WS + client-side routing.

#### 5.2.8 PaneTabBar component (`web/src/components/PaneTabBar.tsx`)

```typescript
interface PaneTabBarProps {
  panes: PaneInfo[];
  activePaneId: string | undefined;  // undefined = main
  onSelectPane: (paneId: string | undefined) => void;
}
```

- Horizontal tab bar, only visible when `panes.length > 0` (extra panes exist)
- Main pane tab always first, labeled "Main" or "Shell"
- Extra panes labeled "Pane {index}" with pane_id as tooltip
- Active tab: `bg-bg-tertiary text-text-primary`, inactive: `text-text-secondary hover:text-text-primary`
- Consistent with existing tab patterns (`text-sm`, `px-3 py-1.5`, `rounded-md`, `transition-colors duration-150`)

#### 5.2.9 SessionPage layout (`web/src/pages/SessionPage.tsx`)

```
SessionPage
+-- Header (existing, unchanged)
+-- Terminal area (flex-1, flex-col)
    +-- AgenticOverlay (existing)
    +-- PaneTabBar (NEW, hidden when single pane)
    +-- Pane panels (flex-1, relative)
        +-- Terminal sessionId paneId=undefined (main, always visible)
        +-- Terminal sessionId paneId="%5" (hidden when not active)
        +-- Terminal sessionId paneId="%6" (hidden when not active)
```

State:
```typescript
const [panes, setPanes] = useState<PaneInfo[]>([]);
const [activePaneId, setActivePaneId] = useState<string | undefined>(undefined);
```

All terminals always mounted (CSS `display: none` for inactive). On `pane_removed` for active pane, switch to main.

---

## 6. Files Changed

| File | Phase | Change |
|------|-------|--------|
| `crates/zremote-agent/src/tmux.rs` | 1+2 | pane_id field, PaneMonitor, extra pane I/O |
| `crates/zremote-agent/src/session.rs` | 2 | PtyOutput struct, write_to_pane |
| `crates/zremote-agent/src/pty.rs` | 2 | PtyOutput (mechanical) |
| `crates/zremote-agent/src/local/mod.rs` | 2 | per-pane output routing, pane sync in check loop |
| `crates/zremote-agent/src/local/routes/terminal.rs` | 2 | pane_id in messages, per-pane scrollback |
| `crates/zremote-agent/src/connection.rs` | 2 | PtyOutput (mechanical) |
| `crates/zremote-core/src/state.rs` | 2 | BrowserMessage pane_id, PaneAdded/Removed, per-pane scrollback |
| `web/src/types/terminal.ts` | 2 | NEW - PaneInfo type |
| `web/src/components/Terminal.tsx` | 2 | paneId prop, pane event handling |
| `web/src/components/PaneTabBar.tsx` | 2 | NEW - tab bar component |
| `web/src/pages/SessionPage.tsx` | 2 | pane state, tabs, multiple terminals |

## 7. Phasing

**Phase 1** is the critical fix. Single file change (`tmux.rs`). Can be shipped immediately. Makes existing sessions work correctly when panes are split.

**Phase 2** builds on Phase 1. Adds multi-pane visibility. Can be implemented incrementally:
- 2a: Backend pane monitoring + PtyOutput type change + BrowserMessage changes
- 2b: Frontend tabs

## 8. Testing

```bash
# Phase 1
cargo build -p zremote-agent && cargo test -p zremote-agent && cargo clippy -p zremote-agent

# Phase 2
cargo test --workspace && cargo clippy --workspace
cd web && bun run typecheck && bun run test

# Manual (both phases)
# 1. cargo run -p zremote-agent -- local --port 3000
# 2. Open browser, create session
# 3. In terminal: tmux split-window -h
# Phase 1: input still goes to original pane (left)
# Phase 2: new "Pane 1" tab appears, can switch and interact with both
# 4. Close the split pane (exit in it)
# Phase 2: tab disappears, back to single terminal
```

New tests:
- `get_pane_id` validation (rejects empty, non-`%` strings)
- `PtyOutput` serialization
- `BrowserMessage::Output` with `pane_id` roundtrip
- `BrowserMessage::PaneAdded`/`PaneRemoved` serialization
- `PaneTabBar` render tests (0 panes hidden, 1+ panes visible, active state)

## 9. Risk Assessment

- **Phase 1 -- Low risk**: Single file, no API changes, public interface unchanged. Backward compatible (pane_id captured at runtime).
- **Phase 2 -- Medium risk**: Output channel type change touches 5 files (mechanical). BrowserMessage change is additive (new optional field + new variants). Frontend is isolated.
- **Edge case -- tmux < 1.8**: `#{pane_id}` format variable introduced in tmux 1.8 (2013). Any version in active use supports it.
- **Edge case -- pane closed between detection and I/O setup**: sync_panes catches this on next poll. Reader gets EOF, cleans up.
