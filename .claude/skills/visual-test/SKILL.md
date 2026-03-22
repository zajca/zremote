---
name: visual-test
description: Visual Test - GPUI Terminal
---

# Headless Visual Test for GPUI Terminal

Automated visual regression testing for the GPUI terminal renderer. Builds the app, runs it headless with test patterns, captures a screenshot, and analyzes the rendering.

## Prerequisites

Must be in the nix dev shell (`nix develop`) which provides `cage` and `grim`.

## Steps

1. **Build the app**:
   ```bash
   cargo build -p zremote-gui
   ```

2. **Run headless screenshot capture** with the terminal test patterns script:
   ```bash
   ./scripts/headless-screenshot.sh \
     "bash -c './scripts/terminal-test-patterns.sh; sleep 999'" \
     /tmp/zremote-visual-test.png \
     3
   ```

   If the app needs a server connection, start the local agent first:
   ```bash
   ./scripts/headless-screenshot.sh \
     "./target/debug/zremote-gui --exit-after 8" \
     /tmp/zremote-visual-test.png \
     5
   ```

3. **Read and analyze the screenshot**:
   Use the Read tool on `/tmp/zremote-visual-test.png` to visually inspect the rendered terminal.

4. **Check for rendering issues**:
   - ANSI colors render correctly (16 standard + bright colors visible and distinct)
   - 256-color palette shows smooth gradients without banding
   - True color (24-bit) gradients are smooth
   - Text styles (bold, italic, underline, strikethrough) are visually distinct
   - Unicode box drawing characters align properly (no gaps between segments)
   - Monospace verification: all rows of different characters (M, i, W, 1, |) are the same width
   - Block elements and braille characters render without missing glyphs
   - Font rendering is clean (no artifacts, proper anti-aliasing)
   - Cell spacing is uniform (no irregular gaps)

5. **Report findings**: List any rendering issues found with specific descriptions. If everything looks correct, confirm the visual test passes.

## When to run

Run this after any changes to:
- `crates/zremote-gui/src/views/terminal_element.rs` (rendering)
- `crates/zremote-gui/src/views/terminal_panel.rs` (state/layout)
- `crates/zremote-gui/src/theme.rs` (colors)
- Font configuration or cell sizing logic
