#!/usr/bin/env bash
# Headless screenshot capture for GPUI applications using cage + grim.
#
# Usage: ./scripts/headless-screenshot.sh [command] [output_path] [delay_seconds]
#
# Requirements: cage (headless Wayland compositor), grim (screenshot tool)
# Both are available in the nix dev shell.
#
# How it works:
#   1. Creates an isolated XDG_RUNTIME_DIR (Wayland needs one)
#   2. Starts cage with WLR_BACKENDS=headless (no GPU, no display)
#   3. Runs the target command inside cage
#   4. Waits for the app to render, then captures with grim
#   5. Cleans up compositor and temp dirs

set -euo pipefail

COMMAND="${1:-./target/debug/zremote-gui --exit-after 5}"
OUTPUT="${2:-/tmp/zremote-gpui-screenshot.png}"
DELAY="${3:-3}"

# Check dependencies
for tool in cage grim; do
    if ! command -v "$tool" &>/dev/null; then
        echo "Error: '$tool' not found. Enter the nix dev shell: nix develop" >&2
        exit 1
    fi
done

# Create isolated Wayland runtime dir
XDG_DIR="$(mktemp -d /tmp/zremote-headless-XXXXXX)"
cleanup() {
    # Kill cage if still running
    if [[ -n "${CAGE_PID:-}" ]] && kill -0 "$CAGE_PID" 2>/dev/null; then
        kill "$CAGE_PID" 2>/dev/null || true
        wait "$CAGE_PID" 2>/dev/null || true
    fi
    rm -rf "$XDG_DIR"
}
trap cleanup EXIT

export XDG_RUNTIME_DIR="$XDG_DIR"
export WLR_BACKENDS=headless
export WLR_LIBINPUT_NO_DEVICES=1

# Start cage compositor with the app
# cage runs the command as its single fullscreen client
echo "Starting headless compositor with: $COMMAND"
cage -- $COMMAND &
CAGE_PID=$!

# Wait for the app to render
echo "Waiting ${DELAY}s for app to render..."
sleep "$DELAY"

# Check if cage is still running
if ! kill -0 "$CAGE_PID" 2>/dev/null; then
    echo "Error: compositor exited early (app may have crashed)" >&2
    exit 1
fi

# Capture screenshot
echo "Capturing screenshot to: $OUTPUT"
WAYLAND_DISPLAY="wayland-0" grim "$OUTPUT"

echo "Screenshot saved: $OUTPUT"
echo "Size: $(du -h "$OUTPUT" | cut -f1)"
