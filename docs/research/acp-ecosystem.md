# ACP Ecosystem Survey

Researcher: `acp-ecosystem` (team `acp-research`, task #2)
Date: 2026-04-25

## TL;DR

- ACP (Agent Client Protocol) is Zed's open JSON-RPC-over-stdio protocol for IDE↔coding-agent communication. Spec hosted at `agentclientprotocol.com`, code at `github.com/agentclientprotocol/agent-client-protocol` (originally `zed-industries/agent-client-protocol`, since moved to its own org under Apache 2.0).
- Latest released versions as of 2026-04-25: Rust crate `agent-client-protocol` **0.11.1** (2026-04-21), npm `@agentclientprotocol/sdk` **0.20.0** (2026-04-23), PyPI `agent-client-protocol` **0.9.0**, Kotlin/Java SDKs official. Repo release **v0.12.2** (2026-04-23).
- Clients shipping today: Zed, JetBrains IDEs (IntelliJ/PyCharm/WebStorm, all platforms), Neovim (CodeCompanion, agentic.nvim, avante.nvim), Emacs (agent-shell), marimo notebook; in development: Eclipse, Toad, plus AionUi, Sidequery, DeepChat, Tidewave, Obsidian.
- Agent registry launched **2026-01-28** at `cdn.agentclientprotocol.com/registry/v1/latest/registry.json` — one-click install across clients; registry currently lists **27 agents** including Claude Agent, Codex CLI, Gemini CLI, GitHub Copilot CLI, Goose, Cline, Cursor, OpenCode, Auggie, Kimi, Qwen, Mistral Vibe, Junie, Pi, Goose, Amp.
- Transport today is **stdio + JSON-RPC 2.0**; Streamable HTTP is in draft. Client launches the agent as a subprocess; lines are newline-delimited UTF-8 JSON; stderr is free for agent logs.
- Lifecycle: `initialize` → `session/new` (or `session/load`/`session/resume`) → `session/prompt` (loop with `session/update` notifications and optional `session/request_permission` / `session/cancel`) → response with `stopReason`.
- Client-exposed capabilities the agent can call: `fs/read_text_file`, `fs/write_text_file`, plus the full `terminal/*` family (`create`, `output`, `wait_for_exit`, `kill`, `release`). All gated by capabilities advertised at `initialize`.
- Speaking ACP gets you for free: rich diff review UI, terminal embedding inside tool cards, plan tracking, streaming text/tool-call updates, permission prompts, MCP server fan-out, and any-editor reach.
- Spec vs Zed: the protocol is editor-agnostic and only *defines schema*; the multi-buffer review, hunk-level accept/reject, agent panel chrome, login flows, and slash-command UI are **Zed implementation choices**, not ACP requirements.

## Clients (editor side)

Sources: [zed.dev/acp](https://zed.dev/acp), [Zed external-agents docs](https://zed.dev/docs/ai/external-agents), [JetBrains AI ACP docs](https://www.jetbrains.com/help/ai-assistant/acp.html), [CodeCompanion ACP page](https://codecompanion.olimorris.dev/agent-client-protocol), [acp-progress-report](https://zed.dev/blog/acp-progress-report).

| Client | Status | Notes |
|---|---|---|
| **Zed** | Stable; reference impl | Native ACP client (Rust). First-class agent panel: thread list, multi-buffer diff review (hunk-level accept/reject, `shift-ctrl-r`), task-list sidebar, `@-mention` files/diagnostics/symbols, slash commands, real-time agent following. External agents installed via ACP Registry. |
| **JetBrains IDEs** (IntelliJ, PyCharm, WebStorm, etc.) | Stable | "AI Assistant supports the Agent Client Protocol (ACP), allowing you to connect external AI agents and use them in the AI Chat." Cursor agent shipped to JetBrains via ACP on 2026-03-04. |
| **Neovim — CodeCompanion** | Stable; v17.18.0+ | Implements ACP v1; supports session/list + session/load (`/resume`), dynamic slash commands (`\command`), permission prompts, tool calls. |
| **Neovim — agentic.nvim** | Stable | Standalone ACP-only Neovim client supporting Claude Code, Gemini, Codex, OpenCode, Cursor-agent. |
| **Neovim — avante.nvim** | Stable | Listed in progress report as adopted. |
| **Emacs — agent-shell** | Stable | Mentioned in Zed's progress report. |
| **marimo** | Stable | Python notebook with ACP client. |
| **Eclipse** | In development | Mentioned in Zed progress report. |
| **Toad** | In development | Terminal-based ACP client (mentioned in Zed progress report). |
| **AionUi, Sidequery, DeepChat, Tidewave, Obsidian, aizen, Web Browser (AI SDK)** | Listed | Per [zed.dev/acp](https://zed.dev/acp) editors panel — status not confirmed individually. |

## Agents (agent side)

Sources: live ACP registry at `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json` (27 entries), [Zed ACP page](https://zed.dev/acp), [acp-progress-report](https://zed.dev/blog/acp-progress-report).

Integration shape:
- **Native** = the agent project itself speaks ACP (often behind a `--acp` flag).
- **Adapter** = first-party adapter wraps the agent's SDK and translates to ACP.
- **Wrapper** = third-party adapter wraps a non-ACP CLI.

| Agent | Status | Repo / Distribution | Shape |
|---|---|---|---|
| **Gemini CLI** | Stable; reference impl | [google-gemini/gemini-cli](https://github.com/google-gemini/gemini-cli), launch with `gemini --acp` | Native (`--acp` flag in CLI; uses ACP "proxied file system" so reads/writes go through client) |
| **Claude Agent** (Claude Code SDK) | Stable beta | [zed-industries/claude-code-acp](https://github.com/zed-industries/claude-code-acp), npm `@zed-industries/claude-code-acp` v0.16.2 (2026-03-26); also new `@zed-industries/claude-agent-acp` | Adapter (wraps Claude Code SDK; Apache-2.0; vendored Claude Code CLI) |
| **Codex CLI** (OpenAI) | Stable | `codex-acp` (auto-installed by Zed v0.208+); [openai/codex](https://github.com/openai/codex) | Adapter |
| **GitHub Copilot CLI** | Stable | Registry: `copilot-cli` | Native via Copilot CLI |
| **Cursor** | Stable | Registry entry; shipped to JetBrains 2026-03-04 | Adapter from Cursor team |
| **Cline** | Stable | Registry entry | Native |
| **Goose** (Block/Square) | Stable | [block/goose](https://github.com/block/goose); Apache-2.0; AAIF inaugural project | Native (Goose works as ACP server **and** can use other ACP agents as providers) |
| **OpenCode** | Stable | [sst/opencode](https://github.com/sst/opencode) | Native |
| **Auggie CLI** (Augment Code) | Stable | npm `@augmentcode/auggie@0.24.0` with `--acp` | Native |
| **Kimi CLI** (Moonshot) | Stable | Registry entry | Native |
| **Qwen Code** (Alibaba) | Stable | Registry entry | Native |
| **Mistral Vibe** | Stable | Registry entry | Native |
| **JetBrains Junie** | Stable | Registry entry | Native (JetBrains-built) |
| **Amp** | Stable | [tao12345666333/amp-acp](https://github.com/tao12345666333/amp-acp) v0.7.0 | Wrapper (community ACP wrapper for Amp) |
| **Autohand Code, Codebuddy Code, Corust, crow-cli, DeepAgents, Factory Droid, fast-agent, Kiro CLI, Kilo, Minion Code, Nova, Pi, Qoder CLI, Stakpak, OpenHands, Docker cagent, AgentPool, Blackbox AI, Code Assistant, VT Code** | Stable (registry/listed) | See registry entries | Mix of native and wrappers |
| **Aider** | In development | Mentioned by progress report as planned | TBD |

The registry JSON also has an `extensions` key, suggesting non-agent ACP extensions are tracked too (not enumerated here).

Each registry entry has fields: `id`, `name`, `version`, `description`, `repository`, `website` (optional), `authors`, `license`, `icon`, and a `distribution` block. Distribution can be either `binary.<platform>.{archive, cmd}` (signed tarballs per arch) or `npx.{package, args, env}` (Node-based agents). This is what makes one-click install work across editors.

## SDK matrix

| Language | Package | Latest | Repo | Examples |
|---|---|---|---|---|
| **Rust** | `agent-client-protocol` (crates.io) | 0.11.1 (2026-04-21) | [agentclientprotocol/rust-sdk](https://github.com/agentclientprotocol/rust-sdk) | `examples/simple_agent.rs`, `examples/yolo_one_shot_client.rs` |
| **TypeScript** | `@agentclientprotocol/sdk` (npm) | 0.20.0 (2026-04-23) | [agentclientprotocol/typescript-sdk](https://github.com/agentclientprotocol/typescript-sdk) | `src/examples/agent.ts`, `src/examples/client.ts` |
| **TypeScript (legacy)** | `@zed-industries/agent-client-protocol` (npm) | 0.4.5 (2025-10-10, archived) | Older Zed-namespaced package, superseded by `@agentclientprotocol/sdk` |  |
| **Python** | `agent-client-protocol` (PyPI) | 0.9.0 | [agentclientprotocol/python-sdk](https://github.com/agentclientprotocol/python-sdk) | `examples/agent.py`, `client.py`, `duet.py`, `echo_agent.py`, `gemini.py` |
| **Kotlin / JVM** | `acp-kotlin` | (per repo) | [agentclientprotocol/kotlin-sdk](https://github.com/agentclientprotocol/kotlin-sdk) | `samples/kotlin-acp-client-sample/` |
| **Java** | `java-sdk` | (per repo) | [agentclientprotocol/java-sdk](https://github.com/agentclientprotocol/java-sdk) | `examples/` |
| **Go** | not found | — | — | — |

Cumulative crates.io downloads: 1.4M for the Rust crate (it is pulled in by Zed and the Claude Code adapter, among others).

## Protocol architecture

Source: [protocol docs (mdx)](https://github.com/agentclientprotocol/agent-client-protocol/tree/main/docs/protocol) — covers `initialization`, `session-setup`, `prompt-turn`, `tool-calls`, `agent-plan`, `file-system`, `terminals`, `transports`, `content`, `error`, `extensibility`, `slash-commands`, `session-modes`, `session-list`, `session-config-options`, `schema`.

### Transport
- **stdio** is the only fully-spec'd transport today: client launches the agent as a subprocess, JSON-RPC frames are newline-delimited UTF-8 on stdin/stdout, agent may use stderr for free-form logs. Client must not write anything else to agent's stdin; agent must not write anything else to stdout.
- **Streamable HTTP** is a draft.
- Custom transports allowed as long as JSON-RPC framing is preserved.

### Lifecycle (each step is JSON-RPC unless noted)
1. `initialize` (request) — exchanges `protocolVersion`, `clientCapabilities` (`fs.readTextFile`, `fs.writeTextFile`, `terminal`), `agentCapabilities` (`loadSession`, `promptCapabilities.{image,audio,embeddedContext}`, `mcpCapabilities.{http,sse}`), `clientInfo` / `agentInfo`, `authMethods`. Capabilities omitted = unsupported. Backward-compatible additions go via capabilities, not version bumps.
2. `authenticate` (request, optional) — runs the chosen auth method.
3. `session/new` (request) — `cwd` + list of `mcpServers` (stdio/HTTP/SSE) → returns `sessionId`. Agent connects to the listed MCP servers as part of session boot.
4. `session/load` (request, optional, if `loadSession`) — replays history as `session/update` notifications then resolves.
5. `session/resume` (request, optional, if `sessionCapabilities.resume`) — reconnect without replay.
6. `session/prompt` (request) — `prompt: ContentBlock[]` (text, image, audio, resource, resource_link). Agent streams `session/update` notifications during the turn.
7. `session/update` (notification, agent → client) — `update.sessionUpdate` ∈ `plan` | `agent_message_chunk` | `user_message_chunk` (during replay) | `tool_call` | `tool_call_update`.
8. `session/request_permission` (request, agent → client) — agent asks before destructive actions; client returns `outcome: Selected{option_id}` or `Cancelled`.
9. `session/cancel` (notification, client → agent) — abort current turn; agent must respond to the prompt request with `stopReason: "cancelled"`.
10. Prompt response — `stopReason` (`end_turn`, `cancelled`, etc.).

### Agent → Client (gated by client capabilities)
- `fs/read_text_file` `{sessionId, path, line?, limit?}` → text content. All paths are absolute. 1-based line numbers.
- `fs/write_text_file` `{sessionId, path, content}` (full content overwrite — no diff format on the wire; diffs are rendered from `oldText`/`newText` in tool-call notifications).
- `terminal/create` `{sessionId, command, args, env, cwd, outputByteLimit}` → `terminalId`. Returns immediately — command runs in background.
- `terminal/output` → `{output, truncated, exitStatus?}`.
- `terminal/wait_for_exit` → `{exitCode?, signal?}`.
- `terminal/kill` — terminate process, terminal stays valid for output retrieval.
- `terminal/release` — required cleanup once agent is done.
- Terminals can be **embedded into tool calls** by adding `{type: "terminal", terminalId}` into the tool-call `content`; the client renders live output and continues to display it after release.

### Tool-call schema (the heart of agent UX)
Each `tool_call` notification has:
- `toolCallId`, `title`, `kind` (`read | edit | delete | move | search | execute | think | fetch | other`),
- `status` (`pending | in_progress | completed | failed`),
- `content[]` — `{type: content, ...}` blocks **or** `{type: diff, path, oldText, newText}` blocks **or** `{type: terminal, terminalId}` blocks,
- `locations[]` (`{path, line?}`) — files touched, used by Zed for "agent following",
- `rawInput`, `rawOutput`.

Updates use the same payload via `tool_call_update` (only changed fields needed). This is what powers Zed's hunk-level diff review and live terminal cards in tool cards.

### Plan schema
- `update.sessionUpdate = "plan"` with full `entries[]` each turn.
- Entry: `{content, priority: high|medium|low, status: pending|in_progress|completed}`.
- Agent **MUST** send the complete plan every update (replace, not patch).

### Permission request schema
- `session/request_permission` (request from agent to client). Request includes the proposed `tool_call` and `options[]` like `{option_id, name, kind: "allow_once"|"allow_always"|"reject_once"|"reject_always"}`. Response: `{outcome: {Selected: {option_id}}}` or `{outcome: "Cancelled"}`.
- (Permission docs page returned 404 at the time of fetch; schema confirmed via `yolo_one_shot_client.rs` which auto-selects `request.options.first().option_id`.)

## Minimal Rust ACP **agent**

From [agentclientprotocol/rust-sdk: src/agent-client-protocol/examples/simple_agent.rs](https://github.com/agentclientprotocol/rust-sdk/blob/main/src/agent-client-protocol/examples/simple_agent.rs):

```rust
use agent_client_protocol::schema::{AgentCapabilities, InitializeRequest, InitializeResponse};
use agent_client_protocol::{Agent, Client, ConnectionTo, Dispatch, Result};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[tokio::main]
async fn main() -> Result<()> {
    Agent
        .builder()
        .name("my-agent")
        .on_receive_request(
            async move |initialize: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(initialize.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_dispatch(
            async move |message: Dispatch, cx: ConnectionTo<Client>| {
                message.respond_with_error(
                    agent_client_protocol::util::internal_error("TODO"), cx)
            },
            agent_client_protocol::on_receive_dispatch!(),
        )
        .connect_to(agent_client_protocol::ByteStreams::new(
            tokio::io::stdout().compat_write(),
            tokio::io::stdin().compat(),
        ))
        .await
}
```

Highlights: builder API, `ByteStreams` over stdio, capabilities returned in `initialize`, all other messages handled by an `on_receive_dispatch` fallback. Real agents replace the fallback with handlers for `session/new`, `session/prompt`, etc.

## Minimal Rust ACP **client**

From [agentclientprotocol/rust-sdk: src/agent-client-protocol/examples/yolo_one_shot_client.rs](https://github.com/agentclientprotocol/rust-sdk/blob/main/src/agent-client-protocol/examples/yolo_one_shot_client.rs) (abridged — full file is ~5.8 KB):

```rust
// Spawn the agent subprocess
let mut cmd = tokio::process::Command::new(&command);
cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped());
let mut child = cmd.spawn()?;

let transport = agent_client_protocol::ByteStreams::new(
    child.stdin.take().unwrap().compat_write(),
    child.stdout.take().unwrap().compat(),
);

Client.builder()
    .on_receive_notification(
        async move |notification: SessionNotification, _cx| {
            println!("{:?}", notification.update); // plan / tool_call / message_chunk
            Ok(())
        },
        agent_client_protocol::on_receive_notification!(),
    )
    .on_receive_request(
        async move |request: RequestPermissionRequest, responder, _conn| {
            // YOLO: auto-approve first option
            let id = request.options.first().map(|o| o.option_id.clone());
            responder.respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id.unwrap())),
            ))
        },
        agent_client_protocol::on_receive_request!(),
    )
    .connect_with(transport, |conn: ConnectionTo<Agent>| async move {
        conn.send_request(InitializeRequest::new(ProtocolVersion::V1)).block_task().await?;
        let s = conn.send_request(NewSessionRequest::new(std::env::current_dir()?)).block_task().await?;
        let resp = conn.send_request(PromptRequest::new(
            s.session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(cli.prompt))],
        )).block_task().await?;
        eprintln!("Stop reason: {:?}", resp.stop_reason);
        Ok(())
    })
    .await?;
```

Highlights: spawn agent → wrap stdio in `ByteStreams` → register notification handler (text/tool_call/plan rendering) and permission handler → drive `initialize`/`session/new`/`session/prompt` and read `stopReason`.

## UX patterns observed in Zed

Sources: [agent panel docs](https://zed.dev/docs/ai/agent-panel), [external-agents docs](https://zed.dev/docs/ai/external-agents), [claude-code-via-acp blog post](https://zed.dev/blog/claude-code-via-acp), [bring-your-own-agent blog](https://zed.dev/blog/bring-your-own-agent-to-zed). No screenshot URLs were directly returned, but the page `https://zed.dev/acp` shows an image labeled "Multiple external agents in Zed made available by ACP".

| Pattern | How Zed renders it from ACP messages |
|---|---|
| **Thread management** | Agent panel sidebar shows running threads. New thread per `session/new`. `+` button picks agent type (Gemini / Claude / Codex / external). |
| **Streaming chat** | `agent_message_chunk` content is appended live. Zed supports markdown, code blocks, and citations from text content blocks. |
| **Plan / task list** | `update.sessionUpdate = "plan"` renders as a checkable task list in the sidebar with priority colours and live `pending → in_progress → completed` transitions. |
| **Tool call cards** | Each `tool_call` is a collapsible card with the kind icon, title, and content. `kind` decides icon (e.g. `edit`, `read`, `execute`). Status pill animates while `in_progress`. |
| **Diff preview** | `content[].type = "diff"` → multi-buffer diff view with **per-hunk accept/reject** and full syntax highlighting + LSP. Setting `expand_edit_card` controls inline-vs-collapsed display. `Review Changes` (shift-ctrl-r) opens a single multi-buffer with the full set of pending edits. |
| **Terminal embedding** | `content[].type = "terminal"` with a `terminalId` → live tail of `terminal/output` inside the tool card; output keeps rendering after `terminal/release`. |
| **Permission prompts** | `session/request_permission` becomes an inline prompt under the tool card with the agent's options ("Allow once", "Allow always for this tool", "Reject"). User selection becomes the `outcome.Selected.option_id`. |
| **`@-mentions`** | Editor side: client expands `@file`, `@diagnostics`, `@symbol` etc. into `ContentBlock::Resource` / `ContentBlock::ResourceLink` blocks before sending the prompt — agent gets typed context, not raw text. |
| **Slash commands** | Agents declare slash commands; client autocompletes and translates them before sending the prompt. |
| **Auth** | Adapter handles login (OAuth, API key) before turning into ACP; Zed renders an inline "Sign in" button when the agent surfaces it via `authMethods`. |
| **Agent following** | When a tool call sets `locations[]`, Zed jumps the editor to that file/line as the tool runs — gives the user real-time "where the agent is looking now". |
| **Multiple agents in one client** | Each session is independent; Zed lets users keep parallel threads with different agents. Permission/profile settings only apply to Zed's first-party agent — external agents get runtime prompts only. |

Documented gaps for external agents in Zed: editing past messages, resuming threads from history, checkpointing, profile-level tool permissions, token usage display.

## What you get for free by speaking ACP

If a client implements ACP, every registry agent ships these capabilities to the client without per-agent code:

1. **Streaming chat with rich content blocks** — text, image, audio, resource (embedded), resource_link.
2. **Plan tracking UI** — pending/in-progress/completed task list with priorities.
3. **Tool-call lifecycle** — pending → in_progress → completed/failed with structured `kind`-based icons.
4. **Diff preview** — agent emits `{oldText, newText, path}`; client renders with whatever editor it already has.
5. **Permission gating** — single in-protocol pattern for "ask before run".
6. **Terminal embedding** — agent runs commands through the client's terminal infra; output auto-streams into the tool card.
7. **MCP fan-out** — `session/new` accepts MCP servers; agent connects on session boot, so MCP tools the user already configured work for any ACP agent.
8. **Slash commands & session resume** — uniform API; client gets per-agent commands and history support without ad-hoc plumbing.
9. **Subprocess isolation** — agent crashes don't crash the client; updates flow purely over stdio JSON.
10. **One-click distribution** — joining the registry means becoming installable in Zed, JetBrains IDEs, and every other registry-aware client without per-IDE work.

## What you'd have to build (gaps a client must fill)

- **Subprocess lifecycle**: spawning, environment, cwd, signals, restart, log capture from stderr.
- **stdio framing**: newline-delimited JSON-RPC parser/writer (or use the SDK).
- **Capability advertising**: deciding which `clientCapabilities` you'll honor (`fs/*`, `terminal`) and implementing the corresponding handlers safely.
- **File-system handlers**: serving `fs/read_text_file` / `fs/write_text_file` calls — typically against the project workspace, with an undo/checkpoint layer (ACP itself defines no transactions).
- **Terminal infra**: PTY, output buffering with byte limits, cancellation, embedding into UI cards.
- **Diff / multi-buffer review UI**: ACP gives you `(path, oldText, newText)`; the multi-buffer hunk-level reviewer is a UI you build (Zed has an unusually good one).
- **Permission UX**: rendering `session/request_permission` and routing the outcome.
- **Agent picker / registry**: fetch `cdn.agentclientprotocol.com/registry/v1/latest/registry.json`, install via `npx` or signed binary, configure auth.
- **Session persistence**: ACP supports `loadSession`/`resume` but the client decides how it stores `sessionId`s and whether it replays.
- **Auth**: plain `authenticate` request; the actual login UX (OAuth flow, API key entry) is not in scope.
- **MCP server list**: collecting MCP server configs from user prefs and forwarding them in `session/new`.
- **Network/transport beyond stdio**: if you want remote agents, you implement Streamable HTTP yourself (still draft) or a custom transport.

## Notes specific to zremote

- **Remote agent execution**: ACP today is stdio-only; an agent process running on a remote host can't directly speak ACP to a local Zed without a tunnel. zremote's existing agent ↔ server WebSocket already provides exactly that bidirectional channel — wrapping ACP frames inside it would let the GUI act as an ACP client for any agent process running on any zremote host.
- **Terminal mapping**: zremote already has a PTY/terminal subsystem with byte-limited buffers — that aligns 1:1 with `terminal/create`/`terminal/output`. The agent host could implement `terminal/*` as thin shims over zremote's existing PTY service.
- **Filesystem mapping**: zremote runs the agent on the remote host, so `fs/read_text_file` would naturally hit the **remote** filesystem. That's exactly what users want when the agent is running where the project lives — no need for sshfs.
- **Agent picker**: pulling the registry JSON gives zremote a free catalog of installable coding agents per host, with binary or `npx` distribution metadata already present.
- **GUI parity**: GPUI desktop app would need to grow tool-call cards, plan list, multi-buffer diff review, terminal embedding, and permission prompts to match Zed's UX — these are non-trivial UI builds but reusable across all ACP agents.

## Sources

- [agentclientprotocol.com](https://agentclientprotocol.com)
- [github.com/agentclientprotocol/agent-client-protocol](https://github.com/agentclientprotocol/agent-client-protocol) (formerly [zed-industries/agent-client-protocol](https://github.com/zed-industries/agent-client-protocol))
- Protocol docs (mdx): [initialization](https://github.com/agentclientprotocol/agent-client-protocol/blob/main/docs/protocol/initialization.mdx) · [session-setup](https://github.com/agentclientprotocol/agent-client-protocol/blob/main/docs/protocol/session-setup.mdx) · [prompt-turn](https://github.com/agentclientprotocol/agent-client-protocol/blob/main/docs/protocol/prompt-turn.mdx) · [tool-calls](https://agentclientprotocol.com/protocol/tool-calls) · [agent-plan](https://agentclientprotocol.com/protocol/agent-plan) · [file-system](https://agentclientprotocol.com/protocol/file-system) · [terminals](https://github.com/agentclientprotocol/agent-client-protocol/blob/main/docs/protocol/terminals.mdx) · [transports](https://github.com/agentclientprotocol/agent-client-protocol/blob/main/docs/protocol/transports.mdx)
- SDKs: [Rust](https://github.com/agentclientprotocol/rust-sdk) · [TypeScript](https://github.com/agentclientprotocol/typescript-sdk) · [Python](https://github.com/agentclientprotocol/python-sdk) · [Kotlin](https://github.com/agentclientprotocol/kotlin-sdk) · [Java](https://github.com/agentclientprotocol/java-sdk)
- Examples: [Rust simple_agent.rs](https://github.com/agentclientprotocol/rust-sdk/blob/main/src/agent-client-protocol/examples/simple_agent.rs) · [Rust yolo_one_shot_client.rs](https://github.com/agentclientprotocol/rust-sdk/blob/main/src/agent-client-protocol/examples/yolo_one_shot_client.rs) · [Python echo_agent.py](https://github.com/agentclientprotocol/python-sdk/blob/main/examples/echo_agent.py) · [TS agent.ts](https://github.com/agentclientprotocol/typescript-sdk/blob/main/src/examples/agent.ts)
- Zed: [zed.dev/acp](https://zed.dev/acp) · [docs/ai/external-agents](https://zed.dev/docs/ai/external-agents) · [docs/ai/agent-panel](https://zed.dev/docs/ai/agent-panel) · [blog/acp-registry](https://zed.dev/blog/acp-registry) · [blog/bring-your-own-agent-to-zed](https://zed.dev/blog/bring-your-own-agent-to-zed) · [blog/claude-code-via-acp](https://zed.dev/blog/claude-code-via-acp) · [blog/acp-progress-report](https://zed.dev/blog/acp-progress-report) · [blog/jetbrains-on-acp](https://zed.dev/blog/jetbrains-on-acp)
- JetBrains: [jetbrains.com/acp](https://www.jetbrains.com/acp/) · [help/ai-assistant/acp](https://www.jetbrains.com/help/ai-assistant/acp.html) · [blog/acp-agent-registry](https://blog.jetbrains.com/ai/2026/01/acp-agent-registry/) · [blog/koog-x-acp](https://blog.jetbrains.com/ai/2026/02/koog-x-acp-connect-an-agent-to-your-ide-and-more/)
- Agents: [Claude adapter @zed-industries/claude-code-acp](https://www.npmjs.com/package/@zed-industries/claude-code-acp) · [Claude adapter repo](https://github.com/zed-industries/claude-agent-acp) · [community Xuanwo/acp-claude-code](https://github.com/Xuanwo/acp-claude-code) · [Gemini CLI ACP mode](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md) · [OpenCode ACP](https://opencode.ai/docs/acp/) · [Goose](https://github.com/block/goose)
- Editor clients: [CodeCompanion ACP](https://codecompanion.olimorris.dev/agent-client-protocol) · [agentic.nvim](https://github.com/carlos-algms/agentic.nvim) · [Kiro ACP CLI](https://kiro.dev/docs/cli/acp/)
- Registry JSON: `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`
- Crates.io API: [agent-client-protocol](https://crates.io/api/v1/crates/agent-client-protocol)
- npm: [@agentclientprotocol/sdk](https://www.npmjs.com/package/@agentclientprotocol/sdk) · [@zed-industries/claude-code-acp](https://www.npmjs.com/package/@zed-industries/claude-code-acp)
