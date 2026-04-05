# RFC: Session Switcher Preview Pane

**Status**: Draft
**Date**: 2026-04-04

## Problem

The session switcher (Ctrl+Tab) currently shows only metadata -- session title, subtitle, Claude Code state badges -- in a 400x320px list modal. With multiple active terminal sessions, users cannot tell what is happening in each session without switching to it. This slows down workflow when managing many parallel tasks.

## Goal

Extend the session switcher with a **terminal content preview pane** that shows the last ~22 lines of terminal output for the selected session. The switcher must open in **<50ms** -- no perceptible delay.

## Constraints

- Only the **active session** has a local `alacritty::Term` instance in the GUI
- Other sessions exist only on the server/agent as raw ANSI bytes in a 100KB scrollback ring buffer (`SessionState.scrollback` in `zremote-core/src/state.rs`)
- Quick-switch mode (<150ms Ctrl+Tab release) must remain unchanged (skips overlay)
- Preview does not need to be real-time -- a snapshot refreshed every ~5s is sufficient
- Must work in both server mode and local mode

## Architecture

```
Server/Agent (continuous):
  PTY output --> append_scrollback() --> SnapshotParser updates 30-line screen grid

Client (background, every 5s):
  GET /api/sessions/previews --> cache in SidebarView.preview_snapshots

Client (on Ctrl+Tab):
  1. Active session: extract from live Term grid (sync, <1ms)
  2. Other sessions: read from cached preview_snapshots (sync, <1ms)
  3. Render two-panel layout: list | preview
```

### Why Server-side Parsing

| Approach | Open latency | Memory/session | Complexity |
|----------|-------------|----------------|------------|
| **Server-side VTE snapshot** (chosen) | <1ms (pre-cached) | ~4KB on server | Medium |
| Client-side mini-Term instances | ~20ms+ first open | ~80KB on client | High |
| Raw ANSI transfer + client parse | ~20ms+ first open | ~80KB on client | High |
| Background pre-warmed client Terms | <1ms | ~80KB on client | High |

Server-side snapshot wins on simplicity: parsing happens incrementally as PTY output arrives (zero extra cost), client gets structured data ready to render.

## Design

### Phase 1: Server-side Screen Snapshot

#### New types (`zremote-core/src/state.rs`)

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScreenSnapshot {
    pub lines: Vec<ScreenLine>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenLine {
    pub text: String,
    pub spans: Vec<ColorSpan>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColorSpan {
    pub start: u16,
    pub end: u16,
    pub fg: String,  // hex color e.g. "#e0e0e0"
}
```

#### SnapshotParser (`zremote-core/src/snapshot_parser.rs` -- NEW)

Lightweight VTE performer tracking a 30-row x 120-col character grid:
- Implements `vte::Perform` trait (`vte` crate is already a transitive dependency via `alacritty_terminal`)
- Tracks: cursor position, current foreground color, character grid
- Handles: `print`, `execute` (CR, LF, BS, HT), CSI cursor movement (`CUU/CUD/CUF/CUB/CUP`), CSI erase (`ED/EL`), SGR color codes (basic 8, 256, and RGB)
- Ignores: mouse, OSC, DCS, private modes, and all other sequences
- Estimated ~150-200 lines of code

Integration into `SessionState`:
```rust
pub fn append_scrollback(&mut self, data: Vec<u8>) {
    self.snapshot_parser.advance(&data);  // NEW
    self.scrollback_size += data.len();
    self.scrollback.push_back(data);
    // ... existing eviction logic
}
```

The parser is called on the same path as existing scrollback writes -- zero additional I/O cost.

#### Batch REST endpoint

`GET /api/sessions/previews`

Returns snapshots for all active sessions in a single call:
```json
{
  "previews": {
    "<session_id>": {
      "lines": [
        {
          "text": "$ cargo test --workspace",
          "spans": [{"start": 0, "end": 1, "fg": "#00ff00"}]
        }
      ],
      "cols": 120,
      "rows": 40
    }
  }
}
```

Handler reads from in-memory `SessionStore` (no DB query). Response size for 10 sessions: ~24KB.

Routes to add:
- `crates/zremote-server/src/routes/sessions.rs` -- handler function
- `crates/zremote-server/src/lib.rs` -- `.route("/api/sessions/previews", get(...))`
- `crates/zremote-agent/src/local/routes/sessions.rs` -- handler for local mode
- `crates/zremote-agent/src/local/mod.rs` -- register route

### Phase 2: Client SDK + Background Polling

#### Client types (`zremote-client/src/types.rs`)

Mirror server types: `PreviewSnapshot`, `PreviewLine`, `PreviewColorSpan`.

#### API method (`zremote-client/src/client.rs`)

```rust
pub async fn get_session_previews(&self) -> Result<HashMap<String, PreviewSnapshot>>
```

#### Background cache (`zremote-gui/src/views/sidebar.rs`)

- New field: `preview_snapshots: HashMap<String, PreviewSnapshot>`
- Poll every 5 seconds (can piggyback on existing 5s reconciliation timer or use a separate interval)
- Exposed via `pub fn preview_snapshots(&self) -> &HashMap<String, PreviewSnapshot>`

### Phase 3: Expanded Session Switcher UI

#### Layout

```
+--------------------------------------------------------------+
|                      Session Switcher                         |
+----------------------------+---------------------------------+
| > Session 1 (current)     |  $ cargo test --workspace        |
|   myhost / myproject       |     Running 42 tests...          |
|   [auto] [opus] ████░     |     test foo::bar ... ok         |
|                            |     test baz::qux ... FAILED     |
|   Session 2                |     ...                          |
|   myhost / other           |                                  |
|   [plan] [sonnet] ██░░    |                                  |
|                            |                                  |
|   Session 3                |                                  |
|   localhost / dev          |                                  |
+----------------------------+---------------------------------+
```

- **Total width**: ~680px (from 400px), max height ~400px
- **Left panel** (280px): existing session list, slightly narrower
- **Right panel** (~400px): terminal preview
  - Dark background (`theme::surface_overlay()` or similar)
  - Monospace font (JetBrainsMono, 11px)
  - Shows last ~22 lines of terminal content
  - Color spans rendered via GPUI `StyledText`
  - Updates instantly on list navigation (swaps cached `PreviewSnapshot`)

#### Session switcher changes (`session_switcher.rs`)

- Constructor accepts `preview_snapshots: HashMap<String, PreviewSnapshot>`
- `SwitcherEntry` gains `preview: Option<PreviewSnapshot>` field
- Two-column `render()`: `h_flex().child(list_panel).child(preview_panel)`
- Right panel reads `entries[selected_index].preview`

#### Current session preview (`terminal_element.rs` or `terminal_panel.rs`)

```rust
pub fn extract_preview_lines(term: &TerminalTerm, max_lines: usize) -> Vec<PreviewLine>
```

Traverses `term.grid()` bottom-up, extracts char + fg color per cell. Single mutex lock, <1ms.

#### MainView integration (`main_view.rs`)

`open_session_switcher()` reads `preview_snapshots` from sidebar, extracts current session preview from live terminal, passes both to `SessionSwitcher::new()`.

### Phase 4: Edge Cases & Polish

| Scenario | Behavior |
|----------|----------|
| Empty session (no output) | Centered "No output yet" + terminal icon |
| Suspended session | Preview content + dim "Suspended" overlay |
| First poll not returned yet | Subtle "Loading..." placeholder |
| Window too narrow (<600px) | Hide preview panel, fall back to list-only |
| Quick-switch (<150ms) | Unchanged -- skips overlay entirely |

## Performance Budget

| Step | Budget | Notes |
|------|--------|-------|
| Key event dispatch | <1ms | GPUI event system |
| Clone sidebar snapshot | <1ms | Rc clone + HashMap clone |
| Extract current term preview | <1ms | Single mutex lock, grid traversal |
| Build SwitcherEntry list | <1ms | Existing logic + attach previews |
| GPUI layout + paint | ~10-15ms | Two-column, text rendering |
| **Total** | **~15-20ms** | Well within 50ms target |

Server-side SnapshotParser overhead: negligible (~1us per PTY output chunk for grid update).
Background poll: ~24KB per 5s (10 sessions). Negligible bandwidth.

## Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| VTE parser adds CPU per PTY byte | Low | Only tracks 30x120 grid, ignores most sequences |
| Stale previews for fast-changing sessions | Low | Acceptable for switcher; active session is always fresh from live Term |
| `vte` crate version conflict | Low | Already transitive dep; pin to same version as alacritty_terminal |
| Memory for SnapshotParser per session | Low | ~4KB per parser (30x120 chars + color data) |

## Files

| File | Action | Description |
|------|--------|-------------|
| `crates/zremote-core/src/snapshot_parser.rs` | CREATE | VTE-based screen snapshot parser (~200 lines) |
| `crates/zremote-core/src/state.rs` | MODIFY | Add ScreenSnapshot types, integrate parser into SessionState |
| `crates/zremote-core/src/lib.rs` | MODIFY | Add `mod snapshot_parser` |
| `crates/zremote-server/src/routes/sessions.rs` | MODIFY | Add `get_session_previews` handler |
| `crates/zremote-server/src/lib.rs` | MODIFY | Register preview route |
| `crates/zremote-agent/src/local/routes/sessions.rs` | MODIFY | Add preview handler for local mode |
| `crates/zremote-agent/src/local/mod.rs` | MODIFY | Register preview route |
| `crates/zremote-client/src/types.rs` | MODIFY | Add preview response types |
| `crates/zremote-client/src/client.rs` | MODIFY | Add `get_session_previews()` method |
| `crates/zremote-gui/src/views/sidebar.rs` | MODIFY | Background polling, preview cache |
| `crates/zremote-gui/src/views/session_switcher.rs` | MODIFY | Two-panel layout, preview rendering |
| `crates/zremote-gui/src/views/main_view.rs` | MODIFY | Pass preview data to switcher |
| `crates/zremote-gui/src/views/terminal_element.rs` | MODIFY | Add `extract_preview_lines()` helper |

## Testing

1. **Unit tests**: SnapshotParser handles CR/LF/cursor movement/SGR correctly
2. **Unit tests**: Preview extraction from alacritty::Term grid
3. **Integration tests**: `/api/sessions/previews` returns correct data
4. **Manual**: Open 3+ sessions, Ctrl+Tab shows preview for each, <50ms latency
5. **Manual**: Edge cases -- empty session, suspended, rapid toggle, narrow window
