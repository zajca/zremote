# ACP Integration Research — Synthesis

**Branch:** `feature/acp` (no implementation yet)
**Date:** 2026-04-25
**Sources:** see sister reports in this directory:
- [`acp-spec.md`](./acp-spec.md) — protocol specification deep-dive
- [`acp-ecosystem.md`](./acp-ecosystem.md) — clients, agents, SDKs, UX patterns
- [`zremote-acp-integration-points.md`](./zremote-acp-integration-points.md) — current zremote architecture and integration seams

This document is a *team-lead synthesis* of the three reports above. Read it first; drill into the sister reports for evidence and details.

> **Update 2026-04-25:** see [`driver-architecture.md`](./driver-architecture.md) for a refined target architecture in which ACP is one of several pluggable `SessionDriver`s (alongside PTY-only, CC-hooks, generic-ACP). That framing supersedes §3 and parts of §6 below; the rest of this document remains valid as evidence and protocol reference.

---

## 1. What ACP is, in one paragraph

The **Agent Client Protocol** is an open JSON-RPC 2.0 protocol (newline-delimited, stdio by default) for communication between an **IDE/UI** and a **coding agent** subprocess. It was incubated by Zed, has since been spun out to its own org (Apache-2.0), and is at v1 with stable Rust/TS/Python/Kotlin/Java SDKs (Rust crate `agent-client-protocol = 0.11.1`, 1.4M downloads). The protocol has converged: 6+ editor clients ship today (Zed, all JetBrains IDEs, Neovim plugins, Emacs, marimo) and the **ACP Registry** lists **27 agents** with one-click distribution metadata — Claude Code, OpenAI Codex CLI, Gemini CLI, GitHub Copilot CLI, Cursor, Cline, Goose, OpenCode, JetBrains Junie, Kimi, Qwen, Mistral Vibe, and more.

Mental model: **LSP is editor↔language. MCP is agent↔tool. ACP is editor↔agent.** They compose: a typical stack is `Editor —ACP→ Agent —MCP→ tool servers`, with the language server still riding LSP.

## 2. What it would bring zremote — concrete capabilities

### 2.1 Replace ad-hoc agentic-loop detection with structured events

Today zremote has a three-layer ad-hoc loop detector:
- **Layer 1**: BFS process-tree scan for the four hard-coded binaries (claude, codex, gemini, aider) — cannot tell working vs waiting (`crates/zremote-agent/src/agentic/detector.rs:11-59`).
- **Layer 2**: PTY output regex + OSC133 prompt markers — line-buffered, can't see structured tool calls or diffs (`crates/zremote-agent/src/agentic/analyzer.rs:371-588`).
- **Layer 3**: CC-specific HTTP hooks sidecar — high-fidelity but **only for Claude Code** (`crates/zremote-agent/src/hooks/handler.rs:124-261`).

ACP replaces layers 1+2 with a typed event stream **for any agent that speaks ACP**, regardless of vendor. Layer 3 becomes redundant for CC over ACP (the Claude Code adapter `@zed-industries/claude-agent-acp` already exists and is shipping).

### 2.2 Instant agent catalog (27+ agents, zero per-agent UI work)

By becoming an ACP client, the GPUI app immediately gains access to every registry agent through a single code path. No per-vendor scraping, no provider-specific adapters in `agentic/adapters/{claude,codex,gemini,aider}.rs`. Adding a new agent is a registry entry, not a code change.

### 2.3 Rich UX features that today are impossible

| Capability | Today in zremote | With ACP |
|---|---|---|
| Streaming text from the model | line-buffered PTY scrape | token-level `agent_message_chunk` |
| Tool calls | "tool name appeared in output" | typed `tool_call` with `kind`, `status`, `locations[]` |
| Diff preview | none | `{type:"diff", path, oldText, newText}` → multi-buffer review |
| Permission prompts | CC-only via hooks | typed `session/request_permission` for every agent |
| Plans / task lists | none | `sessionUpdate:"plan"` with priorities and statuses |
| Terminal embedding in tool cards | none | `terminal/*` provider, live tail in cards |
| Cancellation | send Ctrl-C bytes | `session/cancel` notification |
| Slash commands & session resume | CC-specific | uniform protocol across all agents |
| Session list / history | per-vendor logic | `session/list` + `session/load` / `session/resume` |

### 2.4 The zremote-unique value prop: ACP over the network

ACP today is **stdio-only** (HTTP/WS transport is a draft RFD). That's the protocol's biggest practical limitation: a Zed editor on a laptop cannot drive an agent that lives on a remote server box.

zremote already operates a bidirectional WebSocket tunnel between GUI and remote agents (`crates/zremote-protocol/src/terminal.rs:46-306`). Wrapping ACP frames inside an `AgentMessage::AcpFrame` variant turns zremote into the **first ACP transport that crosses machine boundaries** — the GUI on a laptop drives an agent running on the remote box, with `fs/*` and `terminal/*` operating on the remote filesystem and remote PTY natively. No sshfs, no tmux. This is essentially "Zed for remote hosts" with zero per-agent integration work.

## 3. The three integration shapes — pick (A) first

Three places ACP could land. Numbered by recommended order.

### (A) GPUI as ACP **client** — recommended first phase

**User-facing capability:** users pick "Claude Code (ACP)" / "Codex (ACP)" / etc. in the launcher. The app spawns the chosen ACP agent (locally for standalone, on the remote host for server mode), drives it directly, and renders structured events as native GPUI views.

**What plugs in where:** the existing `AgentLauncher` trait + `LauncherRegistry` from RFC-003 (`crates/zremote-agent/src/agents/mod.rs:94-227`) is exactly the seam ACP needs. Add a `transport: "acp"` flag to `AgentProfileData.settings_json` (no schema migration — the field is already a freeform JSON blob per RFC-003 §1).

**Effort:** **L** (Large; ~3–6 weeks). Wire types via `agent-client-protocol = "=0.11.1"`, GPUI views for tool-call cards / diff hunks / permission modals, launcher integration, profile editor toggle.

**Compatibility:** Forward-compatible — new launcher kinds and `#[serde(other)]` Unknown variants are already the safe-evolution pattern in the codebase.

### (B) `zremote-agent` as ACP **server** — second phase, optional

**User-facing capability:** a Zed editor (or any ACP client) running on the user's laptop connects to a zremote agent and uses it as the agent backend. zremote becomes a remote-execution provider for any ACP client, including non-zremote ones.

**Effort:** **XL**. Bigger because the agent has to *be* an agent loop, not just observe one — conversation state, tool wiring, full permission policy.

**Risk:** double-driver races between CC hooks sidecar and ACP for the same session. Needs a per-launcher transport gate at `connection/mod.rs:368-388`.

### (C) Replace internal GUI↔agent protocol with ACP — defer

The current 50-endpoint REST + tagged-enum WS protocol covers projects, knowledge, linear, worktrees, hooks, channels — most of which has no ACP equivalent. ACP would only cover a slice and replacing the wire is XL with no current user pain. Defer indefinitely.

## 4. Concrete use cases unlocked by (A)

1. **Multi-vendor agent launcher.** "Run this prompt with Claude Code / Codex / Gemini / Goose / Cline / Junie / Cursor / Kimi / OpenCode / etc." with no code changes per vendor.
2. **Native diff review on the remote project.** Agent emits `(path, oldText, newText)` per edit; GUI renders multi-buffer hunk-level accept/reject — and the file is on the *remote* host because `fs/write_text_file` is implemented agent-side.
3. **Agent following.** When the agent sets `tool_call.locations[]`, the GUI auto-jumps the terminal/file pane to that path:line — the user literally watches where the agent is reading.
4. **Permission gating UI.** Every destructive tool call surfaces a typed prompt with Allow once / Allow always / Reject options — driven by the same channel-permission UI we already have, but for any agent.
5. **Plan tracking sidebar.** `sessionUpdate:"plan"` becomes a checkable task list — exactly the kind of agentic-monitoring feature the existing `docs/rfc/agentic-monitoring.md` envisions, but built from typed events instead of regex scraping.
6. **Token-stream chat.** Real-time `agent_message_chunk` rendering in a dedicated view — no more "watch the terminal scroll".
7. **Cancellation that works.** `session/cancel` cleanly aborts the model + tool calls; we keep using `session/prompt` reply with `stopReason:"cancelled"` for state hygiene.
8. **Session resume across reconnects.** `session/load` (replay history) and `session/resume` (restore without replay) survive agent disconnects — directly relevant to the existing `feedback_daemon_ux.md` memory about agent disconnect/reconnect being broken today.

## 5. What it costs us (gaps a client must fill)

ACP defines schema and lifecycle, not UX. We'd build:

- **Subprocess plumbing**: spawn, env, cwd, signals, restart, stderr capture (the SDK helps; tokio process is straightforward).
- **stdio framing**: handled by the Rust SDK; we'd just wrap our WS tunnel for server mode.
- **fs/* handlers**: serve text-file reads/writes against the remote workspace, ideally with a worktree-rooted sandbox (RFC-009 gives us the natural FS root).
- **terminal/* providers**: map onto our existing PTY layer; we already have create/output/kill — need to add an `outputByteLimit` ring buffer and a `terminalId` namespace separate from our `session_id`.
- **Permission UX**: render `session/request_permission` and route the outcome — overlaps cleanly with the existing `ChannelAgentAction::PermissionRequest` flow at `crates/zremote-protocol/src/channel.rs:127-131`.
- **Diff/multi-buffer review UI**: this is the biggest UX build. ACP gives us `(path, oldText, newText)`; the hunk-level reviewer is ours to write. Zed has set the bar high here.
- **Agent picker**: pull `cdn.agentclientprotocol.com/registry/v1/latest/registry.json`, install via `npx` or signed binary, hook into the existing profile editor.
- **Session persistence**: decide how we store `sessionId`s and whether we replay (`session/load`) or restore silently (`session/resume`).
- **Auth UX**: `authenticate` is in the stable response shape; concrete auth payloads are still gated behind `unstable_auth_methods`. Fine to defer.

## 6. Risks (in priority order)

1. **`connection/mod.rs` is already 1000+ lines** with 8+ HashMaps of per-session state. Adding ACP transports without a per-session-struct refactor turns the file unmaintainable. Recommended: extract `SessionState` first.
2. **Double-driver races.** CC hooks sidecar + ACP-CC on the same session both emit loop events. Per-launcher `transport` gate must skip the hooks install when transport is ACP.
3. **Server-mode tunneling.** `/ws/agent` multiplexes everything. ACP needs a new `AgentMessage::AcpFrame { session_id, payload: Value }` variant (forward-compatible per CLAUDE.md, but a new wire variant is still risk surface).
4. **Spec churn.** ACP is young — `unstable_*` cargo features still gate session/list, session/close, session/resume at the type level. We must pin `agent-client-protocol = "=0.11.1"` and explicitly opt into the unstable features we need (recommend `unstable_session_resume` + `unstable_session_close`).
5. **Permission policy double-source.** zremote has per-project `permission-policy` (`crates/zremote-server/src/routes/channel.rs`) and `ChannelDialogDetector` for auto-approve. ACP has its own `request_permission`. We need a single decision point per session. Recommendation: route ACP requests through the existing channel-permission policy.
6. **PTY ownership in case (A).** When the GUI talks ACP directly to a child process, the agent's PTY+analyzer pipeline is bypassed. Existing `ExecutionNode` history feed breaks for ACP sessions unless we add a translator that emits `LoopStateUpdate` + `ExecutionNode` from ACP frames so sidebar/activity-panel/Telegram notifications keep working.
7. **`agentclientprotocol.com/libraries/rust` doc page is misleading.** It still shows `trait Agent` / `trait Client` from older blog sketches. The 0.11 SDK uses a role-typestate (`Client`/`Agent`/`Proxy`/`Conductor` structs + `Builder` + `HandleDispatchFrom<Counterpart>`). Verify against `docs.rs/agent-client-protocol/0.11.1` before writing examples.

## 7. Recommended phasing

| Phase | Scope | Effort | Goal |
|---|---|---|---|
| Spike | One external ACP agent (likely Claude Code via `@zed-industries/claude-agent-acp`) running locally as a subprocess driven by the GPUI app, plain-text streaming UI | 1–2 weeks | Validate JSON-RPC stdio transport + version negotiation + SDK ergonomics in our codebase |
| Phase 1 | ACP launcher in **local mode** only. New `AgentLauncher` impl, `transport:"acp"` on profile, GPUI view with proper text + tool-call + permission UIs | 3–4 weeks | One end-to-end ACP agent in the GUI, no server-mode complications |
| Phase 2 | **Server mode**. New `AgentMessage::AcpFrame` variant tunnels ACP through `/ws/agent`. Agent spawns the ACP child; frames forwarded both ways | 1–2 weeks | "ACP over the network" — the unique zremote value prop |
| Phase 3 | `terminal/*` and `fs/*` providers wired into zremote PTY + sandboxed file IO. Worktree path becomes FS root (RFC-009 alignment) | 2–3 weeks | Agents can run shell commands and edit files on the remote host through standard ACP, no per-agent shims |
| Phase 4 | Translator: ACP `session/update` → `AgenticAgentMessage::LoopStateUpdate` + `ExecutionNode`. Keep existing sidebar / activity panel / Telegram notifications working unchanged | 1–2 weeks | Backwards compatibility for all consumers of the loop-event firehose |
| Phase 5 | (Optional) **Case (B)**: expose ACP from `zremote-agent` for external editors (Zed, JetBrains, Neovim) | XL | Only if Phase 1–4 succeed and there is user demand |
| Defer | Case (C): replace internal protocol with ACP | — | No current pain; rewrite cost not justified |

## 8. Open questions for the user

1. **Which external agent first?** Claude Code via ACP would let us replace the CC hooks sidecar entirely. Codex / Gemini / Goose are alternatives and broaden vendor coverage faster but don't simplify existing code.
2. **Server-mode in v1 or v2?** Standalone-only ACP would ship faster but loses the unique remote-execution angle. Ordering of Phase 1 vs Phase 2 depends on this.
3. **CC hooks coexistence.** When CC is launched with `transport:"acp"`, do we still install the hooks sidecar (defense in depth) or skip it (cleaner separation)? Recommendation: skip and document.
4. **Scope of agent-driven terminals.** ACP's `terminal/create` runs commands the agent requests. Do those land in a new visible PTY session in the sidebar (transparency) or hidden (it's the agent's tool, not the user's session)? Recommendation: visible-but-tagged.
5. **Permission policy unification.** Should the existing per-project `permission-policy` apply to ACP `request_permission`? Recommendation: yes — single decision point.
6. **Backwards compat for legacy CC tasks.** RFC-003 already softly deprecated `/api/claude-tasks`. Does ACP launch finally retire it, or do we keep three paths (legacy CC, generic launcher, ACP)?

---

## 9. References

- Sister reports: [`acp-spec.md`](./acp-spec.md), [`acp-ecosystem.md`](./acp-ecosystem.md), [`zremote-acp-integration-points.md`](./zremote-acp-integration-points.md)
- Spec home: [agentclientprotocol.com](https://agentclientprotocol.com)
- Rust SDK: [`agent-client-protocol = 0.11.1`](https://crates.io/crates/agent-client-protocol)
- Reference repo: [`agentclientprotocol/agent-client-protocol`](https://github.com/agentclientprotocol/agent-client-protocol)
- Agent registry JSON: `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`
- Claude Code ACP adapter: [`@zed-industries/claude-agent-acp`](https://github.com/zed-industries/claude-agent-acp)
