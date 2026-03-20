#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

PORT="${1:-3000}"

echo "Starting local mode at http://localhost:$PORT"
cargo run -p zremote-agent -- local --port "$PORT"
