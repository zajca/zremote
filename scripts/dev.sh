#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

PORT="${1:-3000}"

# Ensure web/dist exists (agent needs it even with --web-dir)
[[ -d web/dist ]] || (cd web && bun run build)

# Start agent backend
cargo run -p zremote-agent -- local --port "$PORT" --web-dir ./web/dist/ &
AGENT_PID=$!

# Start Vite dev server
(cd web && bun run dev) &
VITE_PID=$!

cleanup() { kill "$AGENT_PID" "$VITE_PID" 2>/dev/null; }
trap cleanup EXIT INT TERM

echo "Backend:                http://localhost:$PORT"
echo "Frontend (hot-reload):  http://localhost:5173"
wait
