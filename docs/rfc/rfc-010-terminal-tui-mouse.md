# RFC-010: Terminal TUI Mouse, Keyboard, and Input Compatibility

## Status: Implemented

## Problem Statement

ZRemote's GPUI terminal can render modern TUI applications, but it does not yet behave like a native terminal for mouse-driven tools such as Hunk and lazygit.

The terminal currently treats mouse input as ZRemote UI input:

- left click starts local selection
- drag updates local selection
- mouse up copies selection
- wheel scrolls ZRemote/alacritty scrollback
- Ctrl-hover/click opens detected URLs

That behavior is useful for shell output, but it conflicts with full-screen TUI applications. Hunk and lazygit both expect xterm-style mouse reporting for click, drag, release, and wheel events. When those applications enable mouse mode, ZRemote should forward mouse events to the PTY instead of consuming them for local selection or scrollback.

These tools also rely on normal terminal keyboard semantics. Cursor keys must respect application cursor mode, global ZRemote shortcuts must not steal common TUI keys while a full-screen app is active, and terminal input must preserve raw bytes so legacy mouse reports are not corrupted.

## Goals

1. Make Hunk and lazygit usable inside ZRemote with mouse click, drag, wheel, and keyboard fallback behavior close to a native terminal.
2. Preserve ZRemote's local selection/copy behavior when the running application has not enabled mouse reporting.
3. Provide a deliberate override for local selection while a TUI owns the mouse.
4. Make GUI-to-terminal input binary-safe for both modern SGR mouse and legacy byte-oriented mouse reports.
5. Respect terminal keyboard modes needed by TUI fallback navigation.
6. Add focused tests for mouse sequence encoding, terminal mode routing, keyboard encoding, and binary-safe transport.

## Non-Goals

- Do not implement a new terminal emulator.
- Do not add custom Hunk or lazygit integrations in this RFC.
- Do not implement kitty graphics, sixel, or image protocol support.
- Do not change project action configuration. That belongs in a follow-up RFC for first-class Hunk/lazygit actions.
- Do not remove ZRemote copy-on-select behavior for normal shell usage.

## Current State

### Terminal mouse handlers

File: `crates/zremote-gui/src/views/terminal_panel.rs`

The GPUI terminal currently handles mouse input locally:

- left mouse down starts an alacritty selection
- left drag updates selection
- mouse up copies selected text to the system clipboard
- wheel events add pending scrollback deltas

Relevant areas:

- `on_mouse_down(MouseButton::Left, ...)`
- `on_mouse_move(...)`
- `on_mouse_up(MouseButton::Left, ...)`
- `on_scroll_wheel(...)`

### Terminal modes already tracked

The embedded `alacritty_terminal::Term` already tracks the important modes:

- `TermMode::MOUSE_REPORT_CLICK`
- `TermMode::MOUSE_DRAG`
- `TermMode::MOUSE_MOTION`
- `TermMode::SGR_MOUSE`
- `TermMode::UTF8_MOUSE`
- `TermMode::ALT_SCREEN`
- `TermMode::ALTERNATE_SCROLL`
- `TermMode::FOCUS_IN_OUT`
- `TermMode::BRACKETED_PASTE`

This means ZRemote does not need to parse DECSET sequences itself. The PTY output reader feeds output into alacritty; alacritty updates terminal modes; GPUI event handlers can inspect `term.mode()`.

### Input transport

Mouse reporting reuses the existing `InputSender` path, but the WebSocket frame format must preserve bytes end to end.

The previous client path serialized input through JSON text:

- GUI sends `TerminalInput::Data(Vec<u8>)`
- `zremote-client` converted bytes to `String`
- server/core converted that string back with `into_bytes()`

That corrupts non-UTF-8 bytes. SGR mouse is ASCII-safe, but legacy xterm mouse reporting can emit bytes above `0x7f`, so this RFC includes binary input frames:

- terminal input is sent as WebSocket binary frames
- resize/image/control messages remain JSON text frames
- server/core and direct bridge handlers accept binary frames and forward bytes unchanged

## Native Terminal Behavior Target

When the terminal is in normal shell mode:

- left drag selects text locally
- mouse wheel scrolls scrollback
- Ctrl-click opens URLs
- Ctrl+C copies selection if one exists, otherwise sends SIGINT

When an application enables mouse mode:

- left click is forwarded to the application
- left drag is forwarded if drag or motion reporting is enabled
- left release is forwarded to the application
- wheel up/down is forwarded to the application
- local selection requires Shift-left-drag
- Ctrl-click URL detection is disabled unless Shift is held or mouse reporting is inactive

When an application uses alternate screen:

- wheel events should not scroll ZRemote scrollback by default
- if mouse mode is enabled, send mouse wheel reports
- if mouse mode is not enabled, translate wheel to arrow-key input

## Design

### New helper module

Create:

`crates/zremote-gui/src/views/terminal_mouse.rs`

This keeps terminal escape-sequence logic out of the render/event code.

Suggested public surface:

```rust
use alacritty_terminal::term::TermMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButtonKind {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    Drag,
    Move,
    Wheel,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseGridPosition {
    pub col: usize,
    pub row: usize,
}

pub fn terminal_mouse_active(mode: TermMode) -> bool;

pub fn encode_mouse_event(
    mode: TermMode,
    event: MouseEventKind,
    button: MouseButtonKind,
    pos: MouseGridPosition,
    modifiers: MouseModifiers,
) -> Option<Vec<u8>>;

pub fn encode_alt_scroll(mode: TermMode, lines: i32) -> Option<Vec<u8>>;
```

### Mouse mode detection

```rust
pub fn terminal_mouse_active(mode: TermMode) -> bool {
    mode.intersects(
        TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
    )
}
```

Use this only for routing decisions. Encoding still needs to inspect the exact mode:

- click-only mode reports press/release, not drag/move
- drag mode reports press/release and drag while a button is held
- motion mode reports press/release, drag, and passive movement

### Coordinate system

Terminal mouse reports are 1-based:

- top-left cell is `1;1`
- x is column + 1
- y is row + 1

ZRemote already has `pixel_to_grid(...)` in `terminal_panel.rs`. Add a lighter helper or reuse that result to get viewport-relative row/col. For reporting to the PTY, use viewport coordinates, not scrollback line coordinates. A full-screen TUI sees only the visible screen.

Selection still needs alacritty grid coordinates with `display_offset`; mouse reporting should use unclipped viewport coordinates.

### SGR mouse encoding

Prefer SGR mouse when `TermMode::SGR_MOUSE` is set. Most modern TUIs request SGR mode because it supports large terminal dimensions and clean release events.

Format:

```text
CSI < Cb ; Cx ; Cy M   press/drag/wheel
CSI < Cb ; Cx ; Cy m   release
```

Where:

- `Cb` is button code plus modifier bits
- `Cx` is 1-based column
- `Cy` is 1-based row

Base button codes:

| Event | Button | Base code |
|-------|--------|-----------|
| press | left | 0 |
| press | middle | 1 |
| press | right | 2 |
| release | any | 0, with trailing `m` |
| drag | left | 32 |
| drag | middle | 33 |
| drag | right | 34 |
| move | none | 35 |
| wheel | up | 64 |
| wheel | down | 65 |

Modifier bits:

| Modifier | Bit |
|----------|-----|
| Shift | 4 |
| Alt | 8 |
| Control | 16 |

Examples:

```text
\x1b[<0;10;5M   left press at col 10 row 5
\x1b[<0;10;5m   left release at col 10 row 5
\x1b[<32;11;5M  left drag at col 11 row 5
\x1b[<64;10;5M  wheel up at col 10 row 5
```

### Legacy X10/normal encoding

If `SGR_MOUSE` is not set but mouse reporting is active, emit the classic xterm encoding:

```text
CSI M Cb Cx Cy
```

Where `Cb`, `Cx`, and `Cy` are each encoded as a single byte offset by 32.

Constraints:

- only emit when `Cb + 32`, `Cx + 32`, and `Cy + 32` are valid single-byte values
- if the coordinate is too large, return `None` rather than sending a corrupt sequence
- `UTF8_MOUSE` can be ignored in the first implementation; modern apps should request SGR

### Wheel routing

The current wheel handler should branch on terminal mode:

1. If mouse reporting is active:
   - encode one wheel event per line delta
   - wheel up uses button code 64
   - wheel down uses button code 65
   - send through `input_tx`
   - do not update local scrollback

2. Else if `ALT_SCREEN` is active:
   - translate wheel up to `\x1b[A`
   - translate wheel down to `\x1b[B`
   - repeat by absolute line count
   - send through `input_tx`
   - do not update local scrollback

3. Else:
   - keep current ZRemote scrollback behavior

This makes lazygit/Hunk panes scroll naturally while preserving shell scrollback outside full-screen apps.

### Keyboard mode routing

The terminal should inspect `TermMode` when encoding special keys:

- normal cursor keys use CSI sequences such as `\x1b[A`
- application cursor mode uses SS3 sequences such as `\x1bOA`
- modified cursor keys keep xterm modifier CSI sequences such as `\x1b[1;5C`

`Ctrl+letter` control-byte encoding must not consume unrelated control combinations. For example, `Ctrl+Right` should fall through to the special-key encoder and emit an xterm modifier sequence.

While `ALT_SCREEN` is active, the focused terminal should capture global ZRemote shortcuts and forward the keystroke to the PTY. This prevents Hunk/lazygit from losing common keys such as `Ctrl+K`, `Ctrl+F`, `Ctrl+Tab`, and `F1`. App-level shortcuts still work outside full-screen terminal apps.

### Local selection override

When mouse reporting is active:

- plain left click/drag goes to the TUI
- Shift-left-drag starts local selection
- Shift release copies selected text

When mouse reporting is inactive:

- preserve current local selection behavior

This mirrors common terminal behavior and avoids surprising users who still need to copy text from full-screen apps.

### URL hover/click behavior

Current Ctrl-hover URL detection should only run when mouse reporting is inactive. When a TUI owns the mouse, Ctrl-click may be meaningful to the app and should be forwarded.

Override:

- Ctrl-hover/click works when mouse reporting is inactive
- Ctrl-Shift-hover/click may be used as the local URL override in mouse mode

### Focus reporting

This RFC is primarily about mouse, but focus reporting is small and fits the same compatibility layer.

When the panel gains focus and `FOCUS_IN_OUT` is active:

```text
\x1b[I
```

When the panel loses focus and `FOCUS_IN_OUT` is active:

```text
\x1b[O
```

If GPUI focus events are awkward to wire cleanly, focus reporting can be a phase 2 task in this RFC.

## Implementation Plan

### Phase 1: Mouse encoder

Create `terminal_mouse.rs`.

Add unit tests:

1. `terminal_mouse_active_false_without_mouse_modes`
2. `terminal_mouse_active_true_for_click_mode`
3. `sgr_left_press_uses_one_based_coordinates`
4. `sgr_left_release_uses_lowercase_m`
5. `sgr_drag_sets_drag_button_code`
6. `sgr_wheel_up_down_codes`
7. `sgr_modifiers_are_encoded`
8. `legacy_x10_left_press`
9. `legacy_x10_rejects_large_coordinates`
10. `click_mode_does_not_encode_drag`
11. `drag_mode_encodes_drag`
12. `motion_mode_encodes_passive_move`

### Phase 2: Route left mouse events

Modify `terminal_panel.rs`.

Add helper logic:

```rust
let local_override = event.modifiers.shift;
let mouse_active = term.mode().intersects(...);

if mouse_active && !local_override {
    if let Some(bytes) = encode_mouse_event(...) {
        let _ = input_tx.send(bytes);
    }
    return;
}
```

Apply this to:

- left mouse down
- left mouse move while left button is pressed
- left mouse up

Important details:

- use viewport row/col for mouse reporting
- keep existing selection path for local override and non-mouse-mode
- stop auto-copy on plain mouse release when the event was forwarded to the TUI

### Phase 3: Route wheel events

Modify `on_scroll_wheel`.

Keep the existing pixel-to-line accumulation. Once a line delta is produced:

- inspect terminal mode
- if mouse mode active, send wheel sequences
- else if alt-screen alternate-scroll active, send arrow sequences
- else update `pending_scroll_delta`

Do not hold the terminal mutex while sending to `input_tx`; compute bytes first, drop lock, then send.

### Phase 4: URL and selection overrides

Update Ctrl-hover and Ctrl-click URL logic:

- inactive mouse mode: current behavior
- active mouse mode: require Ctrl+Shift

Update comments at the top of `terminal_panel.rs` so the local selection behavior documents the Shift override.

### Phase 5: Focus in/out

Wire panel focus changes to send focus events when `FOCUS_IN_OUT` is set.

Candidate approaches:

- use GPUI focus callbacks if available
- otherwise compare `focus_handle.is_focused(window)` across renders and send only on edge transitions

Add a `last_focus_state: bool` field to `TerminalPanel` if using edge detection.

### Binary input transport

Update the GUI-to-terminal WebSocket protocol:

- `zremote-client` sends `TerminalInput::Data` as binary frames, chunked under the message-size limit
- `zremote-core::terminal_ws` treats inbound binary frames as terminal input bytes
- `zremote-agent` direct bridge handler treats inbound binary frames as terminal input bytes
- existing JSON text handling remains for resize, image paste, session state, and backward-compatible text input

Add a regression test that sends bytes such as `[0x1b, b'[', b'M', 32, 200, 201]` and verifies the server receives the exact bytes.

### Phase 6: Manual verification

Manual checks in a ZRemote session:

```sh
lazygit
hunk diff --watch
printf '\e[?1000h\e[?1006h'
```

Expected:

- lazygit file list can be clicked
- lazygit diff pane scrolls with mouse wheel
- lazygit staging hunk selection still works by keyboard
- Hunk sidebar can be clicked
- Hunk diff panes scroll with wheel
- Shift-drag still selects/copies terminal text
- normal shell scrollback still works after exiting the TUI

### Phase 7: Keyboard and binary input compatibility

Implement:

- application cursor mode for unmodified arrows
- xterm modifier sequences for modified arrows, including `Ctrl+Right`
- alt-screen shortcut capture for terminal-focused TUIs
- binary-safe WebSocket input frames
- direct bridge binary input handling

## Tests

### Unit tests

Most coverage should live in `terminal_mouse.rs` because sequence generation is deterministic and does not require GPUI.

### Integration-ish tests

Add narrow tests around routing helpers if the event handling is factored enough to test without GPUI.

Avoid brittle full GUI tests for phase 1. The important failure modes are incorrect escape sequences and incorrect routing by terminal mode.

### Regression cases

1. Shell mode: wheel still scrolls local scrollback.
2. Mouse mode: wheel sends SGR wheel bytes.
3. Mouse mode + Shift: drag selects locally.
4. Alt screen + no mouse: wheel sends arrow bytes.
5. Mouse mode + Ctrl-click: forwarded to app.
6. Mouse mode + Ctrl+Shift-click URL: opens URL locally if URL detection matches.
7. Application cursor mode: unmodified arrows use SS3 sequences.
8. Modified cursor keys: `Ctrl+Right` emits xterm modifier CSI instead of being dropped.
9. Binary transport: non-UTF-8 terminal input bytes survive unchanged.

## Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Mouse events break local selection | High | Shift override and mode-gated routing |
| Incorrect coordinates in scrollback | Medium | Use viewport coordinates for PTY mouse reports; keep grid coordinates only for local selection |
| Legacy mouse encoding corrupts large coordinates or non-UTF-8 bytes | Medium | Return `None` when coordinates exceed legacy range; send input via binary WebSocket frames |
| TUI receives duplicate scroll events | Medium | Branch wheel routing so local scrollback is skipped when forwarding |
| Ctrl-click behavior changes in mouse mode | Low | Use Ctrl+Shift as local URL override |
| Focus event spam | Low | Send only on focus edge transitions |

## Open Questions

1. Should local selection override be Shift-drag only, or should Alt-drag also work?
2. Should right click be forwarded in mouse mode immediately, or reserved for a future context menu?
3. Should we expose a user setting for "mouse reporting enabled" in case a remote TUI misbehaves?
4. Should focus reporting ship with this RFC or be split into a keyboard-compatibility RFC?

## Future Work

- Pane-targeted binary terminal input framing if pane-specific input becomes user-facing.
- Kitty keyboard protocol support.
- OSC52 clipboard store/load integration.
- First-class project/worktree action templates for `lazygit`, `hunk diff --watch`, and `hunk session review`.
- Optional `.lazygit.yml` and `.hunk/config.toml` scaffolding in project configuration.
