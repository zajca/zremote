# MyRemote

A self-hosted platform for managing remote machines through the browser. Connect agents on your servers, open terminal sessions, monitor AI agentic loops in real-time, track token usage and costs, and receive Telegram notifications -- all from a single dashboard.

## Architecture

```
┌─────────────┐       HTTP / WebSocket       ┌─────────────────┐       WebSocket       ┌─────────────┐
│   Browser    │  <───────────────────────>   │  MyRemote Server │  <────────────────>   │    Agent     │
│  (React UI)  │                             │   (Axum + SQLite) │                      │ (remote host)│
└─────────────┘                              └─────────────────┘                       └─────────────┘
                                                    │
                                              ┌─────┴─────┐
                                              │ Telegram   │
                                              │ (optional) │
                                              └───────────┘
```

## Features

- **Terminal sessions** -- Open interactive PTY sessions on remote machines directly from the browser (xterm.js)
- **Agentic loop monitoring** -- Track AI agent tool calls, transcripts, token usage, and costs in real-time
- **Tool permissions** -- Approve, reject, or auto-approve agent tool calls with configurable permission rules
- **Project discovery** -- Automatically scan and manage projects on remote hosts
- **Analytics dashboard** -- Token usage, cost tracking, session statistics with charts
- **Full-text search** -- Search across agentic loop transcripts (FTS5)
- **Telegram notifications** -- Get notified about host connections, session events, and pending tool approvals
- **Multi-host management** -- Connect and manage multiple remote machines from one server

## Quick Start

### Prerequisites

- [Nix](https://nixos.org/download/) (recommended) or manually install: Rust 1.94+, Bun 1.3+, Node.js 22+, SQLite
- A shared secret token for authentication

### 1. Start the server

```bash
nix develop

export MYREMOTE_TOKEN="your-secret-token"
cargo run -p myremote-server
```

Server starts on `http://localhost:3000`.

### 2. Connect an agent

On the remote machine (or another terminal for local testing):

```bash
nix develop

export MYREMOTE_TOKEN="your-secret-token"
export MYREMOTE_SERVER_URL="ws://localhost:3000/ws/agent"
cargo run -p myremote-agent
```

### 3. Open the web UI

```bash
cd web
bun install
bun run dev
```

Open `http://localhost:5173` in your browser. Connected hosts appear automatically.

## Configuration

| Variable | Required | Component | Default | Description |
|---|---|---|---|---|
| `MYREMOTE_TOKEN` | Yes | Server + Agent | -- | Shared authentication token |
| `MYREMOTE_SERVER_URL` | Yes | Agent | -- | Server WebSocket URL (e.g. `ws://host:3000/ws/agent`) |
| `DATABASE_URL` | No | Server | `sqlite:myremote.db` | SQLite database path |
| `MYREMOTE_PORT` | No | Server | `3000` | Server listen port |
| `TELEGRAM_BOT_TOKEN` | No | Server | -- | Telegram bot token for notifications |
| `RUST_LOG` | No | Both | `info` | Log level filter (e.g. `debug`, `myremote_server=debug`) |

## Development

```bash
nix develop                          # Enter dev shell with all dependencies

# Rust
cargo build                          # Build all crates
cargo test --workspace               # Run all 207 tests
cargo clippy --workspace             # Lint

# Web
cd web
bun install                          # Install dependencies
bun run dev                          # Dev server (proxies API to :3000)
bun run build                        # Production build
bun run test                         # Run tests
bun run typecheck                    # TypeScript check
bun run lint                         # ESLint
bun run format                       # Prettier
```

## Project Structure

```
myremote/
├── crates/
│   ├── myremote-protocol/           # Shared WebSocket message types
│   ├── myremote-server/             # Axum server (REST API + WebSocket + Telegram)
│   │   └── migrations/              # SQLite migrations (4 files)
│   └── myremote-agent/              # Agent binary (PTY, project scanning, loop detection)
├── web/                             # React frontend (Vite + TypeScript + Tailwind)
│   └── src/
│       ├── components/              # UI components (terminal, agentic panels, sidebar)
│       ├── pages/                   # Route pages (dashboard, sessions, analytics, etc.)
│       ├── stores/                  # Zustand state management
│       └── lib/api.ts               # REST API client
├── docs/                            # Design documents and implementation plans
├── flake.nix                        # Nix development shell
└── Cargo.toml                       # Workspace root
```

## License

MIT
