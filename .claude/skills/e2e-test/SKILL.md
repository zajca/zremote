---
name: E2E GUI Test
description: Interactive end-to-end testing of the GPUI desktop app in headless Wayland
---

# E2E GUI Test

Interactive end-to-end testing for the GPUI desktop app. Runs the app in a headless Wayland compositor (cage) with test introspection enabled, providing element-level interaction and verification.

## Prerequisites

- Must be in `nix develop` shell (provides cage, grim, wtype, ydotool)
- Agent must be running: `cargo run -p zremote-agent -- local --port 3000 &` (or the test connects to any available server)

## Setup

Source the test harness to start the headless environment:

```bash
source scripts/e2e-test.sh
```

This will:
1. Build zremote-gui with test-introspection feature
2. Start cage headless Wayland compositor
3. Launch the GPUI app with `--test-introspect`
4. Wait for the introspection HTTP server

To skip the build step (if already built):

```bash
E2E_BUILD=0 source scripts/e2e-test.sh
```

## Available Commands

| Command | Description |
|---------|-------------|
| `e2e_elements` | List all tracked UI elements with bounds (JSON) |
| `e2e_element <id>` | Get single element bounds by ID |
| `e2e_click <id>` | Click element by ID (computes center, uses ydotool) |
| `e2e_key <key>` | Send keyboard shortcut via wtype (e.g. "ctrl+k") |
| `e2e_type <text>` | Type text via wtype |
| `e2e_screenshot [path]` | Take screenshot, returns file path |
| `e2e_wait_render [timeout]` | Wait for UI to re-render after action |
| `e2e_state` | Get app state (selected session, palette open, etc.) |
| `e2e_stop` | Stop the E2E environment and clean up |

## Element IDs

### Sidebar
- `host-header-{host_id}` - Host header row
- `new-session-{host_id}` - New session button per host
- `new-session-local` - New session button (local mode)
- `session-{session_id}` - Session row (clickable)
- `close-{session_id}` - Close session button
- `project-{project_id}` - Project row

### Terminal
- `terminal-content` - Terminal rendering area

### Modals
- `palette-container` - Command palette container
- `palette-item-{index}` - Individual palette items
- `switcher-container` - Session switcher container
- `switcher-item-{index}` - Individual switcher items

## Test Workflow

The recommended pattern is: **action -> wait -> verify**

1. Perform an action (click, keyboard shortcut)
2. Wait for render: `e2e_wait_render`
3. Verify state: check elements, take screenshot, check state

### Verify initial load

```bash
e2e_elements          # Should show sidebar elements
e2e_screenshot        # Visual check of initial state
e2e_state             # Check mode, no session selected
```

### Open command palette

```bash
e2e_key "ctrl+k"
e2e_wait_render
e2e_elements          # palette-container should appear
e2e_screenshot        # Visual check
e2e_key "Escape"      # Close palette
e2e_wait_render
```

### Open session switcher

```bash
e2e_key "ctrl+Tab"
e2e_wait_render
e2e_elements          # switcher-container should appear
```

### Click sidebar element

```bash
e2e_click "new-session-local"
e2e_wait_render
e2e_state             # Check terminal_active is true
e2e_elements          # terminal-content should appear
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `E2E_BUILD` | `1` | Set to `0` to skip building |
| `E2E_APP_BINARY` | `target/debug/zremote-gui` | Custom binary path |
| `E2E_SERVER_URL` | `http://localhost:3000` | Server URL for the app |

## Troubleshooting

- **ydotool permission denied**: ydotool needs write access to `/dev/uinput`. Run `sudo chmod 666 /dev/uinput` or add yourself to the input group.
- **cage not starting**: Ensure you're in `nix develop`. Check `WLR_BACKENDS=headless` is set.
- **No elements returned**: The app may not have rendered yet. Use `e2e_wait_render` or increase the startup wait time.
- **wtype: No compositor**: Ensure `WAYLAND_DISPLAY` is set correctly (the harness handles this).
- **App crashes on start**: Check that the server URL is reachable. For standalone testing, start a local agent first.

## Cleanup

Always call `e2e_stop` when done, or the cleanup happens automatically via EXIT trap when the shell session ends.

## When to run

Run this after any changes to:
- `crates/zremote-gui/src/views/sidebar.rs` (sidebar interactions)
- `crates/zremote-gui/src/views/main_view.rs` (layout, routing)
- `crates/zremote-gui/src/views/terminal_panel.rs` (terminal state)
- `crates/zremote-gui/src/views/terminal_element.rs` (rendering)
- `crates/zremote-gui/src/theme.rs` (colors)
- Keyboard shortcuts or mouse interaction handling
- New UI features that need end-to-end verification
