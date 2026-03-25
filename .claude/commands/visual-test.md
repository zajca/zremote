# Visual Test - GPUI Terminal

Visual testing skill for the GPUI native terminal application. Takes a screenshot of the running GPUI app and analyzes visual quality.

## Prerequisites

Before testing, verify:
1. Agent/server is running on localhost:3000 (`curl -s http://localhost:3000/api/mode`)
2. GPUI app binary exists (`cargo build -p zremote-gpui` if needed)
3. `grim` is installed for Wayland screenshots

## Workflow

### Step 1: Prepare test content

Run the terminal test pattern script in a terminal session to have rich content for visual analysis:

```bash
bash scripts/terminal-test-patterns.sh
```

This outputs ANSI color grids, Unicode box drawing, bold/dim text, and alignment patterns.

### Step 2: Take screenshot

```bash
grim /tmp/zremote-gpui-screenshot.png
```

If the GPUI window is not full-screen, use `grim -g "$(slurp)"` to select the window region interactively, or find the window geometry and crop:

```bash
# Full screen capture as fallback
grim /tmp/zremote-gpui-screenshot.png
```

### Step 3: Analyze screenshot

Read the screenshot file with the Read tool:

```
Read /tmp/zremote-gpui-screenshot.png
```

### Step 4: Visual quality checklist

Evaluate these aspects and report findings:

**Font rendering:**
- [ ] Monospace font is consistent (all characters same width)
- [ ] No missing glyphs (squares or question marks)
- [ ] Text is sharp, not blurry or aliased badly
- [ ] Unicode characters render correctly (box drawing, arrows, emoji)

**Colors:**
- [ ] 16 standard ANSI colors are distinct and visible
- [ ] Bright variants differ from normal variants
- [ ] Background color is consistent (no artifacts)
- [ ] Foreground text has sufficient contrast against background
- [ ] Bold text is visually distinct (brighter or heavier weight)
- [ ] Dim text is visually distinct (lower opacity/brightness)

**Layout & spacing:**
- [ ] Characters align in a grid (columns line up vertically)
- [ ] Line height is consistent (no varying gaps between rows)
- [ ] No overlapping characters
- [ ] Cursor is visible and positioned correctly
- [ ] No horizontal or vertical clipping of content

**Terminal behavior:**
- [ ] Scrollback content is visible (if scrolled)
- [ ] Window chrome (title bar, borders) looks correct
- [ ] No rendering artifacts (stale pixels, tearing, flickering)

### Step 5: Report

Provide a structured report:

```
## Visual Test Report

**Overall quality:** [Good / Acceptable / Poor]

### Issues found:
1. [Issue description] - [Severity: critical/major/minor]
   - Location: [where in the screenshot]
   - Expected: [what it should look like]
   - Actual: [what it looks like]

### Passed checks:
- [List of checks that passed]

### Recommendations:
- [Specific fixes to improve rendering]
```

## Tips

- Run test patterns before screenshotting to have diverse content
- Compare against a reference terminal (Alacritty, kitty) running the same test patterns
- Check both light and dark themes if supported
- Test with different font sizes if the GPUI app supports it
- For cursor testing, ensure the terminal is focused when taking the screenshot
