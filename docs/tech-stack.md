# ZRemote — Technology Stack

Version-pinned technology choices for ZRemote (March 2026).

---

## Server (Rust / Axum)

| Crate | Version | Purpose |
|---|---|---|
| axum | 0.8.8 | HTTP/WebSocket framework |
| tokio | 1.50.0 | Async runtime |
| tower | 0.5.3 | Middleware |
| sqlx | 0.8.6 | Async SQLite/PostgreSQL |
| serde | 1.0.228 | Serialization |
| serde_json | 1.0.149 | JSON serialization |
| uuid | 1.x | Session/machine IDs |
| tracing | 0.1.x | Structured logging |
| tracing-subscriber | 0.3.x | Log subscribers |
| teloxide | 0.17.0 | Telegram bot |

## Host Agent (Rust)

| Crate | Version | Purpose |
|---|---|---|
| tokio | 1.50.0 | Async runtime |
| tokio-tungstenite | 0.28.0 | WebSocket client |
| portable-pty | 0.9.0 | PTY management |
| serde | 1.0.228 | Serialization |
| serde_json | 1.0.149 | JSON serialization |
| uuid | 1.x | IDs |
| tracing | 0.1.x | Logging |

## GUI (GPUI)

| Crate | Version | Purpose |
|---|---|---|
| gpui | 0.2 | Native desktop UI framework |
| alacritty_terminal | 0.25 | Terminal emulation (VTE processing) |
| rust-embed | 8 | Embedded SVG assets |
| flume | 0.11 | Tokio-GPUI channel communication |
| clap | 4 | CLI argument parsing |

## Database

| Technology | Version | Purpose |
|---|---|---|
| SQLite | 3.51.3 | Embedded database |

## Communication

- **WebSocket (wss://)** — real-time bidirectional (terminal I/O, events)
- **REST API** — CRUD operations (sessions, machines, credentials)
- **JSON messages** — protocol format
