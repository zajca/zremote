# Agent Client Protocol (ACP) — Specification Research

> Audience: ZRemote engineering team evaluating ACP adoption.
> Author: research agent `acp-spec` (team `acp-research`).
> Date: 2026-04-25.
> Sources: official spec at [agentclientprotocol.com](https://agentclientprotocol.com), reference repos `agentclientprotocol/agent-client-protocol`, `agentclientprotocol/rust-sdk`, [crates.io](https://crates.io/crates/agent-client-protocol).

---

## TL;DR (10 lines max)

1. ACP is a **JSON-RPC 2.0** protocol between a **Client** (IDE/editor) and an **Agent** (LLM coding agent), modelled on LSP/MCP.
2. The Client owns the UI, files, terminals, and permissions; the Agent owns the model loop and tool decisions.
3. Default transport is **stdio with newline-delimited JSON** (no `Content-Length` headers — unlike LSP); HTTP/WebSocket is a draft RFD.
4. Lifecycle: `initialize` → optional `authenticate` → `session/new` (or `session/load`/`session/resume`) → many `session/prompt` turns → `session/cancel`/`session/close`.
5. Each prompt turn streams `session/update` notifications (message chunks, plan, tool_call, tool_call_update, available_commands_update, current_mode_update) and ends with a `stopReason`.
6. Tool calls are **fully observable** — the Agent reports `tool_call`/`tool_call_update` to the Client; the Client gates side-effects via `session/request_permission`.
7. The Client may be asked to perform host-side work for the Agent: `fs/read_text_file`, `fs/write_text_file`, `terminal/create|output|wait_for_exit|kill|release` — all gated by capabilities advertised in `initialize`.
8. Versioning is a **single integer MAJOR**; capabilities omitted in `initialize` are treated as unsupported.
9. Extensibility via **`_meta`** fields and **underscore-prefixed methods** (`_zed.dev/...`); reserved keys for W3C trace context (`traceparent`, `tracestate`, `baggage`).
10. The Rust SDK ships as **`agent-client-protocol` v0.11.1** (Apache-2.0, 2026-04-21) on top of **`agent-client-protocol-schema` v0.12.2**; it uses a Role-typestate (`Client`/`Agent`/`Proxy`/`Conductor`) rather than a single trait.

---

## 1. Transport & Framing

### 1.1 stdio (mandatory, recommended baseline)

- The Client spawns the Agent as a subprocess.
- **Wire format:** newline-delimited JSON-RPC 2.0 messages. Each message is a single JSON object terminated by `\n`. Messages **MUST NOT** contain embedded newlines.
- Encoding: UTF-8.
- Stdin: Client → Agent. Stdout: Agent → Client. Both directions are bidirectional JSON-RPC streams (so notifications and requests can flow either way).
- Stderr is **reserved for logging only**; agents MAY emit free-form UTF-8 there. Clients MUST NOT parse stderr as protocol traffic.
- Source: <https://agentclientprotocol.com/protocol/transports.md>.

> **Difference from LSP:** LSP uses `Content-Length: N\r\n\r\n` framed JSON-RPC; ACP uses bare newline-delimited JSON. Simpler, but messages cannot contain raw newlines.
>
> **Difference from MCP:** MCP also uses newline-delimited JSON over stdio, plus optional Streamable HTTP / SSE. ACP currently has no production HTTP transport.

### 1.2 HTTP / WebSocket (draft)

- A **Streamable HTTP / WebSocket transport** is a draft RFD ([rfds/streamable-http-websocket-transport.md](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport.md)) and a recently-formed Transports Working Group ([announcements/transports-working-group.md](https://agentclientprotocol.com/announcements/transports-working-group.md)).
- Custom transports are explicitly allowed as long as they preserve JSON-RPC framing and bidirectional message flow.
- **Implication for ZRemote:** if we want to expose a remote agent over our existing WebSocket, we are doing it ahead of the official transport spec — same status as the broader ACP community, not a violation.

### 1.3 Concurrency & ordering

- A single ACP connection multiplexes **multiple concurrent sessions** (each identified by `sessionId`).
- JSON-RPC `id` correlates request/response on a single connection.
- Notifications from a session are delivered in order on a given connection, but the spec does not impose ordering guarantees across sessions.

---

## 2. Lifecycle

```
+-------- Client --------+                                  +-------- Agent --------+
|                        |  initialize (req)                |                       |
|                        | -------------------------------> |                       |
|                        |  initialize result               |                       |
|                        | <------------------------------- |                       |
|                        |                                  |                       |
|                        |  authenticate (req, optional)    |                       |
|                        | -------------------------------> |                       |
|                        |  authenticate result             |                       |
|                        | <------------------------------- |                       |
|                        |                                  |                       |
|                        |  session/new (or load/resume)    |                       |
|                        | -------------------------------> |                       |
|                        |  { sessionId }                   |                       |
|                        | <------------------------------- |                       |
|                        |                                  |                       |
|                        |  session/prompt (req)            |                       |
|                        | -------------------------------> |  -- model loop --     |
|                        |                                  |   (tool calls,        |
|                        |  session/update (notif) *N       |    permission asks,   |
|                        | <------------------------------- |    fs / terminal ops) |
|                        |                                  |                       |
|                        |  fs/read|write, terminal/* (req) |                       |
|                        | <------------------------------- |                       |
|                        |  results                         |                       |
|                        | -------------------------------> |                       |
|                        |                                  |                       |
|                        |  session/request_permission (req)|                       |
|                        | <------------------------------- |                       |
|                        |  { selected | cancelled }        |                       |
|                        | -------------------------------> |                       |
|                        |                                  |                       |
|                        |  session/prompt result           |                       |
|                        |  { stopReason }                  |                       |
|                        | <------------------------------- |                       |
|                        |                                  |                       |
|                        |  session/cancel (notif, optional)|                       |
|                        | -------------------------------> |                       |
|                        |  session/close (req, optional)   |                       |
|                        | -------------------------------> |                       |
+------------------------+                                  +-----------------------+
```

### 2.1 `initialize` — version + capability negotiation

| Field | Direction | Type | Notes |
|---|---|---|---|
| `protocolVersion` | both | integer | Single integer MAJOR. Client sends its latest; Agent echoes if supported, otherwise its latest. If versions don't agree, Client SHOULD close. |
| `clientCapabilities.fs.readTextFile` | C→A | bool | Agent may call `fs/read_text_file` only if true. |
| `clientCapabilities.fs.writeTextFile` | C→A | bool | Same gate for writes. |
| `clientCapabilities.terminal` | C→A | bool | Gates `terminal/*` methods. |
| `clientInfo` | C→A | `{name,title?,version}` | Optional metadata. |
| `agentCapabilities.loadSession` | A→C | bool | Gates `session/load`. |
| `agentCapabilities.promptCapabilities.image` | A→C | bool | Gates `image` content blocks in prompt. |
| `agentCapabilities.promptCapabilities.audio` | A→C | bool | Gates `audio`. |
| `agentCapabilities.promptCapabilities.embeddedContext` | A→C | bool | Gates `resource` (embedded). |
| `agentCapabilities.mcpCapabilities.http` | A→C | bool | Agent can connect to MCP over HTTP. |
| `agentCapabilities.mcpCapabilities.sse` | A→C | bool | Agent can connect to MCP over SSE. |
| `agentCapabilities.sessionCapabilities.list` | A→C | bool | Gates `session/list`. |
| `agentCapabilities.sessionCapabilities.resume` | A→C | bool | Gates `session/resume`. |
| `agentCapabilities.sessionCapabilities.close` | A→C | bool | Gates `session/close`. |
| `agentInfo` | A→C | `{name,title?,version}` | Optional. |
| `authMethods` | A→C | `AuthMethod[]` | Available auth flows; empty array means no auth required. |

Source (request/response examples): <https://agentclientprotocol.com/protocol/initialization.md>.

> The spec is strict: **"Clients and Agents MUST treat all capabilities omitted in the initialize request as UNSUPPORTED."** Forgetting a capability silently disables a feature; it is not implicitly enabled.

### 2.2 `authenticate` (optional)

- Used only if `authMethods` is non-empty. Concrete shape is part of the unstable `unstable_auth_methods` feature flag in the schema crate.
- A `logout_method` RFD is in flight (<https://agentclientprotocol.com/rfds/logout-method.md>).

### 2.3 `session/new` — required entry point

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "session/new",
  "params": {
    "cwd": "/abs/path",
    "mcpServers": [
      { "name": "filesystem", "command": "mcp-fs", "args": ["--root","/repo"] }
    ]
  }
}
```

Response: `{ "sessionId": "sess_..." }`.

- `cwd` MUST be absolute.
- `mcpServers` are MCP servers the Agent should connect to for this session. Agents MUST support **stdio** MCP transport; HTTP and SSE depend on `mcpCapabilities`.
- New stable additions (announced 2026): **`session/list`**, **`session/resume`**, **`session/close`** ([announcements](https://agentclientprotocol.com/announcements/session-list-stabilized.md)). Some are still gated by capability flags during the transition.

### 2.4 `session/load` vs `session/resume` vs `session/list` vs `session/close`

| Method | Capability gate | Behaviour |
|---|---|---|
| `session/load` | `agentCapabilities.loadSession` | Agent **replays** prior turns by sending `session/update` notifications with prior user/agent messages, then returns `null`. |
| `session/resume` | `sessionCapabilities.resume` | Restores context **without replay**. Returns empty result (optionally with config state). |
| `session/list` | `sessionCapabilities.list` | Cursor-paginated discovery: optional `cwd`, `cursor`; returns `{sessions: SessionInfo[], nextCursor?}`. |
| `session/close` | `sessionCapabilities.close` | Cancels in-flight work and frees resources. |

Source: <https://agentclientprotocol.com/protocol/session-setup.md>, <https://agentclientprotocol.com/protocol/session-list.md>.

### 2.5 Cancellation & shutdown

- `session/cancel` is a **notification** (no response) — Client → Agent — to abort the current prompt.
- The Agent must:
  - halt model + tool calls promptly,
  - send any final updates,
  - reply to the in-flight `session/prompt` with `stopReason: "cancelled"`.
- The Client must:
  - mark non-finished `tool_call`s as cancelled on its side,
  - respond to any pending `session/request_permission` with `outcome: "cancelled"`.
- There is no global `shutdown` RPC like LSP; closing stdin / dropping the connection is the natural exit signal. A `request_cancellation` RFD ([rfds/request-cancellation.md](https://agentclientprotocol.com/rfds/request-cancellation.md)) is being explored for cancelling individual non-prompt requests.

---

## 3. Message catalog

> Naming convention: methods on the **Agent** (called by Client) are `session/...`, `fs/...` is on the **Client**, `terminal/...` is on the **Client**, `initialize`/`authenticate` are on the Agent. `session/update` and `session/request_permission` are agent-initiated.

### Client → Agent

| Method | Kind | Purpose |
|---|---|---|
| `initialize` | request | Version + capability negotiation. |
| `authenticate` | request | Optional auth flow. |
| `session/new` | request | Create a new session. Returns `sessionId`. |
| `session/load` | request | Resume + replay history (capability gated). |
| `session/resume` | request | Resume without replay (capability gated). |
| `session/list` | request | List known sessions (capability gated). |
| `session/close` | request | End session, free resources (capability gated). |
| `session/prompt` | request | Send a turn; long-running, returns `{stopReason}`. |
| `session/cancel` | notification | Abort the current prompt turn. |
| `session/set_mode` | request | Switch session mode (legacy of `session/set_config_option`). |
| `session/set_config_option` | request | Set a generic config option (model, mode, thought level, …). |

### Agent → Client

| Method | Kind | Purpose |
|---|---|---|
| `session/update` | notification | Streamed updates (see §3.1). |
| `session/request_permission` | request | Ask user to allow/reject a tool call. |
| `fs/read_text_file` | request | Read a text file (also returns unsaved editor buffers). |
| `fs/write_text_file` | request | Write a text file. |
| `terminal/create` | request | Start a command in a Client-managed terminal. Returns `terminalId`. |
| `terminal/output` | request | Snapshot of current output (non-blocking). |
| `terminal/wait_for_exit` | request | Block until the command exits. |
| `terminal/kill` | request | Kill the process; terminal stays valid. |
| `terminal/release` | request | Kill (if needed) and release the terminal id. |

### 3.1 `session/update` payload variants

`session/update` is a notification with `params: { sessionId, update: { sessionUpdate: <variant>, ... } }`. The discriminant is the `sessionUpdate` field (snake_case). Confirmed variants:

| `sessionUpdate` value | Carries | Used for |
|---|---|---|
| `user_message_chunk` | `{content: ContentBlock}` | Replays of user messages on `session/load`. |
| `agent_message_chunk` | `{content: ContentBlock}` | Streaming model output to UI. |
| `agent_thought_chunk` | `{content: ContentBlock}` | Reasoning/thought stream (thinking content). |
| `tool_call` | tool call object (see §4) | Announce a new tool call. |
| `tool_call_update` | partial tool call (id required) | Status / content / location updates. |
| `plan` | `{entries: PlanEntry[]}` | Multi-step execution plan, see §6. |
| `available_commands_update` | `{availableCommands: AvailableCommand[]}` | Slash command catalog updates. |
| `current_mode_update` | `{modeId}` | Agent-initiated mode switch. |
| `config_option_update` | `{configId, ...}` | Agent-initiated config switch (see §7). |

Sources: <https://agentclientprotocol.com/protocol/prompt-turn.md>, <https://agentclientprotocol.com/protocol/slash-commands.md>, <https://agentclientprotocol.com/protocol/session-modes.md>, <https://agentclientprotocol.com/protocol/session-config-options.md>.

### 3.2 `session/prompt` request

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session/prompt",
  "params": {
    "sessionId": "sess_abc",
    "prompt": [
      { "type": "text", "text": "Analyze main.py" },
      { "type": "resource",
        "resource": {
          "uri": "file:///repo/main.py",
          "mimeType": "text/x-python",
          "text": "def f(): pass"
        }
      }
    ]
  }
}
```

Response:

```json
{ "jsonrpc": "2.0", "id": 2, "result": { "stopReason": "end_turn" } }
```

`stopReason ∈ { end_turn, max_tokens, max_turn_requests, refusal, cancelled }`.

---

## 4. Tool calls + permissions

### 4.1 The tool call object

```json
{
  "sessionUpdate": "tool_call",
  "toolCallId": "call_001",
  "title": "Run pytest in repo root",
  "kind": "execute",
  "status": "pending",
  "rawInput":  { "cmd": "pytest", "args": ["-q"] },
  "rawOutput": null,
  "locations": [ { "path": "/repo/tests" } ],
  "content":   []
}
```

- `kind ∈ { read, edit, delete, move, search, execute, think, fetch, switch_mode, other }`.
- `status ∈ { pending, in_progress, completed, failed }` (defaults `pending`).
- `locations`: `{ path, line? }[]` — used by Clients to "follow the agent's eyes" in the editor.
- `content` items can be:
  - **regular content**: `{type: "content", content: ContentBlock}`
  - **diff**: `{type: "diff", path, oldText, newText}`
  - **terminal**: `{type: "terminal", terminalId}` — embeds live terminal output.

### 4.2 `tool_call_update`

Identical shape but only `toolCallId` is required; every other field is a partial patch. This is how the Agent reports progress (e.g. `pending → in_progress → completed`) and adds output as it arrives.

### 4.3 `session/request_permission`

Issued by the Agent when a side-effecting tool call needs user approval. Direction: Agent → Client (request).

```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "method": "session/request_permission",
  "params": {
    "sessionId": "sess_abc",
    "toolCall": { /* full tool call object */ },
    "options": [
      { "optionId": "allow_once",    "name": "Allow",            "kind": "allow_once" },
      { "optionId": "allow_always",  "name": "Allow always",     "kind": "allow_always" },
      { "optionId": "reject_once",   "name": "Reject",           "kind": "reject_once" },
      { "optionId": "reject_always", "name": "Reject always",    "kind": "reject_always" }
    ]
  }
}
```

Client response:

```json
{ "jsonrpc": "2.0", "id": 42,
  "result": {
    "outcome": { "type": "selected", "optionId": "allow_once" }
  }
}
```

Or, on cancellation: `"outcome": { "type": "cancelled" }`.

`PermissionOption.kind ∈ { allow_once, allow_always, reject_once, reject_always }` — these are *hints* to the UI; the Agent honours `allow*` vs `reject*` semantics via the `optionId` returned.

Source: <https://agentclientprotocol.com/protocol/tool-calls.md>.

---

## 5. File-system operations (Agent → Client)

Both methods are gated by `clientCapabilities.fs.readTextFile` / `clientCapabilities.fs.writeTextFile`. If the flag is `false` or absent, the Agent **MUST NOT** call them.

### 5.1 `fs/read_text_file`

```json
{ "jsonrpc": "2.0", "id": 3,
  "method": "fs/read_text_file",
  "params": {
    "sessionId": "sess_abc",
    "path": "/abs/repo/src/main.py",
    "line": 10,
    "limit": 50
  }
}
```

Response: `{ "content": "<file body>" }`.

- `path` must be absolute.
- `line` is 1-based (optional), `limit` (optional) caps the number of lines returned.
- The Client SHOULD return **unsaved editor buffer** content if the file is open and dirty — this is the core reason `fs/read_text_file` exists rather than the Agent reading from disk directly.

### 5.2 `fs/write_text_file`

```json
{ "jsonrpc": "2.0", "id": 4,
  "method": "fs/write_text_file",
  "params": {
    "sessionId": "sess_abc",
    "path": "/abs/repo/config.json",
    "content": "{\n  \"debug\": true\n}\n"
  }
}
```

Response: `null`. The Client creates the file if it doesn't exist.

> **Note:** ACP defines only **text** file ops in the stable surface. Binary files travel through `resource_link` content blocks or MCP servers. There is currently no `fs/list`, `fs/stat`, or `fs/move` in the stable spec.

---

## 6. Terminal operations (Agent → Client)

Gated by `clientCapabilities.terminal`. Designed so the Agent can run shell commands without owning the host environment.

### 6.1 Lifecycle

```
                      terminal/create (returns terminalId)
                              │
        ┌─────────────────────┼──────────────────────┐
        ▼                     ▼                      ▼
  terminal/output     terminal/wait_for_exit    terminal/kill
  (poll snapshot)     (block until exit)        (signal process)
        │                     │                      │
        └─────────────────────┴──────────────────────┘
                              │
                       terminal/release
                       (free terminalId; output may stay rendered in tool call)
```

### 6.2 `terminal/create`

```json
{ "method": "terminal/create",
  "params": {
    "sessionId": "sess_abc",
    "command": "cargo",
    "args": ["test", "--workspace"],
    "env":  [{"name":"RUST_LOG","value":"info"}],
    "cwd":  "/abs/repo",
    "outputByteLimit": 1048576
  }
}
```

Returns `{ terminalId }` immediately, even if the command is still running.

### 6.3 `terminal/output`

Returns the current accumulated output, a `truncated` flag, and (if exited) `exitStatus = { exitCode, signal }`. Non-blocking.

### 6.4 `terminal/wait_for_exit`

Blocks until exit, returns `{ exitCode?, signal? }`.

### 6.5 `terminal/kill`

Sends a kill signal but **keeps the terminal id valid** — Agent can still call `terminal/output` and `terminal/wait_for_exit`. `terminal/release` is still required.

### 6.6 `terminal/release`

Frees the terminalId. After release, future `terminal/*` calls with that id are invalid. Output already embedded in a `tool_call` should remain visible in the UI.

> **Implication for ZRemote:** the existing PTY abstraction in `zremote-agent` is a near-perfect fit for this surface. We already have create/output/kill primitives; what's missing is the byte-limit ring buffer and `terminalId` semantics distinct from our `session_id`.

---

## 7. Sessions: modes & config options

### 7.1 Session modes (legacy, being deprecated)

- Agent advertises `availableModes: Mode[]` and `currentModeId` during session setup or via `current_mode_update` notifications.
- Mode = `{ id, name, description? }`. Reference triple: **Ask** (request permission for changes), **Architect** (design without implementation), **Code** (full tool access).
- Client switches via `session/set_mode { sessionId, modeId }`.

### 7.2 Session config options (current direction)

Generalises modes into a typed list of selectable options.

```jsonc
// Agent advertises:
"configOptions": [
  {
    "id": "model",
    "name": "Model",
    "category": "model",
    "type": "select",
    "currentValue": "sonnet",
    "options": [
      { "value": "opus", "name": "Claude Opus" },
      { "value": "sonnet", "name": "Claude Sonnet" }
    ]
  },
  {
    "id": "mode",
    "name": "Mode",
    "category": "mode",
    "type": "select",
    "currentValue": "code",
    "options": [
      { "value": "ask", "name": "Ask" },
      { "value": "code", "name": "Code" }
    ]
  }
]
```

- Reserved `category` values: `mode`, `model`, `thought_level`. Custom values must start with `_`.
- Client mutates with `session/set_config_option { sessionId, configId, value }`; Agent responds with the **complete** updated config (so cascading dependencies surface).
- Agents SHOULD send both `modes` and `configOptions` during the transition window for backwards compatibility.

Source: <https://agentclientprotocol.com/protocol/session-config-options.md>, [announcements/session-config-options-stabilized.md](https://agentclientprotocol.com/announcements/session-config-options-stabilized.md).

### 7.3 Slash commands

- Advertised via the `available_commands_update` `session/update` notification (Agent → Client).
- Schema per command: `{ name, description, input?: { hint?: string } }`.
- The user invokes by typing `"/web search me"` as a normal `session/prompt` message — the prefix is purely a UI convention; the wire format is unchanged. The protocol does **not** define a separate "execute slash command" RPC.
- Source: <https://agentclientprotocol.com/protocol/slash-commands.md>.

### 7.4 Plans

`session/update` with `sessionUpdate: "plan"` carries an `entries: PlanEntry[]` where each entry is `{ content, priority: high|medium|low, status: pending|in_progress|completed }`. Plans can be replaced fully on each notification — the latest one wins. Source: <https://agentclientprotocol.com/protocol/agent-plan.md>.

---

## 8. Content blocks

Reusable across prompts, message chunks, and tool call content. `type` discriminator:

| `type` | Required fields | Notes / capability |
|---|---|---|
| `text` | `text` | Always supported. |
| `image` | `mimeType`, `data` (base64); `uri?` | `promptCapabilities.image` for prompts. |
| `audio` | `mimeType`, `data` (base64) | `promptCapabilities.audio`. |
| `resource` | `resource: {uri, mimeType?, text\|blob}` | `promptCapabilities.embeddedContext`. The "@-mention" of a file with body inlined. |
| `resource_link` | `uri`, `name`; `mimeType?`, `title?`, `description?`, `size?` | Reference without inline data. |

All variants accept optional `annotations` for display metadata. Source: <https://agentclientprotocol.com/protocol/content.md>.

---

## 9. Versioning, capabilities, extensibility

### 9.1 Versioning

- `protocolVersion` is a **single integer** representing the MAJOR version.
- Client sends its highest supported version. Agent replies with the same (if it supports it) or its own latest.
- Both parties MUST act according to the agreed version. If they cannot agree, **client SHOULD close the connection**. There is no "minor compatibility" — semantics are tied to MAJOR.
- The current protocol version observed in examples is `1`.

### 9.2 Capabilities

- All features beyond `initialize` + `session/new` + `session/prompt` + `session/request_permission` are capability-gated.
- Capabilities omitted from the `initialize` payload are treated as **unsupported**.
- Capabilities are flat-namespaced under `clientCapabilities` and `agentCapabilities` and grouped (`fs.*`, `promptCapabilities.*`, `mcpCapabilities.*`, `sessionCapabilities.*`).

### 9.3 Extensibility (`_meta` and underscore methods)

- Every protocol type MAY carry an `_meta: { [string]: unknown }` field. Implementations MUST NOT add custom fields at the **root** of a spec type — only inside `_meta`.
- Reserved root-level `_meta` keys: `traceparent`, `tracestate`, `baggage` (W3C Trace Context).
- **Custom methods MUST start with `_`** (e.g. `_zed.dev/workspace/buffers`):
  - Custom **requests** that aren't recognised → respond with JSON-RPC error `-32601` (Method not found).
  - Custom **notifications** that aren't recognised → silently ignore.
- Recommended pattern for vendor capabilities: advertise inside `_meta` of the capability object during `initialize`:
  ```json
  "_meta": { "zed.dev": { "workspace": true, "fileNotifications": true } }
  ```

Source: <https://agentclientprotocol.com/protocol/extensibility.md>.

---

## 10. ACP vs MCP vs LSP

| Dimension | **ACP** | **MCP** | **LSP** |
|---|---|---|---|
| Primary axis | Editor ↔ AI agent | Agent ↔ tool/data server | Editor ↔ language server |
| Wire | JSON-RPC 2.0 | JSON-RPC 2.0 | JSON-RPC 2.0 |
| Transport | stdio (newline-delimited); HTTP/WS draft | stdio (newline-delimited), Streamable HTTP, SSE | stdio (`Content-Length` framed), TCP, sockets |
| Who initiates | Client spawns Agent | Client spawns server (or connects to remote) | Editor spawns server |
| Who issues tool calls | Agent decides; Client gates with permission | Agent calls server's `tools/call` | n/a (LSP exposes refactors/quick-fixes, not arbitrary tool calls) |
| File access | `fs/read_text_file`, `fs/write_text_file` (text only) | Via `resources/*` and tool calls | `textDocument/*` (full editor sync) |
| Terminal | `terminal/*` first-class | Only via custom tools | None |
| Permissions | First-class `session/request_permission` | Out-of-band (host UX) | n/a |
| Sessions | Multi-session, list/load/resume/close | Connection ≈ session; resumable streams | Per-document state |
| Streaming updates | `session/update` notification with discriminated variants | `notifications/*` + Streamable HTTP SSE chunks | `textDocument/publishDiagnostics`, etc. |
| Versioning | Single integer MAJOR | Date-versioned strings ("2025-06-18") | Semver-style version strings |
| Extensibility | `_meta` + `_methodName` | `_meta` + experimental capabilities | `experimental` capabilities |
| Relationship | **Composes with MCP**: ACP agents typically host MCP clients to consume tool servers. | Standalone, but MCP servers are commonly mounted *into* ACP agents via `session/new.mcpServers`. | Independent. |

> **Mental model:** LSP is **editor ↔ language**. MCP is **agent ↔ tool**. ACP is **editor ↔ agent**. They are complementary: a Zed-style stack is `editor —ACP→ agent —MCP→ tool servers`, with the language server still riding LSP.

There is also an active RFD for **MCP-over-ACP** (<https://agentclientprotocol.com/rfds/mcp-over-acp.md>) which would let an ACP-only Client expose its MCP tools through the same ACP channel — relevant if ZRemote ever wants to be a "tool host" for downstream agents.

---

## 11. Rust SDK API shape

### 11.1 Crate layout

| Crate | Version (2026-04) | Purpose |
|---|---|---|
| `agent-client-protocol-schema` | **0.12.2** | Pure schema crate: every request/response/notification type, derived from `schema/schema.json`. Edition 2024, Apache-2.0, schemars/serde/strum based. |
| `agent-client-protocol` | **0.11.1** | High-level SDK: roles, connections, JSON-RPC plumbing. Depends on `-schema` and `agent-client-protocol-derive`. |
| `agent-client-protocol-tokio` | recent | Tokio process-spawn helpers. |
| `agent-client-protocol-conductor` | 0.11.1 | Binary for proxy chaining. |
| `agent-client-protocol-rmcp` | recent | rmcp (MCP) integration glue. |
| `agent-client-protocol-test` | recent | Test harness. |
| `agent-client-protocol-cookbook` | recent | Doc/examples crate. |

The Rust SDK lives at <https://github.com/agentclientprotocol/rust-sdk> (a Cargo workspace); the **schema** crate alone lives at <https://github.com/zed-industries/agent-client-protocol> (a duplicate-named upstream that auto-generates types from `schema/schema.json`).

### 11.2 Roles, not a single Agent/Client trait

Unlike older blog-post sketches that show `trait Agent` and `trait Client`, the real 0.11 SDK uses a **role-typestate** pattern:

```rust
pub trait Role: Debug + Clone + Send + Sync + 'static + Eq + Ord + Hash {
    type Counterpart: Role<Counterpart = Self>;
    fn role_id(&self) -> RoleId;
    fn counterpart(&self) -> Self::Counterpart;
    // ... default_handle_dispatch_from etc.
}

pub struct Client;     // Counterpart = Agent
pub struct Agent;      // Counterpart = Client
pub struct Proxy;      // Counterpart = Conductor
pub struct Conductor;  // Counterpart = Proxy
```

You don't *implement Agent or Client*; you **build a connection in a given role** and attach handlers to it via `Builder` and `HandleDispatchFrom<Counterpart>` impls.

Minimal client (from `lib.rs` doc):

```rust
use agent_client_protocol::Client;
use agent_client_protocol::schema::{InitializeRequest, ProtocolVersion};

Client.builder()
    .name("my-client")
    .connect_with(transport, async |cx| {
        cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task().await?;

        cx.build_session_cwd()?
            .block_task()
            .run_until(async |mut session| {
                session.send_prompt("What is 2 + 2?")?;
                let response = session.read_to_string().await?;
                println!("{}", response);
                Ok(())
            })
            .await
    })
    .await
```

Key public surface (from `lib.rs` re-exports):

- `Builder<Role, Handler, Run>` — composes handlers and a runner.
- `Channel`, `ByteStreams`, `Lines` — transport abstractions; `ByteStreams` is what you'll wrap a tokio process around.
- `ConnectionTo<Role>` — the working handle inside `connect_with`. Methods: `send_request_to(peer, req)`, `send_notification_to(peer, notif)`, `add_dynamic_handler(...)`, `build_session_cwd()`, etc.
- `Dispatch` — incoming-message envelope (`Request`, `Notification`, `Response`).
- `HandleDispatchFrom<Peer>` — async trait you implement to attach handlers per peer role.
- `JsonRpcMessage`, `JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcNotification`, `Responder`, `SentRequest`, `ResponseRouter` — JSON-RPC plumbing.

`schema::*` types (key ones, all `serde::Serialize`/`Deserialize` + `schemars`):

- `ProtocolVersion`, `InitializeRequest`, `InitializeResponse`,
  `AgentCapabilities`, `ClientCapabilities`, `PromptCapabilities`, `McpCapabilities`, `SessionCapabilities`.
- `NewSessionRequest`, `NewSessionResponse`, `LoadSessionRequest`, `ResumeSessionRequest`, `ListSessionsRequest`, `ListSessionsResponse`, `CloseSessionRequest`.
- `PromptRequest`, `PromptResponse`, `StopReason`, `CancelNotification`.
- `SessionUpdate` (enum with all `sessionUpdate` variants), `SessionNotification`.
- `ToolCall`, `ToolCallUpdate`, `ToolKind`, `ToolCallStatus`, `ToolCallContent`, `Diff`.
- `RequestPermissionRequest`, `RequestPermissionResponse`, `PermissionOption`, `PermissionOptionKind`.
- `ContentBlock`, `EmbeddedResource`, `ResourceLink`.
- `ReadTextFileRequest/Response`, `WriteTextFileRequest`,
  `CreateTerminalRequest/Response`, `TerminalOutputResponse`, `WaitForTerminalExitResponse`, `KillTerminalRequest`, `ReleaseTerminalRequest`.
- `Plan`, `PlanEntry`, `PlanEntryStatus`, `PlanEntryPriority`.
- `AvailableCommand`, `AvailableCommandInput`.
- `ConfigOption`, `ConfigOptionValue`, `SetConfigOptionRequest`, `ConfigOptionUpdate`.

### 11.3 Feature flags

Gate **unstable** schema variants behind cargo features (default `[]`):

```
unstable = [
  unstable_auth_methods, unstable_boolean_config, unstable_logout,
  unstable_message_id, unstable_session_additional_directories,
  unstable_session_close, unstable_session_fork, unstable_session_model,
  unstable_session_resume, unstable_session_usage,
]
```

This is critical: many "stabilized" announcements (session/list, session/close, session/resume) are still gated **at the type level** until the next minor bump. ZRemote should pin specifically (`agent-client-protocol = "=0.11.1"`) and decide explicitly which `unstable_*` features to opt into.

### 11.4 Async runtime

- Tokio, mandatory (`tokio` is a workspace dependency, not optional).
- The SDK uses `async fn in trait` (Rust 2024 edition) — pulls `edition.workspace = true` from the workspace.
- **MSRV** is not stated explicitly in `Cargo.toml`; Rust 2024 edition + `async fn` in traits implies **Rust 1.85+**.

### 11.5 TypeScript SDK

- Package: `@agentclientprotocol/sdk` on npm.
- Key classes: `AgentSideConnection`, `ClientSideConnection`.
- Production reference: Gemini CLI uses it as an Agent.
- API docs: <https://agentclientprotocol.github.io/typescript-sdk>.

### 11.6 Languages

Official SDKs: **Rust, TypeScript, Python, Kotlin, Java**. 40+ agents implement ACP, including **Claude Code** (via Zed adapter), **GitHub Copilot** (preview), **Gemini CLI**, **Cursor**, **Cline**, **OpenHands**, **Goose**, **JetBrains Junie**, **Codex CLI**, etc. Source: <https://agentclientprotocol.com/get-started/agents.md>.

---

## 12. Open questions / ambiguities

1. **HTTP/WebSocket transport.** No production spec yet. We will be improvising. The Transports Working Group exists ([announcement](https://agentclientprotocol.com/announcements/transports-working-group.md)) but no document we can pin to.
2. **Authentication.** `authMethods` is in the stable response shape, but the concrete `authenticate` payloads are gated behind `unstable_auth_methods`. Don't design around them yet.
3. **Binary file ops.** No `fs/read_binary_file` in the stable spec — only text. Binaries flow via `resource_link` URIs or MCP tool calls.
4. **`SDK trait Agent`/`trait Client` references** in third-party blog posts (and in <https://agentclientprotocol.com/libraries/rust>) are **outdated**. The current SDK uses the role-typestate pattern. The doc page is misleading and we should confirm against `docs.rs/agent-client-protocol/0.11.1` before writing examples.
5. **Crate naming collision.** Two crates exist: `agent-client-protocol-schema` (types only) and `agent-client-protocol` (full SDK). Both are advertised as "the" Rust crate. ZRemote should depend on the high-level one unless we want types only.
6. **Two GitHub repos with same name.** `zed-industries/agent-client-protocol` and `agentclientprotocol/agent-client-protocol` both exist. The second is the post-handover canonical home (Sergey Ignatov took over as lead maintainer per [announcements/sergey-ignatov-lead-maintainer.md](https://agentclientprotocol.com/announcements/sergey-ignatov-lead-maintainer.md)). The Rust SDK is in a third repo: `agentclientprotocol/rust-sdk`.
7. **Streaming token semantics.** Spec doesn't mandate chunk granularity for `agent_message_chunk`; agents may stream per-token, per-line, or per-full-message. Clients must tolerate any.
8. **Cancellation of non-prompt requests.** Today, `session/cancel` cancels the entire turn. Cancelling a single in-flight `terminal/wait_for_exit` is the subject of an open RFD (<https://agentclientprotocol.com/rfds/request-cancellation.md>).
9. **`session/load` replay format.** Spec says the Agent emits prior `user_message_chunk` and `agent_message_chunk` updates, but does not specify whether prior tool calls are also replayed. Implementations differ.
10. **Session-mode vs config-option migration window.** Spec asks agents to emit both during transition, but does not say when `session/set_mode` will be removed.

---

## 13. Sources

- Official spec
  - Site index: <https://agentclientprotocol.com/llms.txt>
  - Overview: <https://agentclientprotocol.com/protocol/overview>
  - Architecture: <https://agentclientprotocol.com/get-started/architecture.md>
  - Initialization: <https://agentclientprotocol.com/protocol/initialization.md>
  - Session setup: <https://agentclientprotocol.com/protocol/session-setup.md>
  - Session list: <https://agentclientprotocol.com/protocol/session-list.md>
  - Session modes: <https://agentclientprotocol.com/protocol/session-modes.md>
  - Session config options: <https://agentclientprotocol.com/protocol/session-config-options.md>
  - Prompt turn: <https://agentclientprotocol.com/protocol/prompt-turn.md>
  - Tool calls: <https://agentclientprotocol.com/protocol/tool-calls.md>
  - File system: <https://agentclientprotocol.com/protocol/file-system.md>
  - Terminals: <https://agentclientprotocol.com/protocol/terminals.md>
  - Content blocks: <https://agentclientprotocol.com/protocol/content.md>
  - Plans: <https://agentclientprotocol.com/protocol/agent-plan.md>
  - Slash commands: <https://agentclientprotocol.com/protocol/slash-commands.md>
  - Transports: <https://agentclientprotocol.com/protocol/transports.md>
  - Extensibility: <https://agentclientprotocol.com/protocol/extensibility.md>
  - Schema: <https://agentclientprotocol.com/protocol/schema.md>
  - Agents directory: <https://agentclientprotocol.com/get-started/agents.md>
- Reference implementations
  - Schema repo (Zed): <https://github.com/zed-industries/agent-client-protocol>
  - Schema crate Cargo.toml: `name = "agent-client-protocol-schema", version = "0.12.2", edition = "2024"`
  - SDK repo: <https://github.com/agentclientprotocol/rust-sdk>
  - SDK Cargo.toml: `name = "agent-client-protocol", version = "0.11.1"`, deps include `tokio`, `rmcp`, `serde`, `schemars`, `jsonrpcmsg`.
  - SDK `src/role/acp.rs` — shows `Client`, `Agent`, `Proxy`, `Conductor` role structs.
  - SDK `lib.rs` — top-level docs and re-exports.
- Crates.io / lib.rs
  - <https://crates.io/crates/agent-client-protocol>
  - <https://lib.rs/crates/agent-client-protocol>
- Announcements
  - Lead maintainer handover: <https://agentclientprotocol.com/announcements/sergey-ignatov-lead-maintainer.md>
  - Transports WG: <https://agentclientprotocol.com/announcements/transports-working-group.md>
  - Session list / resume / close stabilizations: <https://agentclientprotocol.com/announcements/session-list-stabilized.md>, <https://agentclientprotocol.com/announcements/session-resume-stabilized.md>, <https://agentclientprotocol.com/announcements/session-close-stabilized.md>
  - Session config options: <https://agentclientprotocol.com/announcements/session-config-options-stabilized.md>
- Open RFDs cited
  - Streamable HTTP/WS: <https://agentclientprotocol.com/rfds/streamable-http-websocket-transport.md>
  - Request cancellation: <https://agentclientprotocol.com/rfds/request-cancellation.md>
  - MCP-over-ACP: <https://agentclientprotocol.com/rfds/mcp-over-acp.md>
  - Logout: <https://agentclientprotocol.com/rfds/logout-method.md>
  - Auth methods: <https://agentclientprotocol.com/rfds/auth-methods.md>
