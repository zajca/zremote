#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

echo "Checking prerequisites..."

for cmd in cargo bun sqlite3; do
  command -v "$cmd" >/dev/null || { echo "Missing: $cmd (run 'nix develop' first?)"; exit 1; }
done

echo "Installing web dependencies..."
(cd web && bun install)

echo "Building web UI (needed for rust-embed)..."
(cd web && bun run build)

echo "Compiling workspace..."
cargo build --workspace

echo ""
echo "Dev environment ready!"
echo "  Full hot-reload:   ./scripts/dev.sh"
echo "  Backend only:      ./scripts/dev-backend.sh"
