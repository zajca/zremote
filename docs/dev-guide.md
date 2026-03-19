# Development Guide

## Prerequisites

- [Nix](https://nixos.org/) with flakes (recommended) -- `nix develop` gives you everything
- Or install manually: Rust (stable), Bun, SQLite3

## First-Time Setup

```bash
git clone <repo-url> && cd myremote
nix develop                    # enter dev shell
./scripts/dev-setup.sh         # install deps, build web UI, compile workspace
```

The setup script is idempotent -- safe to re-run anytime.

## Development Modes

### Full Hot-Reload (frontend + backend)

```bash
./scripts/dev.sh
```

Starts two processes:
- **Agent** on `http://localhost:3000` -- serves API and WebSocket endpoints
- **Vite** on `http://localhost:5173` -- proxies API/WS to agent, hot-reloads React code

Open `http://localhost:5173` in your browser. Edit any `.tsx` file and changes appear instantly.

To use a different agent port:

```bash
./scripts/dev.sh 3001
```

### Backend Only

```bash
./scripts/dev-backend.sh
```

Starts the agent with embedded UI (no Vite, no hot-reload). Open `http://localhost:3000`.

Use this when you're only changing Rust code and don't need frontend hot-reload.

### Manual Setup (if you need more control)

```bash
# Terminal 1: Agent with filesystem-served UI
cargo run -p zremote-agent -- local --port 3000 --web-dir ./web/dist/

# Terminal 2: Vite dev server
cd web && bun run dev
```

## Running Tests

```bash
# Rust tests
cargo test --workspace

# Frontend tests
cd web && bun run test

# Type checking
cd web && bun run typecheck

# Lint
cargo clippy --workspace

# Full coverage check (slower)
./scripts/check-coverage.sh
```

## Simultaneous Dev + Production

You can develop ZRemote on a host that's already running a production agent. Local mode uses a separate port and its own SQLite database (`~/.zremote/local.db`), so there's no conflict.

```
Production agent  -->  connects to server via WebSocket (server mode)
Dev agent         -->  localhost:3000, own DB (local mode)
```

## Protocol Compatibility

When changing message types in `zremote-protocol`, follow these rules to avoid breaking running deployments:

| Change | Safe? | How |
|--------|-------|-----|
| Add optional field | Yes | Use `#[serde(default)]` |
| Add new message variant | Yes* | Old versions silently ignore unknown types |
| Add required field | **No** | Use `Option<T>` + `#[serde(default)]` instead |
| Rename or remove field | **No** | Add new field, deprecate old one |

\* Only safe if the receiving side uses `#[serde(other)]` or ignores unknown variants.

## Deployment Order

When deploying changes to production:

1. **Server first** -- agents auto-reconnect with exponential backoff, tmux sessions survive restart
2. **Agents rolling** -- update one at a time, verify reconnection before proceeding to next

## Project Structure

```
scripts/
  dev-setup.sh          # First-time setup
  dev.sh                # Full hot-reload dev environment
  dev-backend.sh        # Backend-only dev
  check-coverage.sh     # Coverage gate check

crates/
  zremote-protocol/     # Shared message types (agent <-> server)
  zremote-core/         # Shared DB, queries, processing
  zremote-server/       # Multi-host server (Axum)
  zremote-agent/        # Agent binary (local + server + MCP modes)

web/                    # React frontend (Vite + TypeScript + Tailwind)
```

## Useful Commands

```bash
# Rebuild web UI (after pulling changes or switching branches)
cd web && bun run build

# Check if tmux sessions survived agent restart
tmux -L zremote ls

# Run agent with verbose logging
RUST_LOG=debug cargo run -p zremote-agent -- local --port 3000
```
