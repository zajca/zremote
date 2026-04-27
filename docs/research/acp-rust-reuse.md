# ACP Rust reuse — what to depend on, what to vendor, what to build

**Date:** 2026-04-25
**Builds on:** [`README.md`](./README.md), [`acp-spec.md`](./acp-spec.md), [`acp-ecosystem.md`](./acp-ecosystem.md), [`driver-architecture.md`](./driver-architecture.md), [`zremote-acp-integration-points.md`](./zremote-acp-integration-points.md), [`rfc-010-session-drivers-and-acp.md`](../rfc/rfc-010-session-drivers-and-acp.md)
**Scope:** Code-level deep dive into the Rust ACP SDK family and Zed's open-source agent code, with a build-vs-depend-vs-vendor classification for every load-bearing work item in RFC-010.

---

## 1. TL;DR

- **Depend, don't build, the JSON-RPC connection.** `agent-client-protocol = "=0.11.1"` ships the full Client/Agent/Proxy/Conductor builder, JSON-RPC dispatcher, request/response router, dynamic handler chain, session machinery (`SessionBuilder`, `ActiveSession`), and protocol types. Pin Zed's exact version (`=0.11.1`) and use the `unstable` feature group.
- **Depend on `agent-client-protocol-tokio = "=0.11.1"`** for child-process spawn + line-framed stdio + stderr capture + kill-on-drop. It already includes `AcpAgent::zed_claude_code()` (npx invocation), `AcpAgent::zed_codex()`, `AcpAgent::google_gemini()`, plus `from_str` parsing of either bash command or JSON config. **This deletes ~250 lines from RFC-010 P2's plan** (subprocess plumbing, stderr task, child guard, ChildGuard wrapper).
- **Skip `agent-client-protocol-test`.** It is `publish = false` (verified against crates.io: HTTP 404) and contains only mock types for SDK doctests, not a parity-test harness. RFC-010 P0's parity testing must be **built** by us against captured PTY traces — there is no shortcut.
- **Skip `agent-client-protocol-conductor` for runtime use** but read `cookbook::running_proxies_with_conductor` and `concepts::proxies` for inspiration. Conductor wraps messages in a `_proxy/successor/*` envelope that is *not* compatible with talking directly to an agent — irrelevant for our agent-side driver use case.
- **Defer `agent-client-protocol-rmcp`** unless we want to expose ZRemote's knowledge MCP server *to* the ACP agent through the SDK rather than through ACP's `mcp_servers` field on `NewSessionRequest`. RFC-010 §6.3 already wires this through MCP-by-config; the `-rmcp` crate is for *building* MCP servers in Rust, not for forwarding existing ones. Not in P2/P3 scope.
- **Vendor (with attribution) Zed's `handle_session_update` translator shape and `AgentThreadEntry` data model** from `zed-industries/zed`'s `crates/acp_thread/` (Apache-2.0). The match arms in `acp_thread.rs:1428-1504` are a complete, tested mapping from `acp::SessionUpdate` to per-session GUI state. Strip GPUI-specific types (Markdown entity, Diff entity, Terminal entity), keep the structure.
- **Build, no avoidable shortcut**: the diff-review widget (P4), the ACP→ZRemote canonical event translator (the *interface* between SDK and our `LoopStateUpdate`/`ExecutionNode`), the path-validating `fs/*` sandbox, the `terminalId` namespace + ring buffer for `terminal/*`, and the registry-fetch + agent-install machinery. Zed solves these but coupled to its workspace abstraction.
- **The `claude-agent-acp` adapter has been renamed twice.** SDK code references `@zed-industries/claude-code-acp` (deprecated), npm now hosts `@zed-industries/claude-agent-acp`, and the canonical home is `@agentclientprotocol/claude-agent-acp@0.31.0`. RFC-010 P2 should pin `@agentclientprotocol/claude-agent-acp` and document the rename history. This single fact is a P2 risk item and was not in earlier research.

---

## 2. Crate-by-crate assessment of the rust-sdk family

The workspace at `agentclientprotocol/rust-sdk` (Apache-2.0) ships nine crates. Source: `/tmp/rust-sdk/Cargo.toml:1-18`.

### 2.1 `agent-client-protocol = "=0.11.1"` — **DEPEND**

The core SDK. Re-exports `agent-client-protocol-schema` (which is what 0.12.x is — the schema crate, separately versioned).

**What it gives us:**
- All ACP message types (`InitializeRequest`, `NewSessionRequest`, `PromptRequest`, `SessionNotification`, `RequestPermissionRequest`, `CreateTerminalRequest`, all `fs/*` requests, `Plan`, `ToolCall`, `ContentBlock`, `StopReason`, etc.) — see `agent-client-protocol/src/lib.rs:130-138` for the public re-exports and `agent-client-protocol-schema-0.12.2/src/client.rs:84-115` for the `SessionUpdate` enum.
- `Client.builder()` / `Agent.builder()` / `Proxy.builder()` / `Conductor.builder()` — fluent connection setup with `on_receive_request`, `on_receive_notification`, `on_receive_dispatch`. See `agent-client-protocol/src/lib.rs:113-118` and `cookbook::connecting_as_client`.
- `ConnectTo<Role>` trait — abstracts over the transport. `AcpAgent` (subprocess) and `Stdio` (this process's own stdio) both implement it. We can implement it for our own transport if needed (e.g., to relay over our WS tunnel — though RFC-010 §3.5 explicitly says we don't).
- `SessionBuilder` / `ActiveSession` — high-level API for `session/new` + `session/prompt` + reading `session/update` notifications. `ActiveSession::send_prompt()`, `read_update()`, `read_to_string()`. See `agent-client-protocol/src/session.rs:559-614`.
- `SentRequest` with two consumption modes: `block_task()` (`.await` inside a spawned task) and `on_receiving_result(callback)` (callback fires when response arrives, blocking the dispatch loop briefly). Zed wraps both into a single `into_foreground_future` helper at `zed/crates/agent_servers/src/acp.rs:214-229` — recommended pattern.
- All cargo features for "stable in spec, gated as unstable in SDK" methods: `unstable_session_resume`, `unstable_session_close`, `unstable_session_fork`, `unstable_session_model`, `unstable_session_additional_directories`, etc. Source: `rust-sdk/src/agent-client-protocol/Cargo.toml:18-39`.

**Ergonomics observations:**
- The builder uses `async closure` extensively. The macros `on_receive_request!()`, `on_receive_notification!()`, `on_receive_dispatch!()` are required because `return-type notation` is not stabilized — see `agent-client-protocol/src/lib.rs:151-209`. Mildly ugly, but stable Rust 2024.
- `block_task()` deadlocks if used inside a handler. Comment at `cookbook::connecting_as_client::block_task` and at `zed/crates/agent_servers/src/acp.rs:200-213` documents this carefully. Use `on_receiving_result` from within handlers.
- Zed pins `=0.11.1` with the full `unstable` feature group. RFC-010 §6.13 already plans the same. **Confirmed correct.**

**Recommendation: DEPEND. Pin `=0.11.1` (exact), `features = ["unstable"]`.** Maps to RFC-010 P2 line "Pull in `agent-client-protocol = "=0.11.1"` with `unstable_session_resume` and `unstable_session_close`" — the actual feature is just `unstable` which forwards all flags.

### 2.2 `agent-client-protocol-schema = "=0.12.2"` — **TRANSITIVE**

The schema-only types. The core SDK depends on it; no need to add it directly. Note version skew: schema is 0.12.x while SDK is 0.11.x.

**What's in it (Source: `/tmp/acp-schema/agent-client-protocol-schema-0.12.2/src/*`):**
- `agent.rs` (schema for agent-side requests/notifications)
- `client.rs` (`SessionUpdate` enum at line 84, `CurrentModeUpdate`, `ConfigOptionUpdate`, `SessionInfoUpdate`, `UsageUpdate`)
- `tool_call.rs` (674 lines — all tool call shapes including diffs)
- `plan.rs` (147 lines — `Plan`, `PlanEntry`)
- `content.rs`, `elicitation.rs`, `error.rs`, `nes.rs` (next edit suggestion), `protocol_level.rs`, `rpc.rs`, `version.rs`

The full `SessionUpdate` enum (file `client.rs:84-115`) lists every variant we will translate from in our driver. **Goose uses this crate without the full SDK** (Source: `/tmp/goose-block/crates/goose/Cargo.toml:149`) because they need only types. We don't follow that path because we want the connection machinery too.

**Recommendation: transitive only. Don't add to our `Cargo.toml`.**

### 2.3 `agent-client-protocol-tokio = "=0.11.1"` — **DEPEND**

The crate that buys us the most engineering. Source: `/tmp/rust-sdk/src/agent-client-protocol-tokio/src/{lib.rs,acp_agent.rs}`.

**What it gives us:**
- `AcpAgent` — wraps an MCP-style stdio config (`McpServerStdio`). Implements `ConnectTo<Client>` and `ConnectTo<Conductor>`. Spawning, stderr-tee, stdin write-line, EOF handling, `select!` between protocol future and child-exit are all handled (`acp_agent.rs:280-371`).
- `AcpAgent::from_str("python my_agent.py")` and `AcpAgent::from_str(json_config)` parse either form (`acp_agent.rs:471-498`).
- `AcpAgent::from_args(["RUST_LOG=debug", "cargo", "run", "-p", "my-crate"])` parses leading `K=V` as env (`acp_agent.rs:392-443`).
- Convenience constructors:
  - `AcpAgent::zed_claude_code()` → `npx -y @zed-industries/claude-code-acp@latest` (deprecated package name; see §5)
  - `AcpAgent::zed_codex()` → `npx -y @zed-industries/codex-acp@latest`
  - `AcpAgent::google_gemini()` → `npx -y -- @google/gemini-cli@latest --experimental-acp`
- `with_debug(callback)` — taps every line in/out + every stderr line. Maps directly to RFC-010's "agent stderr surfaced as banner" requirement (P2 §4.5).
- `ChildGuard` — wraps the child so dropping the connection kills the process (`acp_agent.rs:225-237`). Solves RFC-010 risk #6 (server-mode child management).
- `Stdio` (`lib.rs:14-104`) — for the agent-side case where *we* are the agent on stdio. Not relevant to v1 (case B from earlier research, deferred).

**Ergonomics observations:**
- The convenience constructors hardcode the deprecated `claude-code-acp` package. **We must build our own** with `@agentclientprotocol/claude-agent-acp`. Use `AcpAgent::from_str("npx -y @agentclientprotocol/claude-agent-acp@latest")` instead, with a fallback to `@zed-industries/claude-agent-acp` for environments stuck on the older name.
- The crate doesn't surface `child.id()` or `wait()` directly — `monitor_child` is internal (`acp_agent.rs:243-268`). For our health-check / "agent crashed" UI banner (RFC-010 §4.5), we'd want the exit status reactively. This is a tiny gap; we either fork-and-PR upstream or wrap with our own `Command` + their connection logic.

**Recommendation: DEPEND. Use `AcpAgent::from_str(...).with_debug(|line, dir| ...)`.** Add this to RFC-010 P2 as the spawn primitive and budget zero engineering for child-process plumbing.

### 2.4 `agent-client-protocol-conductor = "=0.11.1"` — **SKIP, study patterns**

A binary + library for "proxy chains in front of an agent." Source: `/tmp/rust-sdk/src/agent-client-protocol-conductor/src/lib.rs:1-50`.

**What it does:**
```
Editor ← stdio → Conductor → Proxy 1 → Proxy 2 → Agent
```
The conductor wraps each message toward the agent in a `_proxy/successor/*` envelope (`SuccessorMessage`), so proxies can intercept/transform. The agent is unaware. **Direct connection to an agent through the conductor's proxy protocol does NOT work** — see `cookbook::running_proxies_with_conductor` line 838: *"If you connected directly to an agent, your proxy would send `SuccessorMessage` envelopes that the agent doesn't understand."*

**Why it's wrong for us:** Our use case is the inverse. We want a *client* talking to a single agent. We are not building a proxy chain. The conductor is for cases like "add Sparkle embodiment + custom tools to any agent" (cookbook example).

**Could it serve our `Server → Agent` tunnel?** No. Our tunneling goal (RFC-010 §3.5) is *"don't tunnel raw ACP frames at all — translate to canonical events."* The conductor solves a different problem: tunneling *raw ACP* with proxy-chain semantics. Adopting it would defeat the canonical-event design.

**Recommendation: SKIP for runtime. Read `concepts::proxies` once for vocabulary.**

### 2.5 `agent-client-protocol-rmcp = "=0.11.1"` — **DEFER**

Glue between `agent-client-protocol::mcp_server::McpServer` and `rmcp` (the Model Context Protocol SDK). Source: `/tmp/rust-sdk/src/agent-client-protocol-rmcp/Cargo.toml`.

**What it does:** `McpServerExt::from_rmcp(name, factory)` lets you wrap an existing rmcp server as an ACP MCP server (cookbook §`global_mcp_server` shows the example). Used when *you* are the proxy or agent and want to expose Rust MCP tools through the ACP protocol's `_meta.symposium` MCP-server registration.

**Why it's not P2/P3:** ZRemote's existing knowledge MCP server (`crates/zremote-agent/src/mcp/`) is already an MCP server. ACP `NewSessionRequest` carries an `mcp_servers: Vec<McpServer>` field where each entry is a stdio command, HTTP URL, or SSE URL. We forward our existing MCP server *by config* (i.e., as a separate process or HTTP endpoint), not by re-implementing it through `from_rmcp`. RFC-010 §8 open question 3 already plans this.

**When it might matter:** if we ever want to embed our knowledge MCP server *as a library* inside the same Rust process as the ACP driver (no separate stdio fork). That's an optimization for later, not P3.

**Recommendation: DEFER. Document path: future ZRemote consolidation could turn agent's MCP into an `rmcp::ServerHandler` and use `McpServerExt::from_rmcp`.**

### 2.6 `agent-client-protocol-derive = "=0.11.0"` — **TRANSITIVE**

`#[derive(JsonRpcRequest)]` etc. proc macros. Used by the core SDK. Re-exported via `agent_client_protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse}` (`lib.rs:140`). Only needed if we define *custom* JSON-RPC message types (e.g., for our own protocol extensions on `_meta`). RFC-010 doesn't plan any.

**Recommendation: transitive only.**

### 2.7 `agent-client-protocol-cookbook = "=0.11.1"` — **READ, don't depend**

A documentation-only crate (Source: `/tmp/rust-sdk/src/agent-client-protocol-cookbook/src/lib.rs`). Every module is a long doc comment with code examples:

- `one_shot_prompt` — minimal client, useful for our smoke tests.
- `connecting_as_client` — the canonical client setup, with permission handling (lines 173-188 show the `on_receive_request` for `RequestPermissionRequest`).
- `building_an_agent` — agent-side, not relevant in v1 (case B from research).
- `reusable_components` — pattern for `ConnectTo` impls. Useful if we ever want our driver to be a `ConnectTo` (we don't in v1).
- `custom_message_handlers` — `MatchDispatch` for routing. Used for proxy/server work. Not in v1 critical path.
- `global_mcp_server`, `per_session_mcp_server`, `filtering_tools` — only useful when *we* are the proxy/agent. Not v1.
- `running_proxies_with_conductor` — see §2.4 above.

**Recommendation: read `one_shot_prompt`, `connecting_as_client` once. Don't add as a dep — it's pure docs.** The core SDK already has `examples/yolo_one_shot_client.rs` and `examples/simple_agent.rs` for runnable code.

### 2.8 `agent-client-protocol-test` — **UNAVAILABLE** (publish = false)

Source: `/tmp/rust-sdk/src/agent-client-protocol-test/Cargo.toml:11` — `publish = false`. Verified via crates.io API: `https://crates.io/api/v1/crates/agent-client-protocol-test` returns *crate ... does not exist*.

**What's actually in it (we cannot use directly, but content matters because it tells us what *isn't* there):**
- `MockTransport` (`lib.rs:11-17`) — panics if used; only for doctests.
- `MyRequest`, `MyResponse`, `ProcessRequest`, `AnalyzeRequest`, etc. — mock JSON-RPC types for the cookbook examples. `lib.rs:18-90`.
- `mcp-echo-server` and `testy` binaries — internal SDK test fixtures.

**This is not a parity-test harness.** Earlier research's wishful framing of "use `agent-client-protocol-test` for P0 parity tests" doesn't pan out. We have to build P0's golden-trace replay ourselves against captured PTY+analyzer output.

**Recommendation: cannot DEPEND, would need to VENDOR if we wanted any of it — but we don't, the mocks aren't useful. RFC-010 P0 testing remains a BUILD item.**

### 2.9 `agent-client-protocol-trace-viewer = "=0.11.0"` — **DEFER, dev only**

A binary that visualizes JSON-RPC sequence traces from the conductor. Source: `/tmp/rust-sdk/src/agent-client-protocol-trace-viewer/`. Useful as a dev tool when debugging ACP flows: pipe stderr from `with_debug` callback into a trace file, open with the viewer.

**Recommendation: install as `cargo install agent-client-protocol-trace-viewer` for development; do not link as a runtime dep.**

---

## 3. Zed agent code walkthrough (Apache-2.0)

Repo: `zed-industries/zed`. License: Apache-2.0 (top-level `LICENSE`). Branch: `main` as of 2026-04-25. Crates of interest, sparse-checked at `/tmp/zed/`:

| Crate | Path | Lines | Role |
|---|---|---|---|
| `agent_servers` | `crates/agent_servers/src/` | acp.rs 3624; agent_servers.rs 157; custom.rs 599; e2e_tests.rs 500 | The ACP-talking-to-an-agent layer. Spawns subprocess, holds `ConnectionTo<Agent>`, forwards every inbound to a foreground GPUI dispatch queue. |
| `acp_thread` | `crates/acp_thread/src/` | acp_thread.rs 5514; connection.rs 1029; diff.rs 454; mention.rs 808; terminal.rs 255 | Per-session UI state: list of `AgentThreadEntry`, plan, modes, models, terminals, diffs. |
| `agent` | `crates/agent/src/` | many files, ~10k lines | Higher-level agent abstraction over `acp_thread` for the chat panel. Heavily Zed-specific (workspace, project, language registry). Skip. |

Reuse implications: Apache-2.0 is **compatible** with our project — we can vendor with the required attribution (NOTICE file + license preservation per Apache-2.0 §4). Zed's code is heavily coupled to GPUI's `Entity<T>`, `Context<T>`, `Markdown`, `Diff`, `Terminal` types. **Direct copy-paste won't compile in our tree;** the structural patterns and the translator match-arms are the high-value vendor target.

### 3.1 Per-session state struct

`zed/crates/acp_thread/src/acp_thread.rs:1038-…` defines `AcpThread`:

```rust
pub struct AcpThread {
    title: Option<SharedString>,
    provisional_title: Option<...>,
    entries: Vec<AgentThreadEntry>,           // user msgs, assistant msgs, tool calls, completed plans
    plan: ...,                                // pending Plan
    available_commands: Vec<acp::AvailableCommand>,
    token_usage: Option<TokenUsage>,
    cost: Option<SessionCost>,
    streaming_text_buffer: ...,               // for smooth markdown streaming
    // ... + connection handle, project ref, action log, watch channels for capabilities
}
```

`AgentThreadEntry` enum (acp_thread.rs:172-177):
```rust
pub enum AgentThreadEntry {
    UserMessage(UserMessage),
    AssistantMessage(AssistantMessage),
    ToolCall(ToolCall),                       // detailed shape at acp_thread.rs:247-260
    CompletedPlan(Vec<PlanEntry>),
}
```

`ToolCall` (acp_thread.rs:247-260) maps 1:1 to `acp::ToolCall` plus rendering caches (`label: Entity<Markdown>`, resolved file locations, raw-input markdown). For ZRemote, the rendering caches are GPUI-specific; the *logical* fields (id, kind, content, status, locations, raw_input, raw_output) match ACP's schema directly.

**Mapping to RFC-010 canonical events:**

| Zed `AgentThreadEntry` | RFC-010 `DriverEvent` |
|---|---|
| `UserMessage` | (we don't have one — user input goes through `DriverControl::send_user_input`; if we want history, add `DriverEvent::UserMessageEcho`) |
| `AssistantMessage.chunks` | `DriverEvent::AgentMessageChunk { text }` (one event per chunk) |
| `ToolCall` (status = `Pending`/`InProgress`) | `DriverEvent::ToolCall(ExecutionNode)` with status |
| `ToolCall` (status = `Completed`/`Failed`) | a second `DriverEvent::ToolCall` with the updated status |
| `CompletedPlan` | `DriverEvent::Plan(Vec<PlanEntry>)` |

**Reuse implication:** ZRemote's existing `ExecutionNode` (in `crates/zremote-protocol/src/agentic.rs`) already covers the tool-call shape. **No need to vendor `ToolCall`; just translate.** The vendoring target is the *match arms* of the translator below, not the data structures.

### 3.2 The translator: `handle_session_update`

The single most reusable piece. `zed/crates/acp_thread/src/acp_thread.rs:1428-1504`:

```rust
pub fn handle_session_update(
    &mut self,
    update: acp::SessionUpdate,
    cx: &mut Context<Self>,
) -> Result<(), acp::Error> {
    match update {
        acp::SessionUpdate::UserMessageChunk(acp::ContentChunk { content, .. }) => {
            // Optimistically dedupe against the last entry's user message
            // (some agents echo user chunks back).
            ...self.push_user_content_block(...)...
        }
        acp::SessionUpdate::AgentMessageChunk(...) => self.push_assistant_content_block(content, false, cx),
        acp::SessionUpdate::AgentThoughtChunk(...) => self.push_assistant_content_block(content, true, cx),
        acp::SessionUpdate::ToolCall(tool_call)            => self.upsert_tool_call(tool_call, cx)?,
        acp::SessionUpdate::ToolCallUpdate(tool_call_update) => self.update_tool_call(tool_call_update, cx)?,
        acp::SessionUpdate::Plan(plan)                       => self.update_plan(plan, cx),
        acp::SessionUpdate::SessionInfoUpdate(info_update)   => /* update title with provisional logic */,
        acp::SessionUpdate::AvailableCommandsUpdate(...)     => /* emit AvailableCommandsUpdated */,
        acp::SessionUpdate::CurrentModeUpdate(...)           => cx.emit(AcpThreadEvent::ModeUpdated(...)),
        acp::SessionUpdate::ConfigOptionUpdate(...)          => cx.emit(AcpThreadEvent::ConfigOptionsUpdated(...)),
        acp::SessionUpdate::UsageUpdate(update) if cx.has_flag::<AcpBetaFeatureFlag>() => /* update token_usage */,
        _ => {}
    }
    Ok(())
}
```

The non-obvious bits worth vendoring:

1. **User-chunk dedup logic** (lines 1438-1445): some agents echo user chunks back over `session/update`. Zed checks `self.entries.last().and_then(|e| e.user_message()).is_some_and(|m| m.chunks.contains(&content))` before appending. Without this, the user's prompt appears twice.
2. **Streaming text buffer** for smooth markdown (acp_thread.rs:1573-1584). Zed buffers `AgentMessageChunk` text into a `streaming_text_buffer` and flushes on a timer/non-text update, instead of re-rendering the markdown entity on every token.
3. **Provisional title handling** (lines 1462-1473) — a session might be in a temporary "summarizing" title state; `SessionInfoUpdate` clears it.
4. **`UsageUpdate` is feature-flagged** behind `AcpBetaFeatureFlag` because the schema gates it on `unstable_session_usage`. We turn the cargo feature on; keep the runtime flag in our own build to avoid surprising the user.

**Pre/post-handle for terminal embeds:** `zed/crates/agent_servers/src/acp.rs:3296-3451` shows that **before** dispatching to `handle_session_update`, Zed inspects `tool_call.meta.terminal_info` and creates a display-only terminal (`TerminalProviderEvent::Created`) and **after**, on `ToolCallUpdate`, it streams `terminal_output` and `terminal_exit` through the same channel. ACP doesn't yet have first-class terminal-in-tool-call schema, so the `_meta` field is the convention — **this is undocumented in the spec; only Zed's source reveals it.** RFC-010 §4.2 mentions "embedded terminals" but doesn't capture this. **P4 must replicate the meta-field convention or it won't interop with `claude-agent-acp`.**

### 3.3 Connection setup and agent process management

`zed/crates/agent_servers/src/acp.rs:526-902` (`AcpConnection::stdio`).

**The pattern:** Zed does **not** use `AcpAgent::from_str` from the tokio crate. They build their own:
1. Resolve the command (`AgentServerCommand`) — including remote-host rewriting via `project.remote_client()` — line 671-694.
2. Build a `std::process::Command` via `ShellBuilder::new(&Shell::System, cfg!(windows)).non_interactive()` (line 696). This is Zed's wrapper that handles login-shell quoting, Windows quirks.
3. Set `stdin/stdout/stderr` to `Stdio::piped()`, spawn (line 708).
4. Tap the line-by-line reader (line 734-743) and writer (line 745-755) with their `AcpDebugLog` for inspector UI. The tap is nearly identical to `AcpAgent::with_debug` but stores a ring buffer of `MAX_DEBUG_BACKLOG_MESSAGES = 2000` parsed messages.
5. Wrap the tapped streams in `agent_client_protocol::Lines::new(outgoing, incoming)` (line 757) — exact same primitive `AcpAgent` uses internally.
6. Call `Client.builder()....connect_with(transport, |conn| async { connection_tx.send(conn).ok(); pending::<...>().await })` to establish the connection and surface the `ConnectionTo<Agent>` handle through a oneshot.
7. Spawn three tasks: `io_task` (drives the connection future), `dispatch_task` (foreground worker pulling from the `Send` mpsc queue), `wait_task` (`child.status()` → emits `LoadError::Exited` to all sessions when the child dies), `stderr_task` (reads stderr, logs at `warn`, records in debug log). Lines 766-816.
8. Send `InitializeRequest::new(ProtocolVersion::V1).client_capabilities(...).client_info(...)` via the `into_foreground_future` helper, await response, check `protocol_version >= MINIMUM_SUPPORTED_VERSION` (line 817-842).
9. Build the `AcpConnection` struct holding **all four tasks** as `_io_task`, `_dispatch_task`, `_wait_task`, `_stderr_task` (lines 400-404). Drop = cancel.

**Lessons for ZRemote:**
- The `_io_task: Task<()>` etc. fields enforce RFC-010's "no `.detach()` for long-running work" rule. RFC-010's CLAUDE.md async-task convention matches Zed's pattern verbatim.
- The `wait_task` that emits `LoadError::Exited` to all session threads is exactly RFC-010 §4.5 case 1 ("Agent process exits unexpectedly"). **Vendor the structure.** ZRemote's `DriverHandle` should hold its own `wait_task` and emit `DriverEvent::SessionEnded { stop_reason: ShellExit { code, signal } }` on child exit.
- The foreground/background bridge (`enqueue_request` / `enqueue_notification` / `ForegroundWorkItem` trait at `acp.rs:276-383`) is GPUI-specific (the foreground thread is `!Send`). **Tokio is `Send`-friendly; we don't need this.** Replace with direct `tokio::sync::mpsc::Sender<DriverEvent>` calls inside SDK handlers.
- `into_foreground_future` (acp.rs:214-229) — wraps a `SentRequest` so it can be `.await`ed from a `!Send` GPUI task. We can use `block_task()` directly inside our spawned tokio tasks. **No vendoring needed.**

### 3.4 Capability advertising

`zed/crates/agent_servers/src/acp.rs:817-836` shows what Zed advertises:

```rust
acp::ClientCapabilities::new()
    .fs(acp::FileSystemCapabilities::new()
        .read_text_file(true)
        .write_text_file(true))
    .terminal(true)
    .auth(acp::AuthCapabilities::new().terminal(true))
    .meta(acp::Meta::from_iter([
        ("terminal_output".into(), true.into()),
        ("terminal-auth".into(), true.into()),
    ]))
```

**Notable:** `terminal_output` and `terminal-auth` are *meta-keys* (extension flags) Zed invented. `claude-agent-acp` checks for them. **Without these meta keys, terminal output streaming inside tool calls won't work.** This is again undocumented in the spec — read it from the source. **RFC-010 P5 must include these meta keys in our `InitializeRequest`.**

### 3.5 Permission handling

`zed/crates/agent_servers/src/acp.rs:594-597` registers `handle_request_permission` as the handler for `RequestPermissionRequest`. The handler enqueues onto the foreground dispatch queue, where it eventually maps to a GPUI modal. Translation pattern matches RFC-010 §3.7's plan exactly: ACP request → channel `PermissionRequest`. **No code to vendor — pattern is a one-line forward.**

### 3.6 What is not reusable

- `Entity<Markdown>`, `Entity<Diff>`, `Entity<Terminal>` — GPUI-specific; we use a different rendering layer.
- `Project`, `ActionLog`, `LanguageRegistry` references — Zed's workspace model; we have project + worktree from RFC-009 instead.
- `AcpDebugLog` (acp.rs:147-198) — interesting as a debug pane, but not core. Could be a future devtool.
- `proxy_remaining_messages` (in `agent-client-protocol::session::ActiveSession`) — proxy use case, not ours.
- The `pending_sessions: Rc<RefCell<HashMap<...>>>` ref-count bookkeeping for shared session loads (acp.rs:407-409, 941-1080) — we don't share sessions across windows in v1.

### 3.7 License compliance for vendoring

Apache-2.0 §4 requires:
1. Carry copy of the license with redistributed source.
2. State changes prominently in the modified files.
3. Retain attribution notices, copyright statements, NOTICE files.
4. If we redistribute, our NOTICE must mention the upstream NOTICE.

**Practical:** add `LICENSES/Apache-2.0-zed.txt` (or extend an existing third-party-licenses file), prefix vendored files with `// Adapted from zed-industries/zed at <commit>, Apache-2.0. See LICENSES/Apache-2.0-zed.txt`. Add a workspace `NOTICE` file once we vendor *anything*.

---

## 4. Other Rust ACP apps

### 4.1 Goose (`block/goose`) — **server, not client**

| Field | Value |
|---|---|
| Repo | https://github.com/block/goose |
| License | Apache-2.0 |
| Crates | `crates/goose` (with `src/acp/` subtree), `crates/goose-acp-macros` |
| Role | Goose ships an ACP **agent** server with HTTP and WebSocket transports — opposite direction from us. |
| Uses SDK? | Only `agent-client-protocol-schema` (see `goose-block/crates/goose/Cargo.toml:149`). They wrote their own JSON-RPC dispatch and HTTP/WS framing. |
| Reusable? | Their HTTP/WS transport (`crates/goose/src/acp/transport/{http.rs,websocket.rs}`) is interesting *if we ever decide to expose ZRemote-as-ACP-server* (case B from research, deferred indefinitely per RFC-010 §2 non-goals). For v1, **not reusable.** |
| Notable | Goose's roadmap (per their discussions) is to consolidate goosed + goose-cli behind `/acp`. Validates that ACP-over-HTTP is becoming a real thing. |

**Verdict:** instructive for case B, irrelevant for case A (our v1).

### 4.2 `Xuanwo/acp-claude-code` — **deprecated TypeScript**

Archived 2025-09-08. TypeScript. MIT-licensed. Replaced by `@zed-industries/claude-code-acp` (now also deprecated; see §5). Skip.

### 4.3 Other 27 ACP-listed agents

The earlier ecosystem report listed 27 agents in the registry. Cross-referencing crates.io:

- `goose-acp` — **not published on crates.io** (verified: HTTP 404).
- No `*-acp` crates on crates.io belong to any of the registry agents. They are all TypeScript adapters.

**Verdict:** there is no "another Rust ACP client" to imitate beyond Zed. Goose is a Rust server; everyone else is TS/Python.

---

## 5. Claude Code adapter spawn cookbook

Source: `/tmp/claude-agent-acp/{package.json,src/index.ts,src/acp-agent.ts}`.

### 5.1 Package identity confusion

The package has been renamed twice:

| Name | Status |
|---|---|
| `@zed-industries/claude-code-acp` | **Deprecated** — last 0.16.x line. SDK's `AcpAgent::zed_claude_code()` still hardcodes this. |
| `@zed-industries/claude-agent-acp` | **Active mirror** — second rename, npm-only. |
| `@agentclientprotocol/claude-agent-acp` | **Canonical, active** — version 0.31.0 as of 2026-04. Repo `agentclientprotocol/claude-agent-acp`. |

**For RFC-010 P2:** spawn `@agentclientprotocol/claude-agent-acp@latest`. Add a fallback to `@zed-industries/claude-agent-acp@latest` only if the canonical name fails to resolve in npm. Do **not** rely on the SDK's `AcpAgent::zed_claude_code()` — its hardcoded package is deprecated.

### 5.2 Exact spawn command

```bash
npx -y @agentclientprotocol/claude-agent-acp@latest
```

Or from a Rust driver, equivalent to:
```rust
let agent = AcpAgent::from_str("npx -y @agentclientprotocol/claude-agent-acp@latest")?
    .with_debug(|line, dir| { /* tee to log file */ });
```

**Required runtime:** Node.js ≥ 18 (per `package.json` deps on `@anthropic-ai/claude-agent-sdk@0.2.119`). The Anthropic SDK ships a native binary as a platform-specific optional npm dependency (`acp-agent.ts:280-283`). If `npm install --omit=optional` was used, set `CLAUDE_CODE_EXECUTABLE` to the path of the native CLI binary.

### 5.3 What the adapter spawns internally

`acp-agent.ts:267-295` shows the resolution chain for the underlying Claude Code CLI:

1. `process.env.CLAUDE_CODE_EXECUTABLE` if set — used as-is.
2. Otherwise, resolve `@anthropic-ai/claude-agent-sdk-{platform}-{arch}{-musl}/claude` via Node `createRequire`. Linux first tries `-musl`, falls back to glibc.
3. Throws if not found.

So the ACP adapter is *not* "Claude Code installed somewhere on PATH." It's a Node.js wrapper that ships its own copy of the Claude Code native binary as an npm optional-dep. This is good news for ZRemote: **one `npx` invocation pulls in everything.**

### 5.4 Environment variables the adapter consumes

Exhaustive list from `grep "process.env" src/acp-agent.ts`:

| Env var | Purpose | Set by us? |
|---|---|---|
| `CLAUDE_CODE_EXECUTABLE` | Override the bundled native CLI path | No (let it auto-resolve) |
| `CLAUDE_CONFIG_DIR` | Where Claude reads `.claude/` config (defaults to `$HOME/.claude`) | Pass-through if user set it |
| `IS_SANDBOX` | Allow `bypassPermissions` mode even when running as root | No (we are not root) |
| `NO_BROWSER`, `SSH_CONNECTION`, `SSH_CLIENT`, `SSH_TTY`, `CLAUDE_CODE_REMOTE` | Disable browser-based OAuth flow when remote | **Yes** when ZRemote is in server mode (we are remote by definition for the agent host). Set `CLAUDE_CODE_REMOTE=1`. |
| `MAX_THINKING_TOKENS` | Numeric cap on extended thinking | Pass-through if set in profile |
| `CLAUDE_MODEL_CONFIG` | JSON Bedrock-style model overrides | Pass-through if set in profile |
| `ANTHROPIC_MODEL` | Force a specific model | Pass-through; per-profile setting |
| `ANTHROPIC_BASE_URL`, `ANTHROPIC_CUSTOM_HEADERS`, `ANTHROPIC_AUTH_TOKEN` | Gateway routing — set internally by adapter when its own gateway-auth config is present. **Don't override.** | No |

The adapter sets `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1` for the Claude Code SDK child internally (`acp-agent.ts:1752`). We don't set this from the outside.

### 5.5 MCP servers the adapter pre-configures

`acp-agent.ts:1757`: `mcpServers: { ...(userProvidedOptions?.mcpServers || {}), ...mcpServers }` — so MCP servers come from two sources:
1. `_meta.claudeCode.options.mcpServers` on the `NewSessionRequest` (set by the client).
2. The user's local `.claude/` config file (loaded by the Claude Agent SDK itself).

**For ZRemote (RFC-010 §8 open question 3):** we forward our knowledge MCP server via the `mcp_servers` field on `NewSessionRequest`. The adapter will merge it into the session.

### 5.6 ACP unstable features the adapter requires

The adapter is built against `@agentclientprotocol/sdk@0.20.0` (TypeScript SDK; numbering doesn't match Rust SDK). Looking at the protocol-level interactions in `acp-agent.ts`:

- `session/load` and `session/resume` — supported. We must enable `unstable_session_resume` on our Rust SDK.
- `session/cancel` — stable.
- `session/set_mode`, `session/set_model` — used. We need `unstable_session_model`.
- `current_mode_update` notification — stable in 0.12.x schema.
- `_meta.terminal_output` and `_meta.terminal-auth` capability keys — required (see §3.4).

**RFC-010 P2 update:** the SDK feature set is broader than just `unstable_session_resume + unstable_session_close`. Recommend `features = ["unstable"]` (the umbrella forwarding flag) — this is what Zed does.

### 5.7 Failure modes

| Mode | What happens | Surface to user |
|---|---|---|
| Auth failure (no API key + no OAuth login) | The Claude Code CLI itself returns an error on the first `session/prompt`. Adapter forwards as a `session/update` with stop reason `Error`. | Banner: "Claude Code is not authenticated. Run `claude /login` in your terminal." We have to surface stderr to detect this. |
| Network failure | Adapter forwards the underlying SDK error. `PromptResponse.stop_reason = Error` plus an `AgentMessageChunk` containing the error text. | Same banner; offer "Retry" button. |
| Model error / rate limit | Same path as network failure. | Same banner; suggest a different model from `session/set_model` if available. |
| Subprocess crash | `child.exit` event fires → our `wait_task` → `LoadError::Exited` → driver emits `SessionEnded { stop_reason: ShellExit { code, signal } }`. Captured stderr is in the banner. | Banner with "Restart" action. |
| Protocol mismatch | `InitializeResponse.protocol_version < V1` → SDK returns error from `connect_with`. | Driver reports start failure → launcher falls back to `cc-hooks` per RFC-010 §4.5 case 2. |

---

## 6. Reuse decision table

For each work item RFC-010 calls out:

| # | Work item | Verdict | Rationale |
|---|---|---|---|
| 1 | JSON-RPC 2.0 stdio framing (newline-delimited) | **DEPEND** | `agent-client-protocol::Lines::new(out_sink, in_stream)` + `ByteStreams` — `lib.rs:113-118`. Zero-cost; already battle-tested. |
| 2 | Bidirectional connection + request/response router | **DEPEND** | `Client.builder().on_receive_request().on_receive_notification().connect_with(transport, ...)` — `cookbook::connecting_as_client`. Zero new code. |
| 3 | All ACP message schema types | **DEPEND** (transitive via core SDK) | `agent_client_protocol::schema::*` re-exports `agent-client-protocol-schema = 0.12.x`. Match-arm enum already exhaustive (`#[non_exhaustive]`). |
| 4 | Subprocess spawn + lifecycle + stderr capture | **DEPEND** | `agent-client-protocol-tokio::AcpAgent::from_str(...).with_debug(...)`. `ChildGuard` kills on drop. `monitor_child` races protocol vs exit. ~250 LoC saved. |
| 5 | Translator: `session/update` → ZRemote `LoopStateUpdate`/`ExecutionNode`/`Plan`/`AgentMessageChunk` | **VENDOR** | The match-arm structure of Zed's `acp_thread.rs:1428-1504` is the model. Strip GPUI types; emit our canonical events. Ship as `crates/zremote-agent/src/session_driver/acp/translator.rs` with attribution header. |
| 6 | Multi-buffer diff review widget | **BUILD** | Zed's `acp_thread::Diff` (`crates/acp_thread/src/diff.rs:454 LoC`) is GPUI Editor-coupled. Our diff renderer is GPUI-too but uses different abstractions. The *data model* (path + oldText + newText + hunks + per-hunk Accept/Reject) is the standard one — vendor that struct shape. The widget is a build. |
| 7 | Tool-call card rendering data structures | **VENDOR shape, BUILD render** | Vendor `ToolCall { id, kind, content, status, locations, raw_input/output }` shape from acp_thread.rs:247-260. Render as GPUI element ourselves. |
| 8 | Permission UX state machine | **DEPEND** for protocol; **BUILD** for our policy bridge | The `RequestPermissionRequest` → user-facing options flow uses SDK types (`acp::PermissionOption`, `acp::RequestPermissionOutcome`). Our bridge translates to `ChannelAgentAction::PermissionRequest` (existing). Code is ~30 LoC. |
| 9 | Terminal byte-limit ring buffer + `terminalId` namespace | **BUILD** | ACP's `outputByteLimit` requires per-terminal byte counter wrapping. Our PTY layer (`crates/zremote-agent/src/pty/`) doesn't have this. Zed has it (`crates/acp_thread/src/terminal.rs:255 LoC` + `terminal::Terminal` from elsewhere) but it's their full terminal implementation — too big to vendor. **Build a `TerminalRingBuffer` wrapping our existing PTY in P5.** |
| 10 | Path-validating `fs/*` sandbox | **BUILD** | Goose has `goose/src/acp/fs.rs:440 LoC` and Zed has its own. Both bind to their workspace abstraction. ZRemote's worktree root (RFC-009) is the sandbox — the validation fn is ~20 LoC of `path.canonicalize().starts_with(worktree_root)`. **Build, don't vendor.** Security-reviewer signs off. |
| 11 | Session resume / load semantics | **DEPEND on SDK** for protocol; **BUILD** the policy | `unstable_session_resume` + `unstable_session_fork` features expose the methods. Our policy of *which* method to call when (RFC-010 §8 open Q4) is small build. |
| 12 | ACP Registry JSON parsing + agent install | **BUILD** | The registry format (`https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`) is documented by spec. JSON parsing is a `serde::Deserialize` derive. The npx-install side-effect machinery and signed-binary download is **not** in any SDK crate — Zed has it inside their `agent_registry_store` (not in our sparse checkout, but it's not generic-enough to vendor anyway). RFC-010 P3 ~1 day of build. |
| 13 | Test harness for parity testing | **BUILD** | `agent-client-protocol-test` is `publish=false` and contains only doctest mocks, not a parity harness. **Build P0's golden-trace replay against captured PTY+analyzer output ourselves.** This is the same pattern we already use in `agentic/adapters/` tests — extend, don't introduce new tooling. |

---

## 7. RFC-010 amendments

Concrete deltas to the RFC's phasing and scope. Patches to be applied during P0 review.

### 7.1 §5 Phasing — effort revisions

| Phase | RFC-010 estimate | Revised | Reason |
|---|---|---|---|
| **P0 — Driver skeleton** | M | **M** (unchanged) | Test harness still build-our-own; refactor is the bulk of work. |
| **P1 — ClaudeHooksDriver** | S | **S** (unchanged) | Internal refactor only. |
| **P2 — ClaudeAcpDriver** | L | **M** | Subprocess plumbing, stderr-tee, kill-on-drop, child-exit monitor, stdio framing all ship in `agent-client-protocol-tokio`. JSON-RPC dispatch ships in core. **Net: ~400 LoC saved** — only translator + handlers remain. |
| **P3 — GenericAcpDriver + Registry** | M | **M** (unchanged) | SDK helps with transport but registry parsing + npx install + agent install policy is build. |
| **P4 — Rich UI features** | L | **L** (unchanged) | Multi-buffer diff widget remains the largest item. Zed structures inform; we still build. |
| **P5 — Host-side `terminal/*` and `fs/*`** | M | **M** (unchanged) | All-build. The `fs` sandbox is ~20 LoC; the `terminal/*` namespace + ring buffer is the bulk. |

### 7.2 §6 Decisions — additions

Add as decision **#14 (new):**

> **Spawn the canonical `@agentclientprotocol/claude-agent-acp` package, not the SDK's hardcoded `claude-code-acp`.**
> *Why:* Package was renamed twice. SDK's `AcpAgent::zed_claude_code()` points at the deprecated name. Use `AcpAgent::from_str("npx -y @agentclientprotocol/claude-agent-acp@latest")` directly. Document fallback to `@zed-industries/claude-agent-acp` in the troubleshooting docs.

Add as decision **#15 (new):**

> **Use `agent-client-protocol-tokio` `AcpAgent` for child-process management; do not roll our own subprocess code.**
> *Why:* The crate handles spawn, line-framed stdio, stderr capture, kill-on-drop, child-exit race-with-protocol — battle-tested in Zed. We add `with_debug` for tee-to-logfile.

Add as decision **#16 (new):**

> **`features = ["unstable"]` on `agent-client-protocol`, not the per-feature opt-in.**
> *Why:* Adapter requires `unstable_session_resume`, `unstable_session_close`, `unstable_session_fork`, `unstable_session_model`, `unstable_session_additional_directories` simultaneously. The umbrella `unstable` is what Zed uses (`zed/Cargo.toml`).

### 7.3 §7 Risks — additions

Add as risk **#9:**

> **Adapter package rename.** The Claude Code ACP adapter has been renamed twice (`claude-code-acp` → `@zed-industries/claude-agent-acp` → `@agentclientprotocol/claude-agent-acp`). Hardcoding any name is fragile. *Mitigation:* keep the package name in profile config (`AgentProfileData.settings_json.acp_command`), with a sensible default but user-overridable.

Add as risk **#10:**

> **Undocumented `_meta` keys in `InitializeRequest`.** Zed advertises `meta.terminal_output = true` and `meta.terminal-auth = true`. The Claude Code adapter reads them; without them, terminal embedding inside tool calls won't work. *Mitigation:* match Zed's capability advertisement byte-for-byte (`acp.rs:817-836`); document the meta keys in the driver's source comments.

Add as risk **#11:**

> **`agent-client-protocol-test` is unpublished.** Earlier research assumed a parity-test harness was available. It isn't. *Mitigation:* P0's parity testing is BUILD work — capture PTY+analyzer traces during the current implementation, replay through the new `PtyDriver`, assert event-stream equality. Same pattern as `agentic/adapters/` tests, larger scope.

### 7.4 §8 Open questions — partial answers

- **Q3 (MCP servers in `session/new`):** Forward via `NewSessionRequest.mcp_servers` field. Adapter merges automatically (`acp-agent.ts:1757`). Recommendation stands.
- **Q4 (Resume vs load):** SDK gates both behind `unstable_*`. The TypeScript adapter implements both. *New recommendation:* call `session/resume` for in-memory daemon-mode reconnect (no replay), `session/load` for cold-start after agent restart (full replay). Both methods need their respective unstable features enabled — covered by `features=["unstable"]`.
- **Q5 (Auth UX):** ACP's `AuthCapabilities::new().terminal(true)` lets the adapter spawn an auth terminal back through us. Zed advertises this; we should too. P3 ships with the auth-terminal flow, no in-app OAuth in v1.

### 7.5 §10 References — additions

Add:
- ACP Rust SDK: `agentclientprotocol/rust-sdk` — Apache-2.0
- Claude Code ACP adapter: `agentclientprotocol/claude-agent-acp` — Apache-2.0
- Goose ACP server (reference for case B, deferred): `block/goose` — Apache-2.0
- Zed agent crates: `zed-industries/zed/crates/{agent_servers,acp_thread}` — Apache-2.0

---

## 8. Sources

Every claim above traces to one of:

1. `/tmp/rust-sdk/src/agent-client-protocol/{Cargo.toml,src/{lib.rs,session.rs}}` — core SDK, version `=0.11.1`.
2. `/tmp/rust-sdk/src/agent-client-protocol-tokio/src/{lib.rs,acp_agent.rs}` — tokio crate, version `=0.11.1`.
3. `/tmp/rust-sdk/src/agent-client-protocol-test/{Cargo.toml,src/lib.rs}` — `publish = false`, mock-types only.
4. `/tmp/rust-sdk/src/agent-client-protocol-conductor/{Cargo.toml,src/lib.rs}` — `_proxy/successor/*` envelope; not for client→agent direct.
5. `/tmp/rust-sdk/src/agent-client-protocol-cookbook/src/lib.rs` — doc-only crate; cookbook patterns.
6. `/tmp/rust-sdk/src/agent-client-protocol-rmcp/Cargo.toml` — rmcp glue.
7. `/tmp/acp-schema/agent-client-protocol-schema-0.12.2/src/{client.rs,tool_call.rs,plan.rs}` — schema crate (0.12.2 used by SDK 0.11.x).
8. crates.io API: confirmed `-tokio`, `-rmcp`, `-cookbook`, `-conductor`, `agent-client-protocol-schema` are at 0.11.1/0.12.2; `-test` does not exist on crates.io.
9. `/tmp/zed/crates/agent_servers/src/{agent_servers.rs,acp.rs,custom.rs}` — Zed's `AcpConnection`, foreground dispatch bridge, custom-agent settings, registry hooks.
10. `/tmp/zed/crates/acp_thread/src/{acp_thread.rs,connection.rs,diff.rs,terminal.rs}` — Zed's per-session state model, `handle_session_update` translator (lines 1428-1504), terminal-meta convention (acp.rs:3296-3451).
11. `/tmp/zed/Cargo.toml` — `agent-client-protocol = { version = "=0.11.1", features = ["unstable"] }` confirms the pinning + feature set.
12. `/tmp/claude-agent-acp/{package.json,src/{index.ts,acp-agent.ts,utils.ts}}` — adapter at version 0.31.0; canonical package `@agentclientprotocol/claude-agent-acp`; env vars enumerated; native CLI resolution at lines 267-295.
13. `/tmp/goose-block/crates/goose/src/acp/{transport/{mod.rs,http.rs,websocket.rs},server.rs,fs.rs}` — Goose's HTTP/WS ACP server transport (case B reference).
14. Web research for adapter rename history (`@zed-industries/claude-code-acp` → `@zed-industries/claude-agent-acp` → `@agentclientprotocol/claude-agent-acp`) and Goose ACP roadmap.

Sources for adapter rename and ecosystem context:
- [@zed-industries/claude-code-acp on npm](https://www.npmjs.com/package/@zed-industries/claude-code-acp) — deprecated
- [@zed-industries/claude-agent-acp on npm](https://www.npmjs.com/package/@zed-industries/claude-agent-acp) — second-rename mirror
- [`@agentclientprotocol/claude-agent-acp` repo](https://github.com/agentclientprotocol/claude-agent-acp) — canonical
- [Zed docs: External Agents](https://zed.dev/docs/ai/external-agents) — refers to `claude-code-acp` (stale)
- [Goose ACP discussion](https://github.com/block/goose/discussions/4645) — adopt-ACP roadmap
- [Goose ACP-over-HTTP issue](https://github.com/block/goose/issues/6642) — rationale for HTTP transport
- [Xuanwo/acp-claude-code](https://github.com/Xuanwo/acp-claude-code) — TypeScript, archived 2025-09
- [agent-client-protocol crate docs](https://docs.rs/agent-client-protocol/0.11.1)
