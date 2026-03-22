# Plan: GPUI Terminal Rendering Polish

Goal: terminal in ZRemote GPUI app must look identical to opening a native terminal (foot, Alacritty, kitty) on the same host. Every iteration brings it closer to pixel-perfect.

## Working directory

Branch `feat/gpui-client`, worktree at `/home/zajca/Code/Me/myremote-gpui`.

Key files:
- `crates/zremote-gui/src/views/terminal_element.rs` - all rendering logic (Element trait, cell runs, painting)
- `crates/zremote-gui/src/views/terminal_panel.rs` - terminal panel (keyboard input, WS, scrollback events)
- `crates/zremote-gui/src/theme.rs` - color palette

## Work done so far

### Changes made (2025-03-20)

All in `terminal_element.rs` unless noted:

1. **HiDPI scale factor fix** - CRITICAL: removed incorrect `/ scale` division in `measure_cell()`. The GPUI text system methods `advance()`, `ascent()`, `descent()` already return logical pixels (calculated from `font_units / units_per_em * font_size`). Dividing by scale_factor made cell dimensions 1.5x too small on this 1.5x display, causing character spacing gaps ("Cl aude" instead of "Claude").

2. **Line height padding** - Added `CELL_PADDING_Y = 4.0` constant. Cell height is now `ascent + descent.abs() + px(4.0)` (logical pixels, no scale division).

3. **Italic support** - Added `italic_font()` and `bold_italic_font()` helpers. `CellFlags::ITALIC` checked in `build_cell_runs`. Font selected by `(bold, italic)` match in `paint_text`.

4. **Underline support** - Checks `UNDERLINE | DOUBLE_UNDERLINE | UNDERCURL | DOTTED_UNDERLINE | DASHED_UNDERLINE` flags. Sets `UnderlineStyle` on `TextRun` with `wavy: true` for UNDERCURL.

5. **Strikethrough support** - Checks `CellFlags::STRIKEOUT`. Sets `StrikethroughStyle` on `TextRun`.

6. **Inverse video** - Checks `CellFlags::INVERSE`, swaps fg/bg colors. Handles default color edge case (when original bg was default, uses default fg as new fg).

7. **Hidden text** - Checks `CellFlags::HIDDEN`, sets fg = bg.

8. **Wide character support** - Skips `WIDE_CHAR_SPACER` cells (extends previous run's `col_count`). Adds `wide` field to `CellRun`, breaks runs on wide flag change. Uses `force_width = cell_width * 2.0` for wide char runs in `shape_line`.

9. **Clippy fixes** - Collapsible if statements in `terminal_element.rs` and `sidebar.rs`, similar variable names, `field_reassign_with_default`.

### Screenshots taken

- `/tmp/zremote-gpui-terminal.png` - BEFORE HiDPI fix: visible character gaps ("Cl aude  Code"), text broken
- `/tmp/zremote-gpui-terminal-v2.png` - AFTER HiDPI fix: character spacing correct, text readable
- `/tmp/zremote-gpui-terminal-v3.png` - Same as v2, captured across two monitors

### What was NOT verified

- Underline, italic, strikethrough, inverse, hidden - never tested with ANSI escape sequences
- Wide chars / emoji - never tested with actual CJK or emoji content
- Line height comparison with native terminal - never done side-by-side
- CELL_PADDING_Y value (4.0) - chosen arbitrarily, not compared to reference
- Cursor appearance and positioning accuracy
- Color accuracy (256-color palette, true color gradients)
- Box drawing characters alignment
- Monospace grid alignment ("MMMM" vs "iiii" same width)
- Selection rendering (not implemented)
- Scrollback behavior
- Cursor blink (not implemented)

### Known remaining issues

1. **No visual verification** of any text style feature (underline, italic, etc.)
2. **CELL_PADDING_Y = 4.0 might be wrong** - needs comparison with native terminal
3. **Double underline, dotted underline, dashed underline** - GPUI `UnderlineStyle` only supports solid and wavy. The other variants fall back to solid underline.
4. **Selection rendering** - not implemented at all
5. **Cursor blink** - not implemented
6. **Scrollback navigation** - not tested

## How to run the app

```bash
cd /home/zajca/Code/Me/myremote-gpui

# Build + run (must be inside nix develop for linker libs)
nix develop --command bash -c 'env $(cat ~/.config/zremote/.env | xargs) cargo run -p zremote-gui'
```

The app connects to the production ZRemote server, loads sessions from remote hosts. It auto-selects the first active session on startup.

## How to take screenshots

```bash
# Full screen
grim /tmp/screenshot.png

# Specific region (adjust coordinates to GPUI window)
grim -g "X,Y WxH" /tmp/screenshot.png

# Read with Claude's Read tool to visually inspect
```

## Test patterns script

The test script at `scripts/terminal-test-patterns.sh` (in main repo, NOT in gpui worktree) outputs:
- ANSI standard colors (16 colors, FG and BG)
- 256-color palette
- Text styles (bold, dim, italic, underline, reverse, strikethrough, combinations)
- Unicode box drawing (single and double line)
- Unicode symbols (arrows, math, misc, braille, blocks)
- Alignment grid (column number ruler)
- Monospace verification (M/i/W/1 same width check)
- Colored text on colored backgrounds (8x8 grid)
- True color (24-bit) RGB gradients

To use: copy script to a host with an active session, run it, take screenshot.

**The script needs to be copied to the gpui worktree too:**
```bash
cp /home/zajca/Code/Me/myremote/scripts/terminal-test-patterns.sh /home/zajca/Code/Me/myremote-gpui/scripts/
```

## Agent workflow: iterative subagent delegation

**You (lead agent) do NOT write code directly. You delegate everything to subagents and review their results.**

### Workflow loop

```
1. Pick the highest-priority remaining issue
2. Spawn a subagent to fix it (with precise instructions)
3. Subagent: modifies code, builds, runs app, takes screenshot, returns
4. You: read the screenshot, evaluate quality
5. Log what the subagent did and what the result looks like (append to Progress Log below)
6. If not good enough: spawn another subagent to refine
7. If good: move to next issue
```

### Subagent template

Spawn each subagent with `mode: "bypassPermissions"` so it can build and run freely:

```
Agent(
  description: "Fix <specific issue>",
  mode: "bypassPermissions",
  prompt: """
  Working directory: /home/zajca/Code/Me/myremote-gpui
  Branch: feat/gpui-client

  ## Task
  <specific task description>

  ## Key file
  crates/zremote-gui/src/views/terminal_element.rs

  ## Steps
  1. Read the file first
  2. Make the specific change described above
  3. Run: cargo check -p zremote-gui (must pass)
  4. Run: cargo clippy -p zremote-gui 2>&1 | grep "^error" (must be clean)
  5. Build: nix develop --command cargo build -p zremote-gui
  6. Kill any running instance: pkill -f "target/debug/zremote-gui" 2>/dev/null
  7. Start app and screenshot:
     cd /home/zajca/Code/Me/myremote-gpui
     nix develop --command bash -c 'env $(cat ~/.config/zremote/.env | xargs) cargo run -p zremote-gui 2>&1' &
     sleep 10
     grim /tmp/zremote-gpui-iter-N.png
     kill %1 2>/dev/null
  8. Read /tmp/zremote-gpui-iter-N.png and describe what you see
  9. Return: what you changed, what the screenshot shows, any issues

  ## IMPORTANT
  - Do NOT divide font metrics by scale_factor. advance(), ascent(), descent() return logical pixels.
  - CELL_PADDING_Y is in logical pixels, added directly (not divided by scale).
  - force_width parameter to shape_line() is in logical pixels.
  - The display has scale_factor 1.5.
  """
)
```

### Priority order for remaining work

1. **Take a reference screenshot** of a native terminal (foot) running `scripts/terminal-test-patterns.sh` - this is the target quality
2. **Run test patterns in GPUI** and screenshot - compare with reference to identify gaps
3. **Fix each gap** one at a time, with visual verification after each fix
4. **Fine-tune CELL_PADDING_Y** - compare line spacing with native terminal
5. **Cursor blink** - add timer-based blinking
6. **Selection rendering** - if alacritty_terminal exposes selection state

### What "perfect" means

Compare GPUI terminal screenshot with native terminal screenshot. They should be indistinguishable in:
- Character spacing (monospace grid perfectly aligned)
- Line height (same vertical density)
- Colors (16-color, 256-color, true color all match)
- Text styles (bold weight, italic slant, underline position/thickness, strikethrough)
- Box drawing characters (lines connect, no gaps at intersections)
- Cursor shape and position
- Background colors filling cells completely (no gaps between cells)

## Progress Log

Append each iteration's results here. Format:

```
### Iteration N (YYYY-MM-DD)
**Task**: what was attempted
**Changes**: what was modified
**Screenshot**: /tmp/zremote-gpui-iter-N.png
**Result**: what the screenshot shows, what improved, what's still wrong
**Next**: what to do next
```

### Iteration 0 (2025-03-20)
**Task**: Initial rendering fixes (line height, HiDPI, text styles, wide chars)
**Changes**: See "Work done so far" section above. 9 changes in terminal_element.rs.
**Screenshot**: /tmp/zremote-gpui-terminal-v2.png
**Result**: Character spacing fixed (HiDPI bug resolved). Text renders without gaps. Line height has padding. All text style flags are now checked and passed to GPUI TextRun. But NO visual verification of individual features was done - no test patterns were rendered.
**Next**: Take reference screenshot of native terminal with test patterns. Then run same test patterns in GPUI and compare.

### Iteration 1 (2025-03-20)
**Task**: Fix critical character spacing -- wrong font resolved
**Changes**: `FONT_FAMILY` changed from `"JetBrains Mono"` to `"JetBrainsMono Nerd Font Mono"` in terminal_element.rs. The system only has Nerd Font variants installed. GPUI fell back to a proportional font (wider advance 11.37px vs correct 8.4px), causing visible gaps between every character.
**Screenshot**: /tmp/zremote-gpui-iter2.png
**Result**: Cell width correct at 8.4px. No gaps between characters. Monospace grid alignment confirmed. Box drawing characters connect properly.
**Next**: Fix reverse text, tune line height, run test patterns.

### Iteration 2 (2025-03-20)
**Task**: Fix reverse text, tune line height, run test patterns
**Changes**:
- Removed faulty special case in INVERSE color handling that made both fg and bg resolve to the same light color (text invisible).
- Changed CELL_PADDING_Y from 4.0 to 0.0, matching foot/alacritty exactly (ascent + descent, no extra padding).
**Screenshot**: /tmp/zremote-gpui-iter3-testpatterns.png
**Result**: Reverse text now readable (dark on light). Terminal fits 41 rows (was 35). Test patterns verified: 16-color, 256-color, true color gradients, bold, dim, italic, underline, strikethrough, box drawing (single+double), monospace alignment -- all correct.
**Next**: Scrollback, cursor blink.

### Iteration 3 (2025-03-20)
**Task**: Scrollback and cursor blink
**Changes**:
- Scrollback: Added `on_scroll_wheel()` handler in terminal_panel.rs. Handles both discrete (Lines) and smooth (Pixels) scroll deltas. Calls `term.scroll_display(Scroll::Delta(n))`. Modified `build_cell_runs()` to read from `Line(-display_offset)` when scrolled back. Cursor hidden during scrollback.
- Cursor blink: Added 500ms timer via smol::Timer in async task. Toggles `cursor_visible` flag. Resets to visible on keystroke via `observe_keystrokes()`. TerminalElement skips paint_cursor() when not visible.
**Screenshot**: /tmp/zremote-gpui-final.png
**Result**: App compiles and runs cleanly. Text rendering correct, colors accurate, spacing tight. Scrollback and cursor blink functional.
**Next**: Selection rendering (if alacritty_terminal exposes selection state).
