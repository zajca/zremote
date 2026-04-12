# RFC-005: Centralized Keyboard Dispatch

## Context

Keyboard shortcuts are duplicated across 3+ locations in the GUI. The same global shortcuts (Ctrl+K, Ctrl+Tab, Ctrl+Shift+E/P/A, F1) are defined in `main_view.rs` (empty state), `terminal_panel.rs`, and `command_palette/keybindings.rs`. This duplication causes maintenance burden and risk of inconsistency.

## GPUI Constraint

GPUI dispatches `on_key_down` events to the element that owns focus. There is no capture phase or global interceptor. When a modal opens, it steals focus, so its `on_key_down` handler receives events directly. This means we cannot have a single `on_key_down` handler on `MainView` that intercepts all events — child views that hold focus would never propagate keys upward.

## Architecture

### Shared binding registry (`key_bindings.rs`)

A static registry of all keyboard bindings with their scope, keystroke pattern, and metadata (for help modal auto-generation). Bindings are data, not closures — the dispatch function matches keystrokes against the registry and returns an action enum.

```rust
pub struct KeyBinding {
    pub key: &'static str,
    pub modifiers: Modifiers,
    pub scope: KeyScope,
    pub label: &'static str,        // For help modal
    pub description: &'static str,  // For help modal
}

pub enum KeyScope {
    Global,          // Active everywhere (unless consumed by modal)
    Terminal,        // Only when terminal has focus
    CommandPalette,  // Only when palette is open
    SearchOverlay,   // Only when search is open
    SessionSwitcher, // Only when switcher is open
    Modal,           // Any modal (Esc to close)
}

pub enum KeyAction {
    OpenCommandPalette(PaletteTab),
    OpenSessionSwitcher,
    OpenSearch,
    OpenHelp,
    CloseOverlay,
    // Terminal-scoped
    CopySelection,
    PasteClipboard,
    // Palette-scoped
    PaletteNavigateUp,
    PaletteNavigateDown,
    PaletteConfirm,
    PaletteTabNext,
    PaletteTabPrev,
    // etc.
    Unhandled,
}
```

### Dispatch function

A pure function `dispatch_key(key, modifiers, scope) -> KeyAction` that all handlers call instead of duplicating match logic. Each view's `on_key_down` passes its scope context and acts on the returned action.

### Migration plan

| Current handler | Migration |
|---|---|
| `main_view.rs` empty state `on_key_down` | Call `dispatch_key(..., Global)`, handle returned action |
| `terminal_panel.rs` `on_key_down` global shortcuts | Call `dispatch_key(..., Global)` for the shared shortcuts. Keep PTY encoding inline |
| `command_palette/keybindings.rs` toggle shortcuts | Call `dispatch_key(..., CommandPalette)` for Ctrl+K/E/P/A toggle. Keep navigation inline |
| `help_modal.rs` Esc handler | Call `dispatch_key(..., Modal)` |
| `settings_modal.rs` Esc handler | Call `dispatch_key(..., Modal)` |
| `session_switcher.rs` Tab/Esc/modifiers | Keep inline (switcher-specific logic: Ctrl-release confirm, quick-switch) |
| `search_overlay.rs` | Keep inline (char-by-char input, overlay-specific) |

### Double-shift detection

`DoubleShiftDetector` uses `on_modifiers_changed`, not `on_key_down`. Both MainView (empty state) and TerminalPanel have `on_modifiers_changed` handlers for double-shift. The shared dispatch module provides a helper that both locations call.

### Help modal auto-generation

The `SHORTCUTS` constant in `help_modal.rs` is replaced by a function that iterates the binding registry and generates the shortcut list. New bindings automatically appear in the help modal.

### Conflict detection test

A unit test iterates all bindings and verifies no two bindings with the same scope have identical keystroke patterns.

## Phases

1. Create `key_bindings.rs` with binding registry, `KeyAction` enum, `dispatch_key()` function
2. Migrate `main_view.rs` and `terminal_panel.rs` global shortcuts to use dispatch
3. Migrate `command_palette` toggle shortcuts
4. Migrate modal Esc handlers (help, settings)
5. Auto-generate help modal shortcuts
6. Add conflict detection test
