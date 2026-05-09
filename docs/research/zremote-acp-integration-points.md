# zremote ↔ ACP integration points

**Branch:** `feature/acp` (no ACP code yet)
**Author:** zremote-architect (research only — no implementation)
**Date:** 2026-04-25
**Sister reports:** acp-spec, acp-ecosystem (parallel research)

---

## TL;DR

- zremote already runs an **agentic-loop pipeline** (process detection + PTY output analyzer + CC hooks). ACP would replace ad-hoc terminal-output scraping with **structured events from any ACP-speaking agent**, dramatically improving fidelity.
- Best landing zone: **(A) ACP client inside the GPUI app** — drives external agents (Claude Code, Codex, Gemini-CLI, Zed-style agents) over stdio per session. Highest user value, lowest blast radius on existing wire protocol.
- Second: **(B) ACP agent on `zremote-agent`** — exposes zremote's PTY/terminal infrastructure to external editors (Zed). Useful for multi-host but smaller user base.
- Third: **(C) Internal ACP between GUI and agent** — nice in theory but would duplicate the existing tagged-enum WS protocol with no current pain. Defer.
- Existing `AgentLauncher` trait + `LauncherRegistry` (RFC-003) is the natural place to plug ACP-speaking subprocesses. The trait already abstracts "kind → command builder + post-spawn hook".
- ACP **complements** MCP cleanly: MCP supplies tools to the agent; ACP supplies the agent loop to the host. They live on different wires.
- Worktrees (RFC-009) and project hooks (RFC-008) flow naturally into ACP's `cwd`, `fs/*`, and `terminal/*` extensions.
- Highest risk: **double protocols**. The CC-hooks sidecar (`crates/zremote-agent/src/hooks/`) emits structured loop events today; running both that and ACP for the same session would race. Adoption needs a per-launcher switch.
- Suggested phasing: ship ACP-client launcher for one external agent end-to-end (small slice, validates whole stack) → add `terminal/*` and `fs/*` providers → only then consider exposing ACP from `zremote-agent`.
- Open question: can the GPUI app spawn local stdio subprocesses on a **remote** host? The current `--server` mode goes through a WS-multiplexed bridge — ACP-over-WS would need framing. See "Risks" below.

---

## 1. Current architecture map

```
+-----------------------------+              +---------------------------+
|  zremote-gui  (GPUI app)    |  REST/WS     |  zremote-agent (per host) |
|                             | <----------> |                           |
| - sidebar / terminal panel  |              | - PTY sessions            |
| - cc_widgets, command pal.  |              | - bridge:: WS for GUI     |
| - zremote-client SDK        |              | - agentic:: detector +    |
+-----------------------------+              |   analyzer + manager      |
        ^                                    | - hooks:: (CC sidecar)    |
        |                  (server mode      | - claude:: launcher       |
        |                   only — proxy)    | - mcp::    knowledge srv  |
        |                                    | - channel:: (CC bridge)   |
        |                                    | - worktree:: service      |
        |                                    | - knowledge::, projects:: |
        |                                    | - local::  (axum router)  |
        |                                    +---------------------------+
        |                                              ^
+-------+----------------------+   WS /ws/agent       |
|  zremote-server  (multi-host)|<--------------------+
|  (lib used by                |   WS /ws/events
|   `zremote agent server`)    |   REST /api/...
+------------------------------+
```

### Crate responsibilities (load-bearing files)

| Crate | Role | Key entry points |
|---|---|---|
| `zremote-protocol` | Wire-format types, tagged-enum WS messages | `crates/zremote-protocol/src/lib.rs:1-30`, `terminal.rs:46-306` (AgentMessage / ServerMessage) |
| `zremote-core` | Shared DB / state / queries / validation | `crates/zremote-core/src/lib.rs`, `state.rs`, `processing/agentic.rs` (loop processing) |
| `zremote-client` | HTTP/WS SDK consumed by GUI | `crates/zremote-client/src/lib.rs:14-93` (re-exports protocol + SDK types) |
| `zremote-agent` | Runs on each host (local + server modes) | `crates/zremote-agent/src/lib.rs`, `local/mod.rs:40-328`, `connection/mod.rs:222-919` |
| `zremote-server` | Multi-host server (Axum) | `crates/zremote-server/src/lib.rs:51-313`, `state.rs` |
| `zremote-gui` | GPUI desktop app | `crates/zremote-gui/src/views/main_view.rs:39-79`, `cc_widgets.rs` |

---

## 2. Existing wire protocol catalogue

Everything goes through tagged-enum JSON via `#[serde(tag = "type", content = "payload")]` over WebSocket (or REST for sync calls).

### 2.1 Top-level messages (agent ↔ server)

`crates/zremote-protocol/src/terminal.rs`:

| Direction | Variant | Line | Purpose |
|---|---|---|---|
| A→S | `AgentMessage::Register` | `terminal.rs:48-57` | Initial handshake |
| A→S | `AgentMessage::Heartbeat` | `terminal.rs:58-60` | Liveness |
| A→S | `AgentMessage::TerminalOutput` | `terminal.rs:61-64` | PTY bytes |
| A→S | `AgentMessage::SessionCreated/Closed` | `terminal.rs:66-74` | Lifecycle |
| A→S | `AgentMessage::ClaudeAction(...)` | `terminal.rs:178` | CC-specific updates |
| A→S | `AgentMessage::ChannelAction(...)` | `terminal.rs:179` | Channel bridge replies |
| A→S | `AgentMessage::AgentLifecycle(...)` | `terminal.rs:181-182` | Generic launcher (RFC-003) |
| A→S | `AgentMessage::WorktreeCreateResponse` | `terminal.rs:144-152` | RFC-009 sync reply |
| A→S | `AgentMessage::DirectoryListing/...Settings...` | `terminal.rs:155-176` | Request/response pairs |
| S→A | `ServerMessage::SessionCreate/Close/Input/Resize` | `terminal.rs:194-220` | PTY control |
| S→A | `ServerMessage::AgentAction(StartAgent...)` | `terminal.rs:228-231` | Spawn-via-launcher (RFC-003) |
| S→A | `ServerMessage::ClaudeAction(StartSession...)` | `terminal.rs:226` | Legacy CC start |
| S→A | `ServerMessage::WorktreeCreateRequest` | `terminal.rs:265-278` | RFC-009 request |
| S→A | `ServerMessage::ContextPush` | `terminal.rs:298-305` | Inject memories/conventions |

### 2.2 Sub-protocols

- **Claude messages** — `crates/zremote-protocol/src/claude.rs:32-110`: `StartSession`, `DiscoverSessions`, `SessionStarted`, `SessionIdCaptured`, `MetricsUpdate`.
- **Channel messages** — `crates/zremote-protocol/src/channel.rs:8-136`: `Instruction`, `ContextUpdate`, `Signal`, `ChannelResponse::{Reply,StatusReport,ContextRequest}`, **`PermissionRequest`/`PermissionResponse`** — closest existing analogue to ACP `session/request_permission`.
- **Agentic messages** — `crates/zremote-protocol/src/agentic.rs:26-74`: `LoopDetected`, `LoopStateUpdate` (with `prompt_message`, `permission_mode`, `action_tool_name`, `action_description`), `LoopEnded`, `LoopMetricsUpdate`, `ExecutionNode`. **Direct conceptual overlap with ACP `session/update`** (text/tool-call/edit streams).
- **Agents (RFC-003)** — `crates/zremote-protocol/src/agents.rs:18-119`: `AgentServerMessage::StartAgent { profile: AgentProfileData }`, `AgentLifecycleMessage::{Started, StartFailed}`, `SUPPORTED_KINDS = [{ kind: "claude", ... }]`. **This is the seam ACP plugs into.**
- **Server events to GUI** — `crates/zremote-protocol/src/events.rs:91-267`: 24 variants including `LoopDetected/StatusChanged/Ended`, `ChannelPermissionRequested`, `ExecutionNodeCreated`, `ClaudeSessionMetrics`. The `Unknown` variant (`#[serde(other)]` at line 265) is the protocol's forward-compat lever.

### 2.3 Local-mode REST surface

`crates/zremote-agent/src/local/router.rs:17-307` — ~50 endpoints. Most relevant for ACP:

| Endpoint | Line | Role |
|---|---|---|
| `POST /api/agent-tasks` | `router.rs:212-215` | Profile-driven launch (generic, RFC-003) |
| `POST /api/claude-tasks` | `router.rs:218-221` | Legacy CC start |
| `GET /api/agent-profiles/kinds` | `router.rs:29-31` | What kinds are supported |
| `POST /api/sessions/{id}/channel/permission/{request_id}` | `router.rs:276-279` | **Existing permission-grant path** |
| `POST /api/sessions/{id}/context/push` | `router.rs:69-72` | Inject memories (analog of ACP `session/set_session_mode`?) |
| `GET /ws/terminal/{session_id}` | `router.rs:290-293` | Per-session PTY WS |
| `GET /ws/events` | `router.rs:294-295` | Server-event firehose |

---

## 3. Agentic-loop detection today

This is the **single most relevant existing feature** when evaluating ACP. zremote already does loop detection — the question is whether ACP can replace, augment, or coexist with each layer.

### 3.1 Three layers, in order of fidelity

**Layer 1 — Process detection (low fidelity, kind-agnostic):**
`crates/zremote-agent/src/agentic/detector.rs:11-59` — walks the BFS process tree under each session's shell PID, matches against `KNOWN_TOOLS = [("claude","claude-code"), ("codex","codex"), ("gemini","gemini-cli"), ("aider","aider")]`. Manager runs every second (`crates/zremote-agent/src/agentic/manager.rs:35-101`, interval `connection/mod.rs:83`). Output: `LoopDetected` / `LoopEnded`. Cannot tell working vs waiting vs done.

**Layer 2 — Output analyzer (medium fidelity, regex-based):**
`crates/zremote-agent/src/agentic/analyzer.rs:371-588` — `OutputAnalyzer` per session. Strips ANSI, parses **OSC 133 prompt markers** (line 666-691) and **OSC 7 cwd**, runs per-provider adapters in `agentic/adapters/{claude,codex,gemini,aider}.rs`. Emits `AnalyzerEvent::{AgentDetected, PhaseChanged, TokenUpdate, ToolCall, CwdChanged, NodeCompleted}` (`analyzer.rs:56-77`). Phase enum (`analyzer.rs:47-54`): `Unknown / ShellReady / Idle / Busy / NeedsInput`. Cost estimation lives in `agentic/patterns.rs::estimate_cost`.

**Layer 3 — Claude Code hooks (high fidelity, CC-specific):**
`crates/zremote-agent/src/hooks/handler.rs:124-261` is an HTTP sidecar (started per-connection, `connection/mod.rs:368-388`) that CC calls with structured payloads on `PreToolUse`, `PostToolUse`, `Stop`, `Notification`, `Elicitation`, `UserPromptSubmit`, `SessionStart`, `SubagentStart/Stop`, `FileChanged`, `CwdChanged`, `StopFailure`. **This is what ACP looks like, but for CC only.**

The `AgenticStatus` doc-comment at `crates/zremote-protocol/src/agentic.rs:6-12` is explicit:
> "Only hook-driven `WaitingForInput` / `RequiresAction` statuses are authoritative for user-facing notifications (Telegram, toasts)."

PTY-silence-derived `Idle` is a non-notifying fallback (`connection/mod.rs:144-189`). When hooks are active for a session the analyzer's phase events are suppressed (`hook_mode` check at `connection/mod.rs:144`).

### 3.2 What this stack can/can't do

Can: detect any of the four hard-coded tools, count tokens, summarise tool output, build execution-node history (`ExecutionNode` protocol message → DB → UI via `ExecutionNodeCreated` event).

Can't:
- Distinguish "tool is asking permission" from "agent is busy" without CC-specific hooks. Codex / Gemini / Aider all fall back to Layer 2 only.
- Show structured tool-call diffs (we get free-form text in `output_summary`, capped at 500 chars per `analyzer.rs:102`).
- Cancel a running agent's turn cleanly. Today this requires sending Ctrl-C bytes to the PTY.
- Stream token-by-token assistant text. Output is line-buffered through ANSI stripping.

ACP gives all four for any agent that speaks it.

---

## 4. Integration-point analysis

### (A) ACP client inside `zremote-gui`

**Use case.** User picks "Claude Code (ACP)" or "Codex (ACP)" from the launcher. The GUI spawns an external ACP-speaking subprocess (locally for standalone mode; over the agent for server mode), drives it directly with structured `session/prompt` calls, and renders streaming `session/update` events as native GPUI views (text, tool-call cards, file-diff hunks, permission modals). Existing terminal panel becomes the "fallback" for non-ACP agents.

**What changes.**

- New crate or module under `zremote-client` (or `zremote-acp/`) with the ACP wire types + a JSON-RPC stdio transport.
- `zremote-gui`: new view variant alongside `TerminalPanel` for ACP-driven sessions. Permission requests render as a modal (the `WaitingForInput` toast pipeline at `crates/zremote-gui/src/views/main_view.rs:34-79` is the place to dispatch from).
- `zremote-agent`: a new `AgentLauncher` impl (`crates/zremote-agent/src/agents/`) that, instead of returning a `LaunchCommand` for the PTY, spawns the agent as a child process and proxies stdio frames to/from the GUI. The trait would need an extension method (`spawn_acp` returning a transport handle) since today `AgentLauncher::build_command` returns shell text only (`crates/zremote-agent/src/agents/mod.rs:115-117`).
- Profile schema: `agent_profiles.settings_json` stays the same; new field `transport: "pty" | "acp"`. This is exactly the kind of evolution `settings_json` was designed for (RFC-003 §1).

**Compatibility.** Adding a new launcher kind (`"claude-acp"`) and a new optional `AgentMessage` variant for streaming ACP frames is straightforwardly forward-compatible per CLAUDE.md table:
- New variant + `#[serde(other)]` Unknown fallback ⇒ old servers ignore.
- New `transport` field with `#[serde(default)]` ⇒ old profiles default to PTY.

**Effort.** **L**. Wire types + stdio transport (S/M), GPUI views for streamed updates and permission modals (M), launcher integration with the existing registry + profile editor (M), per-host transport for server mode (M).

**Dependencies.** `zremote-protocol` (new variant for ACP frames in server mode), `zremote-agent::agents` (launcher trait extension), `zremote-gui::views` (new view + permission modal), `zremote-gui::notifications` (already there for waiting-for-input). Worktree integration (RFC-009) provides the natural `cwd` per session.

### (B) ACP agent inside `zremote-agent`

**Use case.** Zed (or another ACP-aware editor running on the user's laptop) connects to zremote's agent and asks it to act as the agent. The agent maps `session/prompt` to its existing PTY+CC stack (or a future first-class loop) and streams updates. This makes zremote a **remote-execution backend for any ACP client**, including non-zremote ones.

**What changes.**

- `zremote-agent` exposes a new transport endpoint (stdio if the editor spawns the agent as subprocess; or WS framed with JSON-RPC for the network case).
- A new module under `crates/zremote-agent/` that translates the agent's existing internal events (`AnalyzerEvent`, `AgenticAgentMessage`) into ACP `session/update` frames. The hooks pipeline (`hooks/handler.rs`) is the right place to fork — it already produces structured "tool started / waiting / completed" events per CC turn.
- `terminal/*` operations map onto zremote's PTY layer (`crates/zremote-agent/src/pty/mod.rs`) cleanly — zremote already has PTY create/output/resize/wait-for-exit.
- `fs/read_text_file` and `fs/write_text_file` need a new sandboxed file-IO module. Worktrees (`crates/zremote-agent/src/worktree/service.rs`) define the natural sandbox root.

**Compatibility.** Pure addition. Old GUIs continue to use the existing REST/WS surface; new editors use ACP. **Major caveat**: hooks-driven CC integration would need to either (a) be the canonical loop driver and emit ACP frames, or (b) be disabled for ACP-driven sessions to avoid double-emission.

**Effort.** **XL**. Bigger because zremote-agent has to faithfully implement an agent loop (not just observe one) — including conversation history, tool wiring, permission policy. Roughly: stdio/WS framing (M), session lifecycle (M), `session/prompt` → existing CC launcher with structured streaming (L), `terminal/*`+`fs/*` providers (M), permission mapping to `channel::PermissionRequest` (M), end-to-end test with a real Zed client (M).

**Dependencies.** Touches PTY, worktree, hooks, channel, claude, MCP — almost the whole agent. High blast radius.

### (C) Internal ACP between `zremote-gui` and `zremote-agent`

**Use case.** Replace (some of) the existing tagged-enum REST/WS protocol with ACP messages between GUI and agent.

**What changes.** Almost everything. `zremote-protocol` becomes a thin re-export of ACP types; `zremote-client` becomes an ACP client; `zremote-agent::local::router` becomes an ACP server. **The current protocol is deeply non-ACP-shaped**: 50+ REST endpoints, projects/knowledge/linear/worktrees/hooks/channels are all out of ACP's scope. ACP would only cover a slice.

**Compatibility.** **High risk.** Every protocol-compat rule in CLAUDE.md applies. We have a working tagged-enum protocol with established forward-compat patterns; replacing it with ACP would require a coordinated rollout across server, agent, and GUI versions.

**Effort.** **XL**, with most of the work being protocol churn rather than user-facing capability.

**Verdict.** Defer. There is no current pain that ACP-as-internal-bus solves. (A) and (B) deliver real user value without rewriting working code.

---

## 5. Overlap / conflict analysis

### 5.1 ACP vs current agentic-loop code

| zremote concept | ACP equivalent | Conflict / harmony |
|---|---|---|
| `AgenticLoopManager.check_sessions` (process scan) | `session/new` then track session lifetime explicitly | ACP gives explicit start; process-scan becomes redundant for ACP-driven sessions but still needed for non-ACP fallback. |
| `OutputAnalyzer::process_output` (stripping ANSI to derive phase) | `session/update` (typed) | ACP fully supersedes this for ACP-speaking agents. Keep analyzer for non-ACP terminals. |
| `hooks/handler.rs` (CC-specific) | ACP `session/update` + `session/request_permission` | Direct overlap. **Pick one per launcher** — CC could emit either, depending on profile. |
| `AgenticStatus { Working, WaitingForInput, RequiresAction, Idle, Error, Completed, Unknown }` (`agentic.rs:14-23`) | Implicit in ACP's tool-call states (pending, in_progress, completed, failed) plus `request_permission` | Map at the boundary — produce `LoopStateUpdate` from ACP frames so existing UI keeps working. |
| `ChannelAgentAction::PermissionRequest` (`channel.rs:127-131`) | ACP `session/request_permission` | Almost identical shape (request_id, tool_name, tool_input). Could share data model. |
| `ExecutionNode` (`agentic.rs:59-73`) | ACP `session/update` tool-call entries | Different granularity but bridgeable. |
| `LoopMetricsUpdate` (`agentic.rs:52-58`) | ACP `session/update` token-count fields | Bridgeable. |

### 5.2 ACP vs MCP

**No conflict, complementary.** Today `zremote-agent mcp-serve` (`crates/zremote-agent/src/mcp/mod.rs:30-73`) exposes `knowledge_search`, `knowledge_memories`, `knowledge_context` to whatever agent is running (CC consumes them via MCP). MCP supplies tools to the model; ACP supplies the agent loop to the host. An ACP-driven session would still load the existing MCP server for knowledge tools — no change needed. Confirmed by the code: MCP and ACP would touch different files.

### 5.3 ACP vs REST/WS

The 50-endpoint REST API in `local/router.rs` covers projects, worktrees, knowledge, linear, hooks, command-palette discovery, etc. **Almost none of it overlaps ACP.** ACP would sit beside, not replace, this API. `/ws/events` (server events) might be partially redundant with ACP `session/update` for ACP sessions, but the rest stays.

---

## 6. Risks

1. **Double-driver races.** If a CC session is ACP-driven *and* the existing CC-hooks sidecar is installed, both will emit loop-state events. Solution: the launcher knows the transport; the hooks sidecar should be skipped when transport is ACP. Touch point: `connection/mod.rs:368-388` and the per-session `hook_mode` flag at `connection/mod.rs:144`.
2. **Server-mode transport for ACP stdio.** Server mode multiplexes everything through `WS /ws/agent`. ACP stdio frames need a new `AgentMessage` variant (e.g. `AcpFrame { session_id, payload: serde_json::Value }`) so the agent-side process IO is tunneled to the GUI. This is straightforward but is a new wire variant.
3. **State explosion in `connection/mod.rs`.** This file is already 1000+ lines holding 8+ HashMaps (`session_analyzers`, `channel_dialog_detectors`, `channel_bridge`, `delivery_coordinator`, `session_writers`, ...). Adding ACP transports per-session needs care — likely a per-session struct refactor before ACP work, otherwise this file becomes unmaintainable.
4. **Version skew.** ACP is young; the spec moves. We need a version negotiation step in the GUI (sister report `acp-spec` should cover this). zremote's existing `#[serde(other)]` pattern handles unknown variants but doesn't help with method-name renames.
5. **Permission policy double-source.** zremote has `permission-policy` per-project (`crates/zremote-server/src/routes/channel.rs`, `local/router.rs:271-283`) and `ChannelDialogDetector` for auto-approve. ACP has its own permission_mode + `request_permission` flow. We need a single decision point per session that takes both into account.
6. **PTY ownership.** If the GUI talks ACP directly to a child process (case A), the agent's PTY+analyzer pipeline is bypassed. That's fine for ACP-aware agents but breaks the existing `ExecutionNode` history feed for that session. Need to decide whether ACP frames also produce execution nodes (probably yes, via a translator).

---

## 7. Recommended phasing

If we adopt ACP:

1. **Spike (1–2 weeks).** Pick one external ACP-speaking agent (per `acp-ecosystem`'s recommendation), get it running locally as a subprocess driven by the GPUI app. Stub the streaming UI as plain text. No protocol changes yet. Goal: validate JSON-RPC stdio transport + version negotiation.
2. **Phase 1 — ACP launcher (case A, local mode only).** New `AgentLauncher` impl with `transport: "acp"`. Profile editor gets a transport toggle. New GPUI view renders `session/update` streams with proper text/tool-call/permission cards. No server-mode yet. Touches: `agents/`, `local/routes/agent_tasks`, `gui/views/`.
3. **Phase 2 — server mode.** Add `AgentMessage::AcpFrame` variant to tunnel ACP through `/ws/agent`. Agent spawns the ACP child; frames are forwarded both ways.
4. **Phase 3 — `terminal/*` and `fs/*` providers.** Wire ACP's standardized terminal and file ops into zremote's PTY layer and a sandboxed file IO module. Worktree path becomes the FS root.
5. **Phase 4 — translator to existing UI.** ACP `session/update` → `AgenticAgentMessage::LoopStateUpdate` + `ExecutionNode`. This keeps the sidebar / activity panel / Telegram notifications working unchanged.
6. **Phase 5 (optional) — case B.** Expose ACP from `zremote-agent` to external editors. Only if (A) succeeds and there is demand from Zed users etc.
7. **Defer indefinitely.** Case C (internal protocol replacement).

---

## 8. Open questions for team-lead / user

1. **Which external agent first?** Claude Code over ACP, Codex, or Gemini-CLI? (Sister `acp-ecosystem` report should answer.) The choice affects how much we can dogfood — CC-over-ACP would let us replace the hooks sidecar end-to-end.
2. **Server-mode priority.** Is standalone (`gui --local`) ACP enough for v1, or must server mode work day one? Server mode is significantly more work.
3. **Coexistence policy with hooks sidecar.** When CC is launched with `transport: "acp"`, do we still install the hooks sidecar (defense in depth) or skip it (cleaner separation)? My recommendation: skip when transport is ACP, document the tradeoff.
4. **Scope of `terminal/*`.** ACP's terminal provider can run shell commands the agent requests. Do those land in a new zremote PTY session (visible in sidebar) or a hidden one? Sidebar visibility makes more user sense but conflicts with "this is the agent's tool, not a user session".
5. **Permission policy unification.** Should the existing `permission-policy` per project apply to ACP `request_permission` too? (Recommended: yes, treat ACP requests as identical to the channel `PermissionRequest` flow at the policy layer.)
6. **Backwards compat for CC tasks.** RFC-003 already deprecated `/api/claude-tasks` softly. Does ACP launch finally retire it, or do we keep three paths (legacy CC, generic launcher RFC-003, ACP)?

---

## 9. Appendix: file map for ACP work

If/when implementation starts, these are the files most likely to change:

- `crates/zremote-protocol/src/agents.rs` — extend `AgentProfileData` with transport, add ACP-specific kind metadata.
- `crates/zremote-protocol/src/lib.rs` — possibly new `acp` module mirroring `claude` / `channel`.
- `crates/zremote-protocol/src/terminal.rs:46-306` — new `AgentMessage::AcpFrame` / `ServerMessage::AcpFrame` variants for server-mode tunneling.
- `crates/zremote-agent/src/agents/mod.rs` — extend `AgentLauncher` trait with optional ACP-spawn method (or new sibling trait).
- `crates/zremote-agent/src/agents/` — new file `acp.rs` (or per-vendor: `claude_acp.rs`, `codex_acp.rs`).
- `crates/zremote-agent/src/local/routes/agent_tasks.rs` — branch on transport.
- `crates/zremote-agent/src/connection/mod.rs:368-388` — gate hooks sidecar install on transport.
- `crates/zremote-agent/src/connection/dispatch.rs` — new arm for `AcpFrame` server messages.
- `crates/zremote-gui/src/views/` — new view module for ACP streamed sessions; permission modal; integration with command palette + sidebar.
- `crates/zremote-gui/src/views/main_view.rs:34-79` — wire ACP `request_permission` into existing toast/notification flow.
- `crates/zremote-client/src/` — new module for ACP transport (or pull in an external `agent-client-protocol` crate if one exists — sister report covers this).

No new SQL migrations are required for v1: the `agent_profiles.settings_json` blob already accepts arbitrary kind-specific config (RFC-003 §1).
