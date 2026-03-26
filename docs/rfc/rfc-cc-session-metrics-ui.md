# RFC: Claude Code Session Metrics UI

## Summary

Display Claude Code session metrics (context usage, model, agentic state) in the GUI sidebar, session switcher, command palette, and terminal panel. The data is already collected via ccline and broadcast as `ClaudeSessionMetrics` events but currently ignored by the GUI.

## Motivation

Users running Claude Code sessions in ZRemote have no visibility into:
- Which sessions have Claude Code active
- How much context is consumed (critical for session management)
- Which model is running
- Whether CC is working or waiting for input

This information is already available server-side but not surfaced in the GUI.

## Architecture

### Data Flow (Current)

```
CC status line -> ccline binary -> Unix socket -> Agent listener
    -> DB update (claude_sessions) -> ServerEvent::ClaudeSessionMetrics broadcast
    -> GUI WebSocket -> *** DROPPED (no match arm) ***
```

### Data Flow (Proposed)

```
CC status line -> ccline binary -> Unix socket -> Agent listener
    -> DB update -> ServerEvent::ClaudeSessionMetrics broadcast
    -> GUI WebSocket -> main_view.handle_server_event()
        -> sidebar: update cc_metrics HashMap, re-render
        -> terminal_panel: update badge if current session
        -> session_switcher / command_palette: read from sidebar snapshot
```

### Available Data (from ClaudeSessionMetrics event)

| Field | Type | Source |
|-------|------|--------|
| `session_id` | String | CC status line |
| `model` | Option<String> | display_name (e.g., "Opus 4.6 (1M context)") |
| `context_used_pct` | Option<f64> | Percentage of context window used |
| `context_window_size` | Option<u64> | Total context window in tokens |
| `cost_usd` | Option<f64> | Running session cost |
| `tokens_in` | Option<u64> | Total input tokens |
| `tokens_out` | Option<u64> | Total output tokens |
| `lines_added` | Option<i64> | Lines of code added |
| `lines_removed` | Option<i64> | Lines of code removed |
| `rate_limit_5h_pct` | Option<u64> | 5-hour rate limit usage |
| `rate_limit_7d_pct` | Option<u64> | 7-day rate limit usage |

Note: Claude Code status line does **not** include a "mode" field (plan/code/auto). Mode display is out of scope.

## Design

### New Data Structures

```rust
// In sidebar.rs, alongside existing CcState
#[derive(Clone, Default)]
pub struct CcMetrics {
    pub model: Option<String>,
    pub context_used_pct: Option<f64>,
    pub context_window_size: Option<u64>,
    pub cost_usd: Option<f64>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub lines_added: Option<i64>,
    pub lines_removed: Option<i64>,
    pub rate_limit_5h_pct: Option<u64>,
    pub rate_limit_7d_pct: Option<u64>,
}
```

### Shared Widgets (`cc_widgets.rs`)

| Function | Purpose |
|----------|---------|
| `render_context_bar(metrics, width, height)` | Progress bar: 200k = 100%, colors by usage level |
| `short_model_name(model)` | "Opus 4.6 (1M context)" -> "Opus4.6" |
| `cc_bot_icon(status, size)` | Bot icon colored by AgenticStatus |
| `render_cc_tooltip(metrics, status, task_name)` | Detailed hover tooltip |

### Context Bar Logic

- Base scale: **200,000 tokens = 100%** fill
- Fill calculation: `(used_pct / 100) * (context_window_size / 200_000)`
- Fill color: green (<70%), yellow (70-90%), red (>90%)
- Overflow: when `context_window_size > 200_000` and fill would exceed 100%, show red right-edge indicator
- Bar dimensions: 60px x 4px (sidebar), 40px x 3px (switcher), 50px x 4px (panel)

### Bot Icon

Lucide `bot.svg` -- replaces current Loader/MessageCircle dual-icon approach.

| AgenticStatus | Color | Theme Function |
|---------------|-------|----------------|
| Working | Blue/indigo | `theme::accent()` |
| WaitingForInput | Yellow | `theme::warning()` |
| Error | Red | `theme::error()` |
| Completed | Green | `theme::success()` |

### UI Layouts

#### Sidebar Session Row

Without CC (unchanged):
```
[dot] Session Name                              [X]
```

With CC active + metrics:
```
[Bot(color)] Session Name  [-- task]            [X]
             [====---] Opus4.6
```

- Row 1: Bot icon replaces green dot. Task name from agentic loop.
- Row 2: Context bar + short model name. Only shown when `cc_metrics` exists.
- Tooltip on hover: model, tokens, cost, rate limits, lines changed.

#### Session Switcher (Ctrl+Tab)

```
[dot] [terminal] Title       [====--] Opus4.6  [Bot] task  current
                 Subtitle
```

#### Command Palette (Ctrl+K)

Session accessory area:
```
[Bot(color)] task  [====--] Opus4.6  [dot] 2h
```

#### Terminal Panel Badge

Second badge above existing connection badge:
```
[Bot(color)] Opus4.6 [context bar 50px] 45%    <- new CC badge
[Zap] Direct [dot]                              <- existing connection badge
```

## Implementation Phases

### Phase 1: Data Plumbing
- Add `CcMetrics` struct to `sidebar.rs`
- Handle `ClaudeSessionMetrics` in `sidebar.handle_server_event()`
- Clean up metrics on session close/suspend/host disconnect
- Add `exceeds_200k_tokens` to `ccline/types.rs`
- Forward metrics to terminal panel from `main_view.rs`

### Phase 2: Icon + Shared Widgets
- Add `bot.svg` to `assets/icons/`
- Add `Bot` to `Icon` enum in `icons.rs`
- Create `cc_widgets.rs` with shared rendering helpers

### Phase 3: Sidebar Rendering
- Modify `render_session_item()` for two-row layout
- Replace dot with Bot icon when CC active
- Add tooltip on hover

### Phase 4: Session Switcher + Command Palette
- Thread `cc_metrics` through `SwitcherEntry` and `PaletteSnapshot`
- Update render functions with Bot icon + context bar

### Phase 5: Terminal Panel Badge
- Add CC fields to `TerminalPanel`
- Wire up from `main_view`
- Render CC badge above connection badge

## Files

| File | Action |
|------|--------|
| `crates/zremote-gui/assets/icons/bot.svg` | CREATE |
| `crates/zremote-gui/src/icons.rs` | MODIFY |
| `crates/zremote-gui/src/views/cc_widgets.rs` | CREATE |
| `crates/zremote-gui/src/views/mod.rs` | MODIFY |
| `crates/zremote-gui/src/views/sidebar.rs` | MODIFY |
| `crates/zremote-gui/src/views/main_view.rs` | MODIFY |
| `crates/zremote-gui/src/views/session_switcher.rs` | MODIFY |
| `crates/zremote-gui/src/views/command_palette.rs` | MODIFY |
| `crates/zremote-gui/src/views/terminal_panel.rs` | MODIFY |
| `crates/zremote-agent/src/ccline/types.rs` | MODIFY |

## Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| High-frequency metric updates causing re-renders | Low | GPUI batches `cx.notify()` per frame |
| Sidebar height increase with two-row items | Low | Only +14px per CC session, most users have 1-3 |
| Model name truncation edge cases | Low | Fallback to first 8 chars |
| Protocol compatibility | None | No protocol changes, only GUI-side event handling |

## Verification

1. `cargo check -p zremote-gui` -- compiles
2. `cargo test --workspace` -- all tests pass
3. `cargo clippy --workspace` -- no warnings
4. Visual testing:
   - Bot icon colors by state (blue, yellow, red)
   - Context bar at 20%, 75%, 95% fill
   - Context window > 200k overflow indicator
   - Tooltip content with all/partial fields
   - Session switcher with metrics entries
   - Command palette session accessory
   - Terminal panel CC badge positioning
   - Minimum sidebar width -- no overflow
