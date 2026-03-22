#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

echo "Checking prerequisites..."

for cmd in cargo sqlite3; do
  command -v "$cmd" >/dev/null || { echo "Missing: $cmd (run 'nix develop' first?)"; exit 1; }
done

echo "Compiling workspace..."
cargo build --workspace

echo ""
echo "Dev environment ready!"
echo "  Backend only:      ./scripts/dev-backend.sh"
echo "  GPUI client:       cargo run -p zremote-gui"
