#!/bin/bash
# ZRemote agent enrollment script.
# Usage: ZREMOTE_ENROLL_CODE=<code> ZREMOTE_SERVER=<url> bash <(curl -fsSL "$ZREMOTE_SERVER/enroll.sh")
#
# Required environment variables:
#   ZREMOTE_ENROLL_CODE  One-time enrollment code from the server admin panel
#   ZREMOTE_SERVER       Server base URL (e.g. https://myserver.example.com)
#
# Optional:
#   ZREMOTE_INSTALL_DIR  Installation directory (default: $HOME/.local/bin)
#   ZREMOTE_ARCH         CPU architecture override (auto-detected if unset)
#   ZREMOTE_OS           OS override (auto-detected if unset)
#
# Security note: checksum verification below protects against partial downloads
# and accidental corruption. It does NOT protect against a compromised server —
# if the server is malicious, it can serve both a bad binary and a matching checksum.
# Use this script only from a server you trust.

set -euo pipefail

die() {
    echo "error: $*" >&2
    exit 1
}

# Validate required env vars.
[[ -n "${ZREMOTE_ENROLL_CODE:-}" ]] || die "ZREMOTE_ENROLL_CODE is required"
[[ -n "${ZREMOTE_SERVER:-}" ]]      || die "ZREMOTE_SERVER is required"

SERVER="${ZREMOTE_SERVER%/}"
INSTALL_DIR="${ZREMOTE_INSTALL_DIR:-$HOME/.local/bin}"

# Detect OS and architecture.
OS="${ZREMOTE_OS:-$(uname -s | tr '[:upper:]' '[:lower:]')}"
case "$OS" in
    linux)  OS="linux"  ;;
    darwin) OS="macos"  ;;
    *)      die "unsupported OS: $OS" ;;
esac

MACHINE="${ZREMOTE_ARCH:-$(uname -m)}"
case "$MACHINE" in
    x86_64|amd64)   ARCH="x86_64" ;;
    aarch64|arm64)  ARCH="aarch64" ;;
    *)              die "unsupported architecture: $MACHINE" ;;
esac

BINARY_URL="$SERVER/releases/latest/zremote-agent-$OS-$ARCH"
CHECKSUM_URL="$BINARY_URL.sha256"

echo "==> Installing zremote-agent"
echo "    server:  $SERVER"
echo "    os/arch: $OS/$ARCH"
echo "    install: $INSTALL_DIR"

# Check for required tools.
SHA_CMD=""
if command -v sha256sum >/dev/null 2>&1; then
    SHA_CMD="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
    SHA_CMD="shasum -a 256"
else
    die "neither sha256sum nor shasum found"
fi

mkdir -p "$INSTALL_DIR"

TMPDIR_WORK="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_WORK"' EXIT

BINARY_TMP="$TMPDIR_WORK/zremote-agent"
CHECKSUM_TMP="$TMPDIR_WORK/zremote-agent.sha256"

echo "==> Downloading binary..."
curl -fsSL --progress-bar "$BINARY_URL" -o "$BINARY_TMP"
curl -fsSL "$CHECKSUM_URL" -o "$CHECKSUM_TMP"

echo "==> Verifying checksum..."
EXPECTED="$(awk '{print $1}' "$CHECKSUM_TMP")"
ACTUAL="$($SHA_CMD "$BINARY_TMP" | awk '{print $1}')"
[[ "$EXPECTED" == "$ACTUAL" ]] || die "checksum mismatch (expected $EXPECTED, got $ACTUAL)"

chmod +x "$BINARY_TMP"
mv "$BINARY_TMP" "$INSTALL_DIR/zremote-agent"

echo "==> Installed to $INSTALL_DIR/zremote-agent"

# Add to PATH hint if needed.
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "    (add $INSTALL_DIR to your PATH if needed)" ;;
esac

# Run enrollment. Pass the code via env var to avoid leaking it into
# process argv (visible in /proc/<pid>/cmdline, captured by auditd/systemd journal).
echo "==> Running enrollment..."
ZREMOTE_ENROLL_CODE="$ZREMOTE_ENROLL_CODE" \
    "$INSTALL_DIR/zremote-agent" enroll \
    --server "$SERVER"

# Install systemd user unit (Linux) or launchd plist (macOS).
case "$OS" in
    linux)
        UNIT_DIR="$HOME/.config/systemd/user"
        UNIT_FILE="$UNIT_DIR/zremote-agent.service"
        mkdir -p "$UNIT_DIR"
        cat > "$UNIT_FILE" <<UNIT
[Unit]
Description=ZRemote Agent
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=$INSTALL_DIR/zremote-agent run
Restart=on-failure
RestartSec=5
Environment=ZREMOTE_SERVER_URL=$SERVER/ws/agent

[Install]
WantedBy=default.target
UNIT
        echo "==> Installed systemd unit: $UNIT_FILE"
        if command -v systemctl >/dev/null 2>&1 && systemctl --user status >/dev/null 2>&1; then
            systemctl --user daemon-reload
            systemctl --user enable zremote-agent.service
            systemctl --user start zremote-agent.service
            echo "==> Service started (systemctl --user status zremote-agent)"
        else
            echo "    Run: systemctl --user enable --now zremote-agent"
        fi
        ;;
    macos)
        PLIST_DIR="$HOME/Library/LaunchAgents"
        PLIST_FILE="$PLIST_DIR/com.zremote.agent.plist"
        mkdir -p "$PLIST_DIR"
        cat > "$PLIST_FILE" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.zremote.agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>$INSTALL_DIR/zremote-agent</string>
        <string>run</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>ZREMOTE_SERVER_URL</key>
        <string>$SERVER/ws/agent</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>$HOME/.zremote/agent.log</string>
    <key>StandardErrorPath</key>
    <string>$HOME/.zremote/agent.log</string>
</dict>
</plist>
PLIST
        mkdir -p "$HOME/.zremote"
        echo "==> Installed launchd plist: $PLIST_FILE"
        # Unload existing agent if present (idempotent).
        launchctl unload "$PLIST_FILE" 2>/dev/null || true
        launchctl load -w "$PLIST_FILE"
        echo "==> Agent started (launchctl list com.zremote.agent)"
        ;;
esac

echo ""
echo "Enrollment complete. The agent is now running and connected to $SERVER."
