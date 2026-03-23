# Research: Technology Stack for ZRemote

## Context
Selection of the best technologies for ZRemote implementation based on analysis of existing projects and the current ecosystem state (2026).

---

## 1. Host-Server Communication (how remote machines connect to central server)

### Pattern: Outbound-only connection (reverse tunnel / relay)
Same pattern used by Claude Code Remote Control, Tailscale DERP, ngrok. The host machine never opens a port — it connects outbound to the server and waits for commands.

**Options:**

| Technology | Used by | Pros | Cons |
|---|---|---|---|
| **WebSocket** | Wetty, CloudCLI, Nexterm, wstunnel | Passes through NAT/firewall/proxy, native browser support, bidirectional | No built-in multiplexing, needs heartbeat |
| **gRPC (HTTP/2)** | Kubernetes agents, Spectro Cloud | Built-in multiplexing, streaming, protobuf, load balancing, retries | Doesn't pass through some proxies, browser doesn't natively support (needs grpc-web) |
| **HTTP long polling** | Claude Code RC | Simplicity, passes everywhere | Higher latency, more requests |
| **SSH reverse tunnel** | Classic pattern | Proven security, widely supported | More complex setup, not native in browser |

**Recommendation for ZRemote**: **WebSocket** as the primary host-to-server channel. Reasons:
- Passes through NAT, firewalls, HTTP proxies without configuration
- Bidirectional real-time (terminal I/O needs low latency)
- Well-supported in Rust ecosystem (tokio-tungstenite)
- Proven in practice (Wetty, CloudCLI, ttyd, Nexterm all use WS)

### Host Authentication
- Host connects with a one-time token / API key
- Server validates and assigns identity
- TLS mandatory (wss://)

---

## 2. Terminal Emulation and PTY

### Desktop Client
**GPUI + alacritty_terminal** — native desktop rendering with VTE processing. GPUI (from Zed editor) provides GPU-accelerated UI, alacritty_terminal handles ANSI escape code parsing and terminal state.

### Backend (PTY on host)
| Library | Language | Notes |
|---|---|---|
| **node-pty** | Node.js/TS | Standard for Node projects (VS Code, CloudCLI) |
| **portable-pty** | Rust | Cross-platform, from wezterm project, actively maintained (v0.9.0, Feb 2025) |
| **pseudoterminal** | Rust | Newer, async support, ConPTY + Unix |
| **os/exec + pty** | Go | Native in Go, simple |

### Architecture
```
[GPUI Desktop Client: alacritty_terminal] <--WebSocket--> [Server] <--WebSocket--> [Host Agent: PTY]
```
Host agent creates a PTY process, streams I/O over WebSocket to the server, server relays to the desktop client.

---

## 3. Technology Stack — Recommendations

### Server (central hub)
- **Rust (Axum)** — high performance, async, Tower middleware ecosystem
- Alternative: Go — simpler, but less memory control
- WebSocket server for host agents + for desktop clients
- REST/GraphQL API for management operations

### Host Agent (runs on remote machines)
- **Rust** — small binary, low memory footprint, portable-pty for PTY
- Alternative: Go — simpler distribution, but larger binary
- Connects via WebSocket to server, manages local PTY sessions

### Desktop UI
- **GPUI** (Rust) — native GPU-accelerated desktop UI framework (from Zed editor)
- alacritty_terminal for VTE processing and terminal state
- Custom element-based rendering with per-character glyph caching

### Communication Protocol
- **WebSocket** (wss://) for real-time bidirectional communication (terminal I/O, events)
- **REST API** for CRUD operations (sessions, machines, credentials)
- Messages: JSON or MessagePack (JSON simpler for debugging, MessagePack more efficient for terminal data)

### Database
- **SQLite** (embedded) to start — zero-config, simple
- Migration to PostgreSQL later if needed

### Telegram
- **Telegram Bot API** — HTTP webhooks or long polling
- Existing Rust crates: `teloxide`
- Existing Go libraries: `telebot`

---

## 4. What Existing Projects Use — Summary

| Project | Backend | Frontend | Communication | PTY |
|---|---|---|---|---|
| CloudCLI | Node.js | React + xterm.js | WebSocket | node-pty |
| claude-code-webui | Deno/Node | React + xterm.js | WebSocket | Claude SDK |
| Nexterm | Node.js | React + xterm.js | WebSocket + SSH | node-pty |
| Wetty | Node.js | xterm.js | WebSocket | node-pty |
| ttyd | C (libwebsockets) | xterm.js | WebSocket | fork/exec |
| GoTTY | Go | xterm.js/hterm | WebSocket | os/exec+pty |
| Claude Code RC | (cloud) | Mobile/Web | HTTPS polling/relay | local Claude CLI |

**Pattern**: All projects use **xterm.js + WebSocket**. Backend varies (Node/Go/C/Rust).

---

## 5. Recommended Architecture for ZRemote

```
┌─────────────────┐     wss://     ┌──────────────────┐     wss://     ┌─────────────────┐
│  GPUI Desktop   │ <-----------> │   ZRemote       │ <-----------> │   Host Agent    │
│  Client         │               │   Server         │               │   (Rust binary) │
│  (alacritty_    │               │   (Rust/Axum)    │               │                 │
│   terminal)     │               │                  │               │   - PTY mgmt    │
                                  │   - Session mgr  │               │   - WS client   │
┌─────────────────┐               │   - OAuth monitor│               │   - Local attach│
│   Telegram Bot  │ <-----------> │   - Telegram bot │               └─────────────────┘
└─────────────────┘               │   - REST API     │
                                  │   - SQLite DB    │
                                  └──────────────────┘
```

### Key Design Decisions

1. **WebSocket everywhere** — unified protocol for host-server and server-client communication
2. **Rust for backend** — performance, small binaries, memory safety, portable-pty ecosystem
3. **GPUI + alacritty_terminal for desktop client** — native performance, GPU-accelerated rendering, VTE processing from alacritty
4. **SQLite to start** — simplicity, zero ops overhead, migrate later if needed
5. **JSON messages initially** — easier debugging, switch to MessagePack for terminal data if perf needed
6. **Outbound-only connections** — hosts never expose ports, always connect to server

### Rust Crate Dependencies (key ones)

**Server:**
- `axum` — HTTP/WebSocket framework
- `tokio` — async runtime
- `tower` — middleware
- `sqlx` — async SQLite/PostgreSQL
- `serde` / `serde_json` — serialization
- `uuid` — session/machine IDs
- `tracing` — structured logging
- `teloxide` — Telegram bot

**Agent:**
- `tokio` — async runtime
- `tokio-tungstenite` — WebSocket client
- `portable-pty` — PTY management
- `serde` / `serde_json` — serialization
- `uuid` — IDs
- `tracing` — logging

**Shared protocol crate:**
- `serde` — message type definitions
- `uuid` — ID types

---

## 6. Security Review

### Deployment Context
Server runs inside private VPN (accessible only when connected). Hosts can run outside VPN. Single user — no RBAC needed.

### Adjusted Threat Model
Server behind VPN significantly reduces attack surface:
- No public internet exposure = no random scanning, no DDoS from outside
- Single user = no RBAC, no multi-tenant isolation needed
- VPN provides network-level authentication layer

**Remaining attack vectors:**
- Host-to-server channel crosses internet (hosts outside VPN)
- Compromised host agent could attack server
- Telegram bot is publicly accessible (Telegram API)
- VPN breach exposes everything

### CRITICAL Findings

| # | Finding | Recommendation |
|---|---------|----------------|
| 1 | **Host-server channel over internet** — hosts outside VPN connect to server, this channel is exposed | TLS mandatory (wss://), strong agent auth token (256-bit, hashed in DB), token rotation on reconnect |
| 2 | **PTY privilege escalation** — no spec on which user runs agent/PTY | Agent MUST run as non-root dedicated user, document expected user setup |
| 3 | **Telegram bot publicly accessible** — anyone can message it | Whitelist authorized Telegram user IDs, validate on every command, strict command parsing |
| 4 | **Agent auth token storage** — if host is compromised, token is leaked | Store token in file with 0600 perms, consider `zeroize` in memory, token revocation endpoint on server |

### HIGH Findings

| # | Finding | Recommendation |
|---|---------|----------------|
| 1 | **No agent authentication beyond token** — stolen token = full access | Token + host fingerprint (machine-id or SSH host key), reject mismatched |
| 2 | **No session timeout** — idle sessions stay open forever | Configurable idle timeout (default 30min), server-side enforcement |
| 3 | **Terminal data may contain secrets** — passwords, API keys in output | Log session metadata only (not raw I/O), warn in docs about sensitive data |
| 4 | **Telegram webhook verification** — fake webhooks if using webhook mode | Use long-polling (simpler, no webhook exposure) or verify secret header |
| 5 | **No cargo audit in workflow** — vulnerable dependencies undetected | Add `cargo audit` to CI/build process |
| 6 | **REST API needs basic auth** — even behind VPN, protect against accidental exposure | Simple API key or session token for REST endpoints |

### MEDIUM Findings
- Agent version verification (server rejects old agents)
- Heartbeat with timeout for dead connection detection
- Input validation on REST API (defense in depth)
- SQLite file permissions (0600)
- Structured error responses (no stack traces to client)

### Dropped (mitigated by VPN / single-user)
- ~~RBAC/ACLs~~ — single user
- ~~CORS policy~~ — native desktop client, no browser origin concerns
- ~~DoS protection~~ — VPN filters traffic
- ~~MFA~~ — VPN is the second factor
- ~~E2E encryption~~ — nice to have, not critical for single user behind VPN
- ~~Network segmentation~~ — VPN handles this
- ~~GDPR/compliance~~ — personal project
- ~~Intrusion detection~~ — VPN + single user
- ~~Code signing~~ — overkill for personal deployment

---

## 7. Architecture Review

### CRITICAL Issues

| # | Issue | Recommendation |
|---|-------|----------------|
| 1 | **No server crash recovery** — active sessions lost on restart | Persist session state in DB, agent auto-reconnect with exponential backoff, resume PTY sessions |
| 2 | **Session state lifecycle undefined** — no schema, no cleanup | Design DB schema, define when sessions are created/closed/cleaned up |
| 3 | **SQLite sufficient for personal use** — but need WAL mode | SQLite is fine for single-user with <50 machines. Enable WAL mode, busy_timeout. Plan PostgreSQL only if scaling beyond that. |

### HIGH Issues

| # | Issue | Recommendation |
|---|-------|----------------|
| 1 | **Protocol underspecified** — no message format, no multiplexing | Define message types: `{type, session_id, payload}`. Start with JSON, add binary framing later if needed |
| 2 | **No reconnection strategy** — agent/client disconnect = session lost | Agent: exponential backoff reconnect (1s–5min). Client: auto-reconnect with terminal history replay. Server: keep PTY alive for N minutes after disconnect |
| 3 | **No deployment strategy** | Server: Docker or systemd. Agent: single binary + install script. Config: TOML file + env vars |
| 4 | **No observability** | Structured logging (tracing + JSON), basic Prometheus metrics, `/health` endpoint, dead host detection via heartbeat |
| 5 | **No graceful shutdown** | Drain connections, notify agents, give 30s for reconnect before killing sessions |
| 6 | **portable-pty risk** — v0.9.0, unproven | Test early in prototype phase. Fallback: `std::process::Command` + raw PTY via `nix` crate on Linux |

### MEDIUM Issues
- No protocol versioning (add version field in handshake)
- JSON overhead for terminal data (switch to binary/MessagePack later if needed)
- No flow control / backpressure on WebSocket
- No latency targets defined
- teloxide abandonment risk (fallback: raw HTTP Telegram API)

### Technology Choices Assessment

| Choice | Verdict | Note |
|--------|---------|------|
| Rust backend (Axum) | Good | Performance, single binary, async |
| portable-pty | Test early | Risk: v0.9.0. Fallback: nix crate |
| teloxide | OK | Fallback: raw HTTP API |
| SQLite | Fine for personal use | WAL mode, consider PostgreSQL only at scale |
| GPUI + alacritty_terminal | Native desktop, GPU-accelerated | Zed ecosystem, actively maintained |
| WebSocket | Correct choice | Proven pattern |

---

## 8. Recommended DB Schema (minimal)

```sql
hosts: id, name, fingerprint, auth_token_hash, last_seen, agent_version, status
sessions: id, host_id, created_at, closed_at, shell, status
```

Terminal history: keep in-memory ring buffer on server (last 1000 lines per session). No need to persist raw I/O to DB.

---

## 9. Recommended Next Steps (priority order)

1. **Protocol spec** — define WebSocket message types (connect, terminal_data, resize, heartbeat, error)
2. **Agent auth flow** — token generation, storage, validation, rotation
3. **Session lifecycle** — create/attach/detach/close/timeout states
4. **DB schema** — hosts, sessions, auth tokens
5. **Reconnection strategy** — agent and client reconnect with state recovery
6. **Telegram bot security** — user ID whitelist, command validation
7. **Observability basics** — structured logging, health endpoint, heartbeat
8. **Deployment** — Docker/systemd for server, install script for agent
