#!/usr/bin/env bash
# E2E test harness for zremote-gui test introspection system.
#
# Starts a headless Wayland compositor (cage), builds and launches the GPUI app
# with --test-introspect, waits for the introspection HTTP server, and exports
# helper functions for interactive testing.
#
# Usage:
#   source scripts/e2e-test.sh          # Source to get helper functions
#   ./scripts/e2e-test.sh               # Run directly to see usage info
#
# Requirements: cage, grim, wlrctl, wtype, jq, curl
# All are available in the nix dev shell: nix develop

set -euo pipefail

# --- Configuration ---

E2E_PORT_FILE="/tmp/zremote-gui-test-port"
E2E_APP_BINARY="${E2E_APP_BINARY:-target/debug/zremote-gui}"
E2E_BUILD="${E2E_BUILD:-1}"           # Set to 0 to skip building
E2E_SERVER_URL="${E2E_SERVER_URL:-http://localhost:3000}"

# --- Dependency check ---

_e2e_check_deps() {
    local missing=()
    for tool in cage grim wlrctl wtype jq curl; do
        if ! command -v "$tool" &>/dev/null; then
            missing+=("$tool")
        fi
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        echo "Error: missing tools: ${missing[*]}" >&2
        echo "Enter the nix dev shell: nix develop" >&2
        return 1
    fi
}

# --- Cleanup ---

_e2e_xdg_dir=""
_e2e_cage_pid=""
_e2e_app_pid=""

_e2e_cleanup() {
    echo "[e2e] Cleaning up..."

    # Kill app
    if [[ -n "${_e2e_app_pid:-}" ]] && kill -0 "$_e2e_app_pid" 2>/dev/null; then
        kill "$_e2e_app_pid" 2>/dev/null || true
        wait "$_e2e_app_pid" 2>/dev/null || true
    fi

    # Kill cage compositor
    if [[ -n "${_e2e_cage_pid:-}" ]] && kill -0 "$_e2e_cage_pid" 2>/dev/null; then
        kill "$_e2e_cage_pid" 2>/dev/null || true
        wait "$_e2e_cage_pid" 2>/dev/null || true
    fi

    # Clean port file
    rm -f "$E2E_PORT_FILE"

    # Clean temp XDG dir
    if [[ -n "${_e2e_xdg_dir:-}" ]] && [[ -d "$_e2e_xdg_dir" ]]; then
        rm -rf "$_e2e_xdg_dir"
    fi

    echo "[e2e] Cleanup done."
}

# --- Helper functions ---

# List all tracked elements with their bounds (JSON)
e2e_elements() {
    curl -s "http://localhost:${E2E_PORT}/elements" | jq .
}

# Get single element bounds by ID
e2e_element() {
    local id="$1"
    curl -s "http://localhost:${E2E_PORT}/elements/${id}" | jq .
}

# Click an element by ID (computes center coords, uses wlrctl via Wayland protocol).
# wlrctl only supports relative pointer movement, so we first reset to (0,0) by
# moving a large negative amount, then move to the target coordinates.
e2e_click() {
    local id="$1"
    local bounds
    bounds=$(curl -s "http://localhost:${E2E_PORT}/elements/${id}")
    local x y
    x=$(echo "$bounds" | jq '.x + .w / 2' | cut -d. -f1)
    y=$(echo "$bounds" | jq '.y + .h / 2' | cut -d. -f1)
    if [[ -z "$x" ]] || [[ "$x" = "null" ]]; then
        echo "ERROR: Element '$id' not found" >&2
        return 1
    fi
    # Reset pointer to origin (0,0) then move to target
    WAYLAND_DISPLAY="$E2E_WAYLAND_DISPLAY" wlrctl pointer move -10000 -10000
    sleep 0.02
    WAYLAND_DISPLAY="$E2E_WAYLAND_DISPLAY" wlrctl pointer move "$x" "$y"
    sleep 0.05
    WAYLAND_DISPLAY="$E2E_WAYLAND_DISPLAY" wlrctl pointer click left
}

# Send keyboard shortcut via wtype (Wayland-aware).
# Accepts shortcuts like "ctrl+k", "ctrl+shift+p", "Return", "Tab".
# For key combos, uses -M/-m (modifier hold/release) + -k (key tap).
e2e_key() {
    local input="$1"
    local -a parts
    IFS='+' read -ra parts <<< "$input"

    if [[ ${#parts[@]} -eq 1 ]]; then
        # Single key, no modifiers
        WAYLAND_DISPLAY="$E2E_WAYLAND_DISPLAY" wtype -k "${parts[0]}"
        return
    fi

    # Last part is the key, everything before is a modifier
    local key="${parts[${#parts[@]}-1]}"
    local -a wtype_args=()
    for ((i=0; i<${#parts[@]}-1; i++)); do
        wtype_args+=(-M "${parts[$i]}")
    done
    wtype_args+=(-k "$key")
    for ((i=${#parts[@]}-2; i>=0; i--)); do
        wtype_args+=(-m "${parts[$i]}")
    done

    WAYLAND_DISPLAY="$E2E_WAYLAND_DISPLAY" wtype "${wtype_args[@]}"
}

# Type text via wtype (Wayland-aware)
e2e_type() {
    local text="$1"
    WAYLAND_DISPLAY="$E2E_WAYLAND_DISPLAY" wtype "$text"
}

# Take screenshot, return file path
e2e_screenshot() {
    local path="${1:-/tmp/zremote-e2e-$(date +%s).png}"
    WAYLAND_DISPLAY="$E2E_WAYLAND_DISPLAY" grim "$path"
    echo "$path"
}

# Wait for UI to re-render after an action
e2e_wait_render() {
    local timeout="${1:-5}"
    local current_gen
    current_gen=$(curl -s "http://localhost:${E2E_PORT}/elements" | jq -r '.generation // 0')
    curl -s --max-time "$timeout" "http://localhost:${E2E_PORT}/ready?after=${current_gen}" > /dev/null
}

# Get app state (JSON)
e2e_state() {
    curl -s "http://localhost:${E2E_PORT}/state" | jq .
}

# Stop the E2E environment
e2e_stop() {
    _e2e_cleanup
}

# --- Startup ---

_e2e_start() {
    _e2e_check_deps

    # Build the app with test-introspection feature
    if [[ "$E2E_BUILD" = "1" ]]; then
        echo "[e2e] Building zremote-gui with test-introspection feature..."
        cargo build -p zremote-gui --features test-introspection
    fi

    if [[ ! -f "$E2E_APP_BINARY" ]]; then
        echo "Error: binary not found at $E2E_APP_BINARY" >&2
        echo "Run: cargo build -p zremote-gui --features test-introspection" >&2
        return 1
    fi

    # Clean stale port file
    rm -f "$E2E_PORT_FILE"

    # Create isolated Wayland runtime dir
    _e2e_xdg_dir="$(mktemp -d /tmp/zremote-e2e-XXXXXX)"
    export XDG_RUNTIME_DIR="$_e2e_xdg_dir"
    export WLR_BACKENDS=headless
    export WLR_LIBINPUT_NO_DEVICES=1

    # Start cage compositor with the app
    echo "[e2e] Starting headless Wayland compositor..."
    cage -- "$E2E_APP_BINARY" --test-introspect --server "$E2E_SERVER_URL" &
    _e2e_cage_pid=$!

    # Wait for Wayland socket to appear
    echo "[e2e] Waiting for Wayland socket..."
    local waited=0
    while [[ $waited -lt 10 ]]; do
        if ls "$_e2e_xdg_dir"/wayland-* &>/dev/null 2>&1; then
            break
        fi
        sleep 0.5
        waited=$((waited + 1))
    done

    if ! ls "$_e2e_xdg_dir"/wayland-* &>/dev/null 2>&1; then
        echo "Error: Wayland socket did not appear after 5s" >&2
        _e2e_cleanup
        return 1
    fi

    # Detect the Wayland display name
    E2E_WAYLAND_DISPLAY=$(basename "$_e2e_xdg_dir"/wayland-* | head -1)
    # Strip .lock suffix if we matched the lock file
    E2E_WAYLAND_DISPLAY="${E2E_WAYLAND_DISPLAY%.lock}"
    export E2E_WAYLAND_DISPLAY

    echo "[e2e] Wayland display: $E2E_WAYLAND_DISPLAY"

    # Wait for introspection server port file
    echo "[e2e] Waiting for introspection server..."
    waited=0
    while [[ $waited -lt 30 ]]; do
        if [[ -f "$E2E_PORT_FILE" ]]; then
            E2E_PORT=$(cat "$E2E_PORT_FILE")
            if [[ -n "$E2E_PORT" ]]; then
                break
            fi
        fi
        # Check if cage/app crashed
        if ! kill -0 "$_e2e_cage_pid" 2>/dev/null; then
            echo "Error: compositor exited early (app may have crashed)" >&2
            _e2e_cleanup
            return 1
        fi
        sleep 0.5
        waited=$((waited + 1))
    done

    if [[ -z "${E2E_PORT:-}" ]]; then
        echo "Error: introspection server did not start after 15s" >&2
        echo "Check that --test-introspect flag is supported and test-introspection feature is enabled." >&2
        _e2e_cleanup
        return 1
    fi

    export E2E_PORT

    echo "[e2e] Introspection server ready on port $E2E_PORT"
    echo "[e2e] E2E test environment is ready."
    echo ""
    echo "Available commands:"
    echo "  e2e_elements             List all tracked elements (JSON)"
    echo "  e2e_element <id>         Get single element bounds"
    echo "  e2e_click <id>           Click element by ID"
    echo "  e2e_key <key>            Send keyboard shortcut (e.g. 'ctrl+k')"
    echo "  e2e_type <text>          Type text"
    echo "  e2e_screenshot [path]    Take screenshot, return path"
    echo "  e2e_wait_render [secs]   Wait for UI re-render"
    echo "  e2e_state                Get app state (JSON)"
    echo "  e2e_stop                 Stop E2E environment"
}

# --- Main ---

# Detect if script is being sourced or executed directly
_e2e_is_sourced() {
    [[ "${BASH_SOURCE[0]}" != "${0}" ]]
}

if _e2e_is_sourced; then
    # Sourced: start environment and register cleanup trap
    trap _e2e_cleanup EXIT
    _e2e_start
else
    # Executed directly: print usage
    echo "E2E Test Harness for zremote-gui"
    echo ""
    echo "This script should be SOURCED to set up the test environment:"
    echo ""
    echo "  source scripts/e2e-test.sh"
    echo ""
    echo "This will:"
    echo "  1. Build zremote-gui with test-introspection feature"
    echo "  2. Start a headless Wayland compositor (cage)"
    echo "  3. Launch the GPUI app with --test-introspect"
    echo "  4. Wait for the introspection HTTP server"
    echo "  5. Export helper functions for interactive testing"
    echo ""
    echo "Environment variables:"
    echo "  E2E_BUILD=0              Skip building (use existing binary)"
    echo "  E2E_APP_BINARY=<path>    Custom binary path (default: target/debug/zremote-gui)"
    echo "  E2E_SERVER_URL=<url>     Server URL (default: http://localhost:3000)"
    echo ""
    echo "Helper functions (available after sourcing):"
    echo "  e2e_elements             List all tracked elements (JSON)"
    echo "  e2e_element <id>         Get single element bounds"
    echo "  e2e_click <id>           Click element by ID"
    echo "  e2e_key <key>            Send keyboard shortcut (e.g. 'ctrl+k')"
    echo "  e2e_type <text>          Type text"
    echo "  e2e_screenshot [path]    Take screenshot, return path"
    echo "  e2e_wait_render [secs]   Wait for UI re-render (default 5s timeout)"
    echo "  e2e_state                Get app state (JSON)"
    echo "  e2e_stop                 Stop E2E environment"
fi
