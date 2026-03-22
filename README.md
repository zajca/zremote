# ZRemote

Remote machine management platform with interactive terminal sessions, AI agent monitoring, and real-time analytics. Native GPUI desktop client. Runs in three modes: **Local** (single-host, zero-config), **Server** (multi-host via central server), and **MCP** (Claude Code integration over stdio).

## Features

**Terminal & Sessions**
- Interactive PTY sessions in a native GPUI desktop app
- Terminal rendering via alacritty_terminal with per-character glyph caching
- Persistent sessions via tmux -- survive agent restarts, crashes, and updates
- Multi-host management from a single sidebar

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
- Token usage, cost tracking, and session statistics
- Full-text transcript search (FTS5)

**Other**
- Telegram notifications (host events, session activity, pending approvals)
- MCP server mode for Claude Code tool integration
- Interactive project configuration with Claude

## Quick Start

### Install

**Pre-built binaries** from [GitHub Releases](../../releases):

| Platform | Target |
|----------|--------|
| Linux x86_64 | `zremote-x86_64-unknown-linux-musl.tar.gz` |
| Linux aarch64 | `zremote-aarch64-unknown-linux-musl.tar.gz` |
| macOS x86_64 | `zremote-x86_64-apple-darwin.tar.gz` |
| macOS aarch64 | `zremote-aarch64-apple-darwin.tar.gz` |

Each archive contains `zremote-agent`, `zremote-server`, and `zremote-gui` binaries.

**From source** (Nix recommended):

```bash
nix develop                           # Provides Rust, system libs, etc.
cargo build --workspace --release
```

Or manually: Rust 1.94+, SQLite, and system libraries (libxcb, libxkbcommon, libfreetype).

### GPUI Desktop Client

```bash
# Connect to a server
cargo run -p zremote-gui -- --server http://myserver:3000

# Or use env var (same as agent, WS path auto-stripped)
ZREMOTE_SERVER_URL=ws://myserver:3000/ws/agent cargo run -p zremote-gui
```

### Local Mode (recommended for getting started)

```bash
zremote-agent local --port 3000
cargo run -p zremote-gui -- --server http://localhost:3000
```

### Server Mode (multi-host)

```bash
# Server
ZREMOTE_TOKEN=secret cargo run -p zremote-server

# Agent (on each remote host)
ZREMOTE_SERVER_URL=ws://server-host:3000/ws/agent ZREMOTE_TOKEN=secret cargo run -p zremote-agent

# GPUI client
cargo run -p zremote-gui -- --server http://server-host:3000
```

### MCP Server (Claude Code integration)

```bash
zremote-agent mcp-serve --project /path/to/project
```

Exposes project knowledge and tools over JSON-RPC stdio transport.

## Architecture

```
LOCAL MODE:

  GPUI App <--REST/WS--> Agent (Axum + SQLite)
                           |-- REST API (/api/*)
                           |-- Terminal WebSocket (/ws/terminal/:id)
                           |-- Events WebSocket (/ws/events)
                           |-- PTY sessions (direct)
                           |-- Agentic detection & hooks

SERVER MODE:

  GPUI App <--REST/WS--> Server (Axum + SQLite) <--WS--> Agent(s)
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

### GUI CLI Flags

| Flag | Env var | Default | Description |
|------|---------|---------|-------------|
| `--server` | `ZREMOTE_SERVER_URL` | `http://localhost:3000` | Server URL (http/ws, path auto-stripped) |
| `--exit-after` | -- | -- | Auto-exit after N seconds (headless testing) |

## CLI Reference

```
zremote-agent [COMMAND]
```

| Command | Description |
|---------|-------------|
| `run` (default) | Connect to remote server |
| `local` | Run HTTP/WS server (single-host mode) |
| `mcp-serve` | Run as MCP server over stdio |
| `configure` | Interactive project configuration with Claude |

### `local`

```
zremote-agent local [--port 3000] [--db ~/.zremote/local.db] [--bind 127.0.0.1]
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
3. The GPUI client shows sessions as **suspended**
4. When the agent reconnects, it discovers surviving tmux sessions and resumes them
5. Scrollback is preserved -- the terminal continues seamlessly

### Requirements

- `tmux` installed on the host (any recent version)
- That's it. No configuration, no environment variables.

### Fallback

If tmux is not installed, the agent uses standard PTY sessions (sessions will not survive agent restarts).

## Development

### Prerequisites

[Nix](https://nixos.org/download/) (recommended):

```bash
nix develop
```

Or manually install: Rust 1.94+, SQLite, libxcb, libxkbcommon, libxkbcommon-x11, libfreetype.

### Build

```bash
cargo build --workspace                # Full workspace
cargo build -p zremote-gui             # GPUI desktop client
cargo build -p zremote-agent           # Agent with local mode (default)
cargo build -p zremote-agent --no-default-features  # Agent without local mode
```

### Tests

```bash
cargo test --workspace                 # Rust tests
cargo clippy --workspace               # Lint
```

## Project Structure

```
crates/
  zremote-gui/            # Native GPUI desktop client
  zremote-protocol/       # Shared WebSocket message types
  zremote-core/           # Shared types, DB, queries, message processing
  zremote-server/         # Axum server (multi-host mode, Telegram)
  zremote-agent/          # Agent binary (local/server/MCP/configure modes)
scripts/                  # Dev workflow scripts
```

## License

MIT
