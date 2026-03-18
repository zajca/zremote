# ZRemote

Remote machine management platform with interactive terminal sessions, AI agent monitoring, and real-time analytics. Runs in three modes: **Local** (single-host, zero-config), **Server** (multi-host via central server), and **MCP** (Claude Code integration over stdio).

## Features

**Terminal & Sessions**
- Interactive PTY sessions in the browser (xterm.js)
- Persistent sessions via tmux -- survive agent restarts, crashes, and updates
- Multi-host management from a single dashboard

**AI Agent Monitoring**
- Agentic loop tracking: tool calls, transcripts, token usage, costs
- Tool permissions: approve, reject, or auto-approve agent tool calls
- Claude Code hooks integration for real-time event capture
- Claude task lifecycle management

**Project Management**
- Auto-discovery of projects (Cargo.toml, package.json, pyproject.toml)
- Git integration: branch info, recent commits, dirty state, worktree operations
- Linear issue integration
- Knowledge extraction via OpenViking

**Analytics & Search**
- Dashboard with token usage, cost tracking, and session statistics (recharts)
- Full-text transcript search (FTS5)
- History browser for past sessions and loops

**Other**
- Telegram notifications (host events, session activity, pending approvals)
- MCP server mode for Claude Code tool integration
- Interactive project configuration with Claude
- Local mode: single binary, embedded web UI, zero configuration

## Quick Start

### Install

**Pre-built binaries** from [GitHub Releases](../../releases):

| Platform | Target |
|----------|--------|
| Linux x86_64 | `zremote-x86_64-unknown-linux-musl.tar.gz` |
| Linux aarch64 | `zremote-aarch64-unknown-linux-musl.tar.gz` |
| macOS x86_64 | `zremote-x86_64-apple-darwin.tar.gz` |
| macOS aarch64 | `zremote-aarch64-apple-darwin.tar.gz` |

Each archive contains `zremote-agent` and `zremote-server` binaries.

**From source** (Nix recommended):

```bash
nix develop                           # Provides Rust, Bun, SQLite, etc.
cd web && bun install && bun run build && cd ..
cargo build --workspace --release
```

Or manually: Rust 1.94+, Bun 1.3+, SQLite.

### Local Mode (recommended for getting started)

```bash
zremote-agent local --port 3000
# Open http://127.0.0.1:3000
```

Single binary with embedded web UI and SQLite database. No server needed.

### Server Mode (multi-host)

```bash
# Server
ZREMOTE_TOKEN=secret cargo run -p zremote-server

# Agent (on each remote host)
ZREMOTE_SERVER_URL=ws://server-host:3000/ws/agent ZREMOTE_TOKEN=secret cargo run -p zremote-agent

# Web UI (development)
cd web && bun install && bun run dev   # :5173 proxies API to :3000
```

### MCP Server (Claude Code integration)

```bash
zremote-agent mcp-serve --project /path/to/project
```

Exposes project knowledge and tools over JSON-RPC stdio transport.

## Architecture

```
LOCAL MODE:

  Browser <--HTTP/WS--> Agent (Axum + embedded UI + SQLite)
                         |-- REST API (/api/*)
                         |-- Terminal WebSocket (/ws/terminal/:id)
                         |-- Events WebSocket (/ws/events)
                         |-- PTY sessions (direct)
                         |-- Agentic detection & hooks

SERVER MODE:

  Browser <--HTTP/WS--> Server (Axum + SQLite) <--WS--> Agent(s)
                              |                          |-- PTY/tmux sessions
                              |-- Telegram bot           |-- Agentic detection
                              |-- REST API               |-- Project scanning
                              |-- Event broadcast        |-- Claude Code hooks
```

## Configuration

### Server Mode Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ZREMOTE_TOKEN` | Yes | -- | Shared authentication token |
| `ZREMOTE_SERVER_URL` | Yes (agent) | -- | Server WebSocket URL, e.g. `ws://host:3000/ws/agent` |
| `DATABASE_URL` | No | `sqlite:zremote.db` | SQLite connection string |
| `ZREMOTE_PORT` | No | `3000` | HTTP/WS listen port |
| `TELEGRAM_BOT_TOKEN` | No | -- | Enables Telegram bot integration |
| `RUST_LOG` | No | `info` | Tracing filter level |

### Local Mode CLI Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `3000` | HTTP/WS listen port |
| `--db` | `~/.zremote/local.db` | SQLite database path |
| `--bind` | `127.0.0.1` | Bind address |
| `--web-dir` | embedded | Serve web UI from filesystem (for development) |

## CLI Reference

```
zremote-agent [COMMAND]
```

| Command | Description |
|---------|-------------|
| `run` (default) | Connect to remote server |
| `local` | Run with embedded web server (single-host mode) |
| `mcp-serve` | Run as MCP server over stdio |
| `configure` | Interactive project configuration with Claude |

### `local`

```
zremote-agent local [--port 3000] [--db ~/.zremote/local.db] [--bind 127.0.0.1] [--web-dir PATH]
```

### `mcp-serve`

```
zremote-agent mcp-serve --project PATH [--ov-port 8741]
```

### `configure`

```
zremote-agent configure --project PATH [--model sonnet] [--skip-permissions]
```

## Persistent Sessions (tmux)

When [tmux](https://github.com/tmux/tmux) is installed, terminal sessions automatically survive agent restarts in both local and server mode. No configuration needed -- the agent detects tmux at startup.

### How it works

Without tmux, killing the agent kills all terminal sessions. With tmux:

1. Sessions spawn inside a dedicated tmux server (`tmux -L zremote`)
2. When the agent stops (crash, update, restart), tmux keeps the shells alive
3. The browser shows sessions as **suspended** with a yellow badge
4. When the agent reconnects, it discovers surviving tmux sessions and resumes them
5. Scrollback is preserved -- the browser terminal continues seamlessly

Especially useful for long-running Claude Code sessions that would otherwise be destroyed by agent updates.

### Requirements

- `tmux` installed on the host (any recent version)
- That's it. No configuration, no environment variables.

### Verification

```bash
# Check agent logs at startup for:
# "tmux detected, persistent sessions enabled"

# List active zremote sessions
tmux -L zremote ls

# Test: open a session in the UI, then kill the agent
kill -9 $(pgrep zremote-agent)

# Sessions are still alive
tmux -L zremote ls    # zremote-<uuid>: 1 windows ...

# Restart the agent -- sessions resume automatically
```

### Fallback

If tmux is not installed, the agent uses standard PTY sessions (sessions will not survive agent restarts). The agent logs `"tmux not found, using standard PTY sessions"` in this case.

## Development

### Prerequisites

[Nix](https://nixos.org/download/) (recommended):

```bash
nix develop
```

Or manually install: Rust 1.94+, Bun 1.3+, SQLite.

### Dev Scripts

```bash
./scripts/dev-setup.sh       # First-time setup (checks tools, installs deps, builds)
./scripts/dev.sh             # Full hot-reload: agent :3000 + Vite :5173
./scripts/dev.sh 3001        # Override agent port
./scripts/dev-backend.sh     # Backend only: agent :3000 with embedded UI
```

**Full dev** (`dev.sh`): Open `http://localhost:5173` -- Vite proxies API/WS to agent, frontend hot-reloads on save.

**Backend only** (`dev-backend.sh`): Open `http://localhost:3000` -- embedded UI, no Vite needed.

### Build

```bash
cargo build --workspace                # Full workspace
cd web && bun run build && cd ..       # Web UI (required for rust-embed)
cargo build -p zremote-agent           # Agent with embedded UI (default)
cargo build -p zremote-agent --no-default-features  # Agent without local mode
```

### Tests

```bash
cargo test --workspace                 # Rust tests
cargo clippy --workspace               # Lint
cd web && bun run test                 # Frontend tests
cd web && bun run typecheck            # TypeScript check
```

### Coverage

The project maintains minimum code coverage thresholds, enforced in CI on every push and PR:

| Target | Threshold |
|--------|-----------|
| Backend (lines) | >= 80% |
| Frontend (statements/lines) | >= 75% |
| Frontend (branches/functions) | >= 70% |

```bash
cargo llvm-cov --workspace --html     # Rust coverage -> target/llvm-cov/html/
cd web && bun run test:coverage        # Frontend coverage -> web/coverage/
./scripts/check-coverage.sh           # Full gate check (fails on regression)
```

## Project Structure

```
crates/
  zremote-protocol/     # Shared WebSocket message types
  zremote-core/         # Shared types, DB, queries, message processing
  zremote-server/       # Axum server (multi-host mode, Telegram)
  zremote-agent/        # Agent binary (local/server/MCP/configure modes)
web/                    # React 19 + TypeScript 5.8 + Tailwind CSS 4
scripts/                # Dev workflow scripts
```

## License

MIT
