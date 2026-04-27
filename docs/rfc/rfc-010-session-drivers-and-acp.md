# RFC 010: Session Drivers and ACP Integration

**Status:** Draft
**Author:** team-lead (synthesizing research from team `acp-research`)
**Date:** 2026-04-25
**Branch:** `feature/acp`
**Research basis:** [`docs/research/README.md`](../research/README.md), [`docs/research/acp-spec.md`](../research/acp-spec.md), [`docs/research/acp-ecosystem.md`](../research/acp-ecosystem.md), [`docs/research/zremote-acp-integration-points.md`](../research/zremote-acp-integration-points.md), [`docs/research/driver-architecture.md`](../research/driver-architecture.md), [`docs/research/acp-rust-reuse.md`](../research/acp-rust-reuse.md)

---

## 0. Terminology

ACP and zremote both use the words "client," "agent," and "session." They mean different things. This RFC resolves the collision as follows:

| Term | In this RFC means | Not to be confused with |
|---|---|---|
| **ACP Client** | The role on one side of the ACP wire that hosts the UI/files/terminals. In zremote that role is played by the **`SessionDriver` running inside `zremote-agent`**, never the GUI. | "client" in zremote sense (the GUI/CLI) |
| **ACP Agent** | The role on the other side of the ACP wire that runs the model loop. Always a **subprocess** (Claude Code adapter, Codex CLI, Gemini, …). | `zremote-agent` (the Rust crate / daemon process) |
| **ACP child** | The concrete subprocess implementing the ACP Agent role. Always lives on the same host as `zremote-agent`. | — |
| **`zremote-agent`** | Our Rust process running on each managed host. Plays the ACP Client role internally; speaks zremote's REST+WS to remote GUIs. | ACP Agent |
| **GUI / CLI** | The user-facing client (GPUI desktop app or `zremote` CLI). Speaks zremote's REST+WS only, **never ACP**. | ACP Client |
| **Driver** | An implementation of the `SessionDriver` trait owning a single session's lifecycle and producing canonical events. | ACP-specific concept; drivers may have nothing to do with ACP |
| **ACP session** | A single `session/new` lifecycle inside the ACP wire (agent-host-local). | zremote session (PTY + metadata, may or may not have an ACP child) |
| **zremote session** | Today's per-PTY session row in the DB; with this RFC, it owns one driver. May contain at most one ACP session. | ACP session |
| **Canonical event** | A `LoopStateUpdate` / `ExecutionNode` / `PermissionRequest` / `Plan` / `AgentMessageChunk` / `SessionEnded` event emitted by a driver and consumed by GUI / CLI / Telegram / DB. | Raw ACP `session/update` notification (which only the driver sees) |
| **Tunneling** | Forwarding raw ACP frames over zremote's WS. **This RFC explicitly does not do this.** | Canonical-event flow |

When in doubt, "ACP" prefix means inside-the-driver, "canonical" prefix means crosses-the-network.

---

## 1. Context & Problem

ZRemote needs first-class support for coding agents (Claude Code, Codex, Gemini, Cursor, etc.) and the agent ecosystem is consolidating around the **Agent Client Protocol (ACP)** — an open JSON-RPC protocol with stable Rust SDK, 27+ agents in a public registry, and 6+ editor clients shipping today (Zed, all JetBrains IDEs, Neovim, Emacs, marimo).

### What we have today

Per-session "what's running" is implicit and fragmented across `crates/zremote-agent/`:

- A PTY is always created.
- A process-tree scanner polls every 1 s looking for four hard-coded binaries (`claude`, `codex`, `gemini`, `aider`).
- An `OutputAnalyzer` per session strips ANSI, parses OSC 133 prompt markers, and runs vendor-specific regex adapters under `agentic/adapters/`.
- A Claude Code HTTP hooks sidecar may or may not be installed per connection, producing structured loop events but only for CC.
- `connection/mod.rs` (1000+ lines, 8+ HashMaps) ties it all together with conditional branches.

The result is a system that works for the four hard-coded vendors at varying fidelity — high for CC (via hooks), low for everyone else (regex on terminal output) — and that gets harder to extend with each new agent.

### What ACP would give us

Replacing the regex stack with structured events from any ACP-speaking agent, gaining:
- streaming model output (token-level), typed tool calls with diff and terminal embeds, plan tracking, permission gating, clean cancellation, session resume — for **any** agent that speaks ACP, not per-vendor;
- one-click distribution of 27+ agents via the ACP Registry;
- an unusual zremote-only opportunity: ACP today is stdio-only, so editors like Zed cannot drive a remote agent. zremote already operates a bidirectional WS tunnel to remote hosts. With our agent-side drivers, "ACP over the network" works through the existing event channel — effectively *Zed for remote hosts*.

### The architectural choice

Two ways to bring ACP in:

- **Bolt-on:** add an ACP launcher next to the current code paths (research synthesis §3, case A). Works, but leaves CC-via-hooks and ACP as parallel pipelines competing for the same session and grows `connection/mod.rs` further.
- **Refactor + integrate:** introduce a `SessionDriver` abstraction. Every session is owned by exactly one driver picked at start. ACP, CC-via-hooks, plain PTY, and future integrations are siblings, not branches.

**This RFC chooses the second option.** It is the connection-state refactor we already need *and* the foundation ACP plugs into.

---

## 2. Goals & Non-Goals

### Goals

- One per-session abstraction (`SessionDriver`) owning all "what runs and what it emits" decisions.
- A canonical event stream that downstream consumers (UI, DB, Telegram, channel bridge, permission policy) consume regardless of driver.
- Three driver implementations in v1: PTY (refactor of today's behaviour), CC-via-hooks (refactor), CC-via-ACP (new).
- A pluggable seam where adding a new ACP agent is a profile entry, not code in five places.
- Server-mode parity from the start: drivers run agent-side; canonical events flow over the existing WS event channel without new tunneling variants.
- Replace zero existing UX. Today's terminal panel keeps working unchanged for sessions that don't pick an ACP driver.

### Non-goals

- Exposing ACP from `zremote-agent` to external editors (case B from research). Possibly later, not v1.
- Replacing the internal GUI↔agent REST/WS protocol with ACP (case C). Defer indefinitely.
- Custom user-defined drivers loaded as plugins. All v1 drivers live in-tree under `zremote-agent`.
- Network transport for ACP itself. We do not ship our own HTTP/WS-ACP transport; we tunnel canonical events, not raw ACP frames.
- Migration of stored session histories. New driver fields are additive on profiles, no SQL migration.

---

## 3. Architecture

### 3.1 Where the seam goes

A new module `crates/zremote-agent/src/session_driver/` defines a single trait `SessionDriver` with an event stream and a small control surface. `connection/mod.rs` becomes a dispatcher: per session it owns one `ActiveDriver` instead of eight specialized HashMaps.

The trait's job is: given a start request, spawn whatever underlying processes the driver needs (PTY, ACP child, hooks sidecar, …), and produce a canonical event stream until the session ends. Control flows the other way for user input, permission decisions, and clean cancel.

### 3.2 The canonical event stream

The events drivers emit are intentionally close to what already exists today in `crates/zremote-protocol/src/agentic.rs` and `channel.rs`:

- raw output bytes for a terminal panel, with a `source` discriminator (`MainPty` for PTY-driver-owned shells and CC-hooks TUIs; `SidePty` for ACP drivers' side PTY per §4.6; `EmbeddedTerminal { tool_call_id, terminal_id }` for ACP terminal embeds);
- `LoopStateUpdate` — the existing `Working / WaitingForInput / RequiresAction / Idle / Error / Completed` state machine;
- `ExecutionNode` — one row in the existing tool-call history;
- `PermissionRequest` — same shape as today's `ChannelAgentAction::PermissionRequest`;
- new ACP-only signals: `Plan`, `AgentMessageChunk` (token streaming), `SessionEnded { stop_reason }`. PTY drivers leave these off.

Translation work — turning ACP `session/update` notifications into `LoopStateUpdate` and `ExecutionNode` — lives **inside the ACP driver**, not at the GUI boundary. This is the load-bearing decision: every consumer downstream (sidebar, Telegram, activity panel, DB persistence) keeps its current code unchanged.

### 3.3 Capability advertising

Each driver exposes a small set of capability flags (structured loop state, typed diffs, permission requests, streaming text, plan tracking, clean cancel, session resume, slash commands, embedded terminals). The GUI reads them at session start and decides which UI components to render.

Capabilities are **immutable for the lifetime of a session**. ACP itself does have dynamic notifications for slash commands and session modes, but those affect content within an already-advertised capability — they don't toggle the capability itself.

### 3.4 Driver selection

The order of precedence at start time:

1. **Explicit user choice** in the launcher — "Run with: Claude Code (ACP) / Claude Code (hooks) / Codex (ACP) / plain shell".
2. **Profile-level default** stored as `driver_id` inside `AgentProfileData.settings_json` (free-form JSON blob, no migration needed per RFC-003 §1).
3. **Built-in default per agent kind** — `claude` defaults to `cc-hooks` in v1, flips to `claude-acp` after a soak release.
4. **Plain `PtyDriver`** when the user just wants a shell.

This makes the migration a non-event: every existing session is implicitly using either `PtyDriver` (plain shell) or `ClaudeHooksDriver` (CC tasks), depending on what's already happening. No flag day, no schema change.

### 3.5 Server mode and remote access

#### 3.5.1 Cardinal rule

**ACP stdio JSON-RPC never travels over the network.** It lives entirely inside the agent host's process boundary, between the `SessionDriver` and the ACP child process. The GUI and CLI never speak ACP. They speak zremote's existing canonical event stream, which already crosses the network reliably with daemon-mode buffering, reconnection, and multiplex support.

This is the single most important architectural decision in this RFC. It makes server mode work without any new network-layer concepts: ACP-driven sessions are accessed by remote clients through exactly the same channels as PTY-driven sessions are today.

#### 3.5.2 Topology

```
+--------------+       +-----------------+       +------------------------------+
| GUI (laptop) | HTTPS | zremote-server  |  WS   | zremote-agent (remote host)  |
| or CLI       |   +   |  - auth, route  | /ws/  |  - SessionDriver (ACP)       |
|              |   WS  |  - session reg. | agent |    spawns + owns ACP child   |
| - launcher   |<----->|  - WS multiplex |<----->|  - PTY layer (terminal/*)    |
| - tool cards |       |    GUI<->agent  |       |  - worktree sandbox (fs/*)   |
| - diff view  | /ws/  |  - event fan-   |       |  - MCP server (host-local)   |
| - perm modal | events|    out          |       |       |                      |
| - plan side  |       +-----------------+       |       v                      |
+--------------+                                 |  +-----------------------+   |
                                                 |  | ACP child (subprocess)|   |
                                                 |  | (stdio JSON-RPC)      |   |
                                                 |  +-----------------------+   |
                                                 +------------------------------+
```

The ACP wire stays inside the right-hand box. Only canonical events cross the network.

#### 3.5.3 What flows over the GUI/CLI ↔ agent-host wire

Three categories of traffic, all on the existing protocol surface:

**Commands client → agent** (extending current `ServerMessage` variants):

- `StartAcpSession { profile, driver_id }` — picks a driver and spawns the child
- `Prompt { session_id, content_blocks }` — `@-mention` resolution and Resource block construction happens GUI-side before send
- `AnswerPermission { request_id, decision }` — same shape as today's channel-permission flow
- `Cancel { session_id }` — agent host translates to ACP `session/cancel` notification on the stdio channel
- `SwitchSessionMode` / `SetConfigOption` — for session modes / config options
- `LoadSession` / `ResumeSession` — after reconnect

**Canonical events agent → client** (extending current `AgentMessage` variants):

- `LoopStateUpdate` (existing) — Working / WaitingForInput / RequiresAction / Idle
- `ExecutionNode` (existing) — one tool-call row in history
- `PermissionRequest` (existing) — translated 1:1 from ACP `session/request_permission`
- `Plan` (**new**) — ACP-only, other drivers leave off; full entries list per spec
- `AgentMessageChunk` (**new**) — ACP-only, token-level streaming text
- `SessionEnded { stop_reason }` — translated from ACP `stopReason`

The two new variants follow the existing forward-compat pattern (new variants + `#[serde(other)] Unknown`) so older clients/servers ignore them.

**Bulk content fetch-on-demand** (extending REST surface):

Tool-call content can be large (10 MB diff, 50 MB terminal output). To keep events small:

- Inline up to ~256 KB directly in the canonical event.
- Larger content → reference `{ tool_call_id, content_index, size }`; client fetches via new endpoint `GET /api/sessions/{id}/tool-calls/{tcid}/content/{idx}` only when the user expands the card.
- Streaming output (embedded terminals) — driver maintains the ACP `outputByteLimit` ring buffer; client receives delta chunks.

This is the only REST-surface addition the RFC requires.

#### 3.5.4 ACP host-side methods (`fs/*`, `terminal/*`) execute on the agent host

The agent host is the ACP "client" from the protocol's POV, so when the ACP child calls back with `fs/read_text_file` or `terminal/create`, the request resolves locally with no network hop:

1. Driver receives the request over stdio from the ACP child.
2. Driver path-validates against the worktree sandbox root (RFC-009) and the project's `permission-policy`.
3. Driver invokes zremote's internal services:
   - `fs/*` → sandboxed file IO module; paths outside the worktree root are rejected.
   - `terminal/*` → existing PTY layer; new PTYs surface in the sidebar tagged as agent-spawned (§4.4).
4. Driver returns the result to the ACP child over stdio.
5. In parallel, the driver emits canonical events to the GUI/CLI so the user sees what the agent is doing — typically an `ExecutionNode` update with the diff content or `terminalId`.

**No round-trip to the remote client for fs/terminal.** This is the main argument for the driver-on-host model: if ACP frames travelled to the GUI and back, every `fs/read_text_file` would be two WAN round-trips per call.

#### 3.5.5 Daemon-mode and reconnection

ACP fits cleanly into today's daemon-mode session semantics:

- **GUI/CLI disconnect** → driver does not tear down. The ACP child keeps running on the agent host. Canonical events buffer in the existing event channel.
- **GUI/CLI reconnect** → client lists sessions via `GET /api/sessions`, opens `/ws/events`, receives the buffered tail.
- **Resume after agent restart** — if the ACP child survived, client emits `ResumeSession` and the driver calls ACP `session/resume` on the child (or `session/load` for replay from history when supported).
- **ACP child died during disconnect** — driver emits `SessionEnded { stop_reason: "shell_exit" }`; client surfaces a "Restart agent" banner.

#### 3.5.6 Credentials live on the agent host

ACP child authentication (API keys, OAuth tokens, …) lives in the agent host's secret/profile storage — the same mechanism CC tasks already use. The GUI is a control panel and never holds credentials. Side benefit: laptop loss does not leak agent secrets.

#### 3.5.7 MCP server propagation

ACP `session/new` accepts a list of MCP server configs the agent should connect to. In server mode, the agent host (which is the ACP "client") forwards configs from its local MCP configuration — both the bundled knowledge MCP server and any user-configured ones. The GUI does not need to know about MCP servers; they are a host-side concern.

#### 3.5.8 One driver code path across all three modes

The driver is identical regardless of where the GUI is:

| Mode | Communication |
|---|---|
| Standalone (`gui --local`) | Driver runs in the in-process agent spawn; events flow over an in-process channel |
| Local mode (`agent local`) | Driver on localhost agent; events flow over localhost WS |
| Server mode | Driver on remote agent host; events flow over the multiplexed WS through `zremote-server` |

This is the structural payoff of the driver framing: ACP is not a "server-mode feature" or a "standalone feature." It is a per-session driver, indifferent to where the session physically runs.

### 3.6 ACP terminals and files (host-side)

ACP defines client-side methods the agent calls back: `terminal/{create,output,kill,wait_for_exit,release}` and `fs/{read_text_file,write_text_file}`. In our model, the **driver** answers them — so they execute on the agent host.

- `terminal/*` maps onto the existing PTY infrastructure with two additions: an `outputByteLimit` ring buffer and a `terminalId` namespace separate from our `session_id`. Sub-terminals spawned by the agent become first-class but visually-tagged sessions in the sidebar (see UX §4.4).
- `fs/*` operates inside the worktree root from RFC-009. The worktree path is the natural sandbox boundary — paths outside it are rejected. Only text ops are stable in ACP today; binary access goes through MCP if needed (per spec §5).

### 3.7 Permissions

ACP's `session/request_permission` translates one-to-one into the existing `ChannelAgentAction::PermissionRequest`. The same per-project `permission-policy` that gates CC hook prompts gates ACP requests. **Single decision point per session.** The driver is the source-of-truth-translator; the channel layer is the policy layer.

### 3.8 Where `LauncherRegistry` and `AgentLauncher` (RFC-003) end up

Kept, repositioned. RFC-003's launcher trait was designed for "kind → command builder + post-spawn hook." That's exactly what `PtyDriver` and `ClaudeHooksDriver` need internally to spawn their underlying process. Drivers **use** the launcher; they don't replace it.

`AgentLauncher::build_command` does not need to grow an ACP variant. The ACP driver spawns its child directly with the agent's binary + args from the profile. Two seams remain: launchers for shell-based agents, drivers for the per-session lifecycle. They compose.

---

## 4. UX Decisions

### 4.1 Launcher picker

The new-task / new-session UI gains a single "Run with" dropdown:

- "Plain shell" (default, no agent)
- "Claude Code (hooks)" — current behaviour, stays the v1 default for CC profiles
- "Claude Code (ACP)" — opt-in in v1, default after soak
- "Codex (ACP)", "Gemini (ACP)", "Goose (ACP)", … — populated from the ACP Registry the first time the user installs an agent

**Why a dropdown rather than a separate button per agent:** keeps the launcher screen flat and lets the registry grow without UI changes. Profile-level defaults still apply; the dropdown is the override.

**Driver-mode hint text** appears below the dropdown when a non-default driver is selected. For "Claude Code (ACP)" specifically, the hint reads: *"Rich UI (typed cards, diffs, plans). TUI shortcuts (`ESC, ESC`, `/cost`) replaced by typed equivalents. Built-in shell pane for raw shell access."* For "Claude Code (hooks)": *"Full TUI compatibility. No streaming chat panel, no typed diffs."* These hints make the tradeoff explicit at the moment of choice (resolves a §9 fixable item — moves it from "documentation gap" to "UX surface").

### 4.2 What the GUI looks like for an ACP session

The terminal panel is still there, but for ACP sessions it shrinks to the bottom and a structured panel above it renders the canonical event stream:

- **Streaming chat** — `AgentMessageChunk` events appended live with markdown rendering. Capability flag `streaming_text` decides whether this panel shows up at all.
- **Tool-call cards** — one card per `ExecutionNode`. Status pill animates while in-progress. The `kind` (read/edit/execute/search/think/...) drives the icon. Locations link into the file tree.
- **Diff hunks inside tool cards** — when the tool call carries `(path, oldText, newText)`, a multi-buffer diff view opens with per-hunk accept/reject. **This is the biggest UX build** of the RFC and the main reason ACP sessions look qualitatively different from PTY sessions.
- **Plan list in the sidebar** — `Plan` events render a checkable task list with priorities. Updates replace the whole list (per spec).
- **Permission modal** — `PermissionRequest` becomes an inline prompt under the tool card with the agent's options ("Allow once / Allow always / Reject"). Same flow that today renders for CC channel prompts; the `permission-policy` per project still applies.
- **Embedded terminals** — when a tool call carries a `terminalId`, a small terminal widget renders inside the card with live tail. The widget keeps showing output even after the agent releases the terminal.

For non-ACP sessions, none of this appears — the panel falls back to a regular terminal. The driver's capability flags decide.

### 4.3 Capability degradation

Drivers below the bar still get a usable UI. The decision tree:

| Capability | If `false`, the GUI… |
|---|---|
| `structured_loop_state` | falls back to existing OSC133/regex-derived phase. Sidebar status becomes best-effort, not authoritative. |
| `typed_tool_calls` | hides tool-call cards entirely; tools surface only as terminal output. |
| `typed_diffs` | tool-card edits show as plain text patches, not multi-buffer diffs. |
| `streaming_text` | hides the chat panel; output flows only through the terminal panel. |
| `plan_tracking` | sidebar plan widget hidden. |
| `permission_requests` | no inline approval prompts; legacy channel-permission flow if available, otherwise none. |
| `clean_cancel` | cancel button sends Ctrl-C to the PTY. |
| `embedded_terminals` | tool-card terminal widgets fall back to "see terminal panel". |

**Why per-feature fallbacks rather than "ACP UI" vs "PTY UI":** lets `ClaudeHooksDriver` (which has structured loop state and permissions but no streaming text or diffs) light up most of the new UI without pretending to be ACP. The flags map naturally to the driver matrix.

### 4.4 Agent-driven terminals visibility

When an ACP agent calls `terminal/create`, the resulting PTY shows up in the sidebar tagged as agent-spawned, with the parent session's color and an arrow icon. Visible by default, hideable via a sidebar filter.

**Why visible:** transparency. The user should be able to see what the agent ran. Hidden agent terminals make the user blind to long-running side effects (a `cargo build` that's been running 20 minutes).

**Why tagged, not promoted:** these are tool calls, not first-class user sessions. Closing the parent session closes them; the user can't accidentally type into them.

### 4.5 Falling back when the agent crashes or the protocol breaks

ACP is young. Three kinds of failure to plan for:

1. **Agent process exits unexpectedly.** Driver emits `SessionEnded { stop_reason: "shell_exit" }` and surfaces a banner in the chat panel: "Agent crashed (exit code N). Logs in stderr." User can restart with one click.
2. **Protocol version mismatch at `initialize`.** Driver emits a one-line error event and the launcher tries to fall back to the next-best driver for that agent kind (e.g., CC-ACP fails → CC-hooks). Visible "fell back to ..." banner.
3. **Capability the GUI used isn't actually delivered** (agent advertised `plan_tracking=true` but never sends a plan). Not an error — UI just stays empty. No cleanup needed.

### 4.6 ACP drivers own a side PTY in the same session

When a user moves from `cc-hooks` (PTY-based, full TUI) to `claude-acp` (ACP child runs without a TUI), TUI shortcuts like `ESC, ESC`, `Ctrl-C`, prompt-history arrows, and `!`-prefix bash mode have no native channel into the ACP wire — the ACP child does not expose a PTY, and stuffing bytes into its stdin would break JSON-RPC parsing.

The cleanest fix is **architectural, not a sidecar**: the ACP driver provisions both an ACP child *and* a side PTY in the same session, owned by the same driver, sharing one `session_id`, one cwd (the worktree root), one permission scope, and one sidebar row. The side PTY is a regular shell (`$SHELL` in the worktree); CC does not run inside it. The two resources are independent on the wire — the PTY is for the user, the ACP child is for the model — but unified in the UI.

#### Why this beats a separate-session companion shell

- One sidebar row, not two — less visual noise, easier lifecycle.
- Same worktree by definition; no risk of drift between agent's view of files and shell's view.
- Permissions and credentials are shared automatically.
- Discoverability: the shell pane is right there in the ACP session view; the user does not have to know that "Open companion shell" exists.
- Decision 4 (one driver per session) holds without amendment — a driver may own multiple resources, just not multiple drivers per session.

#### UI layout

The ACP session view stacks:

```
┌────────── ACP Session ──────────┐
│ Chat panel (streaming text +    │  top, dominant
│ tool cards + diffs + permission │
│ prompts + plan if compact)      │
├──────────────────────────────────┤
│ [Plan list — right rail,         │  optional
│  collapsible]                    │
├──────────────────────────────────┤
│ Shell pane (live PTY in cwd,     │  collapsible,
│ ~1/3 height by default)          │  ~1/3 of session
└──────────────────────────────────┘
```

The shell pane is interactive with full keystroke fidelity for the shell. `ESC, ESC` works for the shell; for CC there is no `ESC, ESC` to interpret because CC does not run there. That is fine — CC-style cancel is reachable from the chat panel via the typed mapping below.

#### Three input channels in one session

The user has three ways to send something:

1. **Chat input** → typed `Prompt` over ACP (the model receives content blocks).
2. **Shell pane** → raw bytes to the side PTY (the shell interprets them; full TUI fidelity for `git`, `cargo`, `vim`, etc.).
3. **Quick-action buttons in the chat panel** — surface things ACP does not natively map. For example:
   - **"Cancel turn"** → `session/cancel` (the typed equivalent of CC's `ESC, ESC`).
   - **`/cost`, `/compact`, `/clear`** — match against `available_commands_update` from the agent; if not advertised, render as disabled with a tooltip explaining they are unavailable in ACP mode.
   - **"Run in shell"** mini-form → inject a command into the side PTY (with a confirm step for destructive-looking commands).

This set replaces the earlier "companion shell as separate session" idea — there is no separate session.

#### GUI keystroke mapping in the chat input

The GUI catches known TUI shortcuts in the chat input and translates them into typed ACP actions. The shell pane is unaffected (it always passes raw bytes).

| GUI keystroke (in chat input) | Translated to | Notes |
|---|---|---|
| `ESC, ESC` | `Cancel { session_id }` → ACP `session/cancel` | Cancels the active turn. |
| `Ctrl-C` during active turn | `Cancel` + clear any embedded terminal-output panels | Mirrors "interrupt and stop" intent. |
| `Ctrl-C` while idle | Clear chat input | GUI-only; never reaches the wire. |
| Up arrow at empty input | Replay last prompt | GUI keeps a per-session history store; ACP has no history protocol. |
| `/<command>` typed in input | Match against `available_commands_update`; if unknown, send as plain text | Adapter advertises slash commands per ACP §7.3. |

Raw bytes never reach the ACP child via this layer. By construction, the JSON-RPC stream is never corrupted.

#### Lifecycle of the two resources

- ACP child ends with `stopReason` → shell pane stays open (user may want to inspect).
- Shell exits → ACP child keeps running; user can re-open a fresh shell.
- Session close → both terminated.
- ACP child crashes → banner in chat panel, shell pane unaffected, "Restart agent" button.

#### Generalization across drivers

The shell pane is a **driver-level capability**, not a CC-specific feature. Every driver advertises whether it has one:

| Driver | Shell pane | Mechanism |
|---|---|---|
| `pty` | yes (implicit) | driver IS the shell |
| `cc-hooks` | yes (implicit) | CC TUI runs in the driver's PTY |
| `claude-acp` | yes (explicit) | driver provisions a separate side PTY |
| `codex-acp`, `gemini-acp`, `goose-acp`, `generic-acp` | yes (default on, opt-out per profile) | same as `claude-acp` |

Capability flag `shell_pane: bool` is advertised at session start. The GUI either renders the shell pane or it does not. The same widget renders for all drivers that have a PTY, regardless of why.

### 4.7 What does *not* change

- Existing terminal panel for non-ACP sessions: identical behaviour, same shortcuts, same rendering.
- Sidebar / activity-panel feed: identical, because drivers feed the same canonical events.
- Telegram and toast notifications: identical, same `LoopStateUpdate` source.
- Per-project `permission-policy`: identical, applies to all drivers.
- Worktree, project hooks, knowledge, MCP: untouched.

---

## 5. Phasing

| Phase | Scope | Deliverable | Effort |
|---|---|---|---|
| **P0 — Driver skeleton** | Define `SessionDriver` trait, canonical events, capability flags. Wire one driver — `PtyDriver` — that wraps existing PTY + analyzer. Refactor `connection/mod.rs` to dispatch through the driver. Build the golden-trace replay harness ourselves (no upstream test crate to lean on per Decision 23). **Pure refactor, no user-visible change.** | Connection state collapses from 8 HashMaps to 1. Foundation for everything that follows. | M |
| **P1 — ClaudeHooksDriver** | Move ownership of `hooks/handler.rs` into a driver impl. The "should I install hooks?" decision becomes "did I instantiate `ClaudeHooksDriver`?" No new behaviour. | Two HashMaps deleted from `connection/mod.rs`. CC sessions look identical. | S |
| **P2 — ClaudeAcpDriver (spike → ship)** | Add deps `agent-client-protocol = "=0.11.1"` and `agent-client-protocol-tokio = "=0.11.1"` with `features = ["unstable"]`. Spawn `@agentclientprotocol/claude-agent-acp@<pinned>` via `AcpAgent::from_str(...).with_debug(...)` (Decision 19). Vendor Zed's `handle_session_update` translator pattern (Decision 21). Advertise the `_meta` capability keys (`terminal_output`, `terminal-auth` per Decision 22). New launcher entry "Claude Code (ACP)" in the GUI alongside the existing one. CC remains usable via hooks; user picks. | First user-facing ACP capability. Effort dropped from L to M because `-tokio` saves the subprocess plumbing and Zed's translator gives us the SessionUpdate match-arms. | M |
| **P3 — GenericAcpDriver + Registry** | Reuse the `AcpAgent::from_str` plumbing from P2; parameterize the command via `AgentProfileData`. Pull the [ACP Registry JSON](https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json) for one-click agent install in the profile editor. Document the fallback chain for renamed npm packages (Decision 20). | Codex, Gemini, Goose, Junie, Cline, Cursor, OpenCode, Kimi, Qwen, etc. light up at once. | M |
| **P4 — Rich UI features** | Tool-call cards, multi-buffer diff review with hunk accept/reject, plan list in sidebar, permission modal, embedded-terminal widget. All driven by capability flags so PTY/CC-hooks sessions don't see them and ACP sessions get the full treatment. See §13 for sub-phase breakdown and decisions. | The qualitative UX leap. Reusable across all ACP drivers. | L |
| **P5 — Host-side `terminal/*` and `fs/*` providers** | Implement ACP's host-side methods on the agent. Worktree from RFC-009 becomes the FS sandbox root. PTY layer gains `outputByteLimit` ring buffer and `terminalId` namespace. Implement `tool_call.meta.terminal_info` pre-handle for Zed-style terminal embeds (Decision 22). | "ACP over the network" is now user-visible: agents run shell commands and edit files on the *remote* host through standard ACP, no per-agent shims. | M |
| **Defer** | ACP-as-server (case B in research): expose ACP from `zremote-agent` to external editors. Internal protocol replacement (case C). | Possible later. Not v1. | XL each |

P0 is the hard gate — it lands a refactor with full behavioural parity before any new feature builds on it. P1 is small but unblocks deleting CC-specific branching from shared code. P2 is the proof of value; if P2 ships and feels right, P3–P5 are mechanical extensions.

---

## 6. Decisions Made (with reasoning)

This section captures the load-bearing choices and why we picked them. Implementation should treat these as defaults; any reversal needs a new RFC or amendment.

1. **`SessionDriver` is in the agent crate, not the protocol crate.**
   *Why:* drivers spawn local processes, manage PTYs, talk to ACP children, hold async state. None of that belongs over the wire. The protocol crate stays a pure types crate.

2. **Drivers run agent-side; the GUI sees only canonical events.**
   *Why:* this makes server-mode parity automatic. No new `AgentMessage::AcpFrame` tunneling variant; no proxying raw protocol frames. It also keeps ACP's stdio-only constraint (the `agent-client-protocol` crate is tokio + stdio) on the host that has the child process — there's no need to make ACP travel.

3. **Translation from ACP → canonical events lives inside the driver.**
   *Why:* every existing consumer (sidebar, DB, Telegram, channel bridge) stays untouched. If we pushed translation up to the GUI, every consumer would need an "is-this-ACP?" branch.

4. **Capabilities are immutable per session, set at start.**
   *Why:* it's what ACP semantically gives us, and "capabilities can change mid-session" would force every UI consumer to reactively re-render. ACP's mid-session signals (slash commands, session modes) are *content within* an advertised capability, not capability changes.

5. **Permission policy is single-source: the channel layer.**
   *Why:* we already have a per-project `permission-policy` plus auto-approve detector for channels. ACP `request_permission` translates into `ChannelAgentAction::PermissionRequest` and goes through the same path. Two policy engines per session is a footgun.

6. **`AgentLauncher` (RFC-003) is kept and used by drivers, not replaced.**
   *Why:* the launcher does "kind → command + env + cwd," which is exactly what `PtyDriver` and `ClaudeHooksDriver` need internally. Drivers compose with launchers; reverting RFC-003 would be churn for no gain.

7. **Default driver for `claude` profiles in v1: `cc-hooks` (existing behaviour).**
   *Why:* hooks are the proven path. ACP-CC is opt-in in v1, soaked for at least one minor release, then promoted to default. No flag day for existing users.

8. **No SQL migration in v1.** The `driver_id` field lives in `AgentProfileData.settings_json`, which is already a free-form JSON blob (RFC-003 §1).
   *Why:* avoids schema churn. Adding `driver_id` later as a first-class column is reversible.

9. **One new optional `AgentMessage` variant for the ACP-only events (`Plan`, `AgentMessageChunk`); no `AcpFrame` tunneling.**
   *Why:* preserves the existing tagged-enum + `#[serde(other)] Unknown` forward-compat pattern. Old servers ignore the new variant; new servers use it.

10. **Agent-spawned terminals are visible in the sidebar by default.**
    *Why:* transparency for long-running side effects. Tagged so the user can tell them apart from interactive sessions.

11. **Capability flags drive UI degradation per feature, not per driver.**
    *Why:* keeps `ClaudeHooksDriver` (structured loop state + permissions but no diffs or streaming text) eligible for most of the new UI, without lying about being ACP.

12. **No custom user-defined drivers as plugins in v1.**
    *Why:* trait + dyn registration adds ABI/version pinning concerns we don't need to take on yet. All drivers in-tree.

13. **Bind `agent-client-protocol = "=0.11.1"` and `agent-client-protocol-tokio = "=0.11.1"` exactly, with the `unstable` feature group.**
    *Why:* ACP is moving and the SDK still gates "stabilized" methods (`session/resume`, `session/close`, `session/list`, etc.) behind unstable cargo features at the type level. The actual feature is `unstable` (forwards all the granular flags) — Zed itself pins exactly this. Adding `-tokio` gives us the subprocess+stdio plumbing for free (Decision 19). Pinning exact version + explicit feature opt-in beats surprise breakage.

14. **ACP wire stays inside the agent host's process boundary.**
    *Why:* §3.5 cardinal rule. The agent host plays the ACP Client role; the GUI/CLI never speak ACP. fs/* and terminal/* execute locally where files and PTYs live. Tunneling ACP frames over WAN would force two extra round-trips per host-side method call.

15. **Bulk content uses an inline-or-fetch threshold, not a streaming protocol.**
    *Why:* tool-call content can be 10+ MB (large diffs, build logs). Inline up to ~256 KB in canonical events; above that, send a reference and let the GUI fetch on demand via REST. Keeps event channel small without a new streaming format. Threshold is a configuration knob, not a wire-protocol decision.

16. **Credentials for ACP children live on the agent host, not in the GUI.**
    *Why:* the ACP child process runs on the agent host, so its API keys / OAuth state must be there. Side benefit: laptop loss does not leak agent credentials. Reuses today's profile/secret storage.

17. **MCP server propagation is automatic for bundled servers, opt-in per profile for user-configured ones.**
    *Why:* the bundled knowledge MCP server is always wanted; user-configured servers may have side effects the user doesn't want auto-applied to every ACP session. Explicit opt-in keeps the principle of least surprise.

18. **Reconnect uses `session/resume` by default, falls back to `session/load` only after agent restart.**
    *Why:* ACP `resume` restores context without replay (cheap when state is in memory). `load` replays the full history as `session/update` notifications (expensive but the only correct behaviour after process restart). The agent host knows which case it's in by checking whether the ACP child still has a live PID.

19. **Use `agent-client-protocol-tokio` for child-process plumbing; do not roll our own.**
    *Why:* the crate ships `AcpAgent::from_str(...).with_debug(...)` which already covers subprocess spawn, line-framed stdio, stderr capture/tee, kill-on-drop (`ChildGuard`), and the `select!` between protocol future and child exit. Reuse research measured this saves ~250–400 LoC vs. building it ourselves and removes a class of bugs around child cleanup. P2 effort drops from L to M as a result. The convenience constructors (`AcpAgent::zed_claude_code()` etc.) hardcode deprecated npm names; we use `from_str` with our own command line (Decision 20).

20. **Spawn `@agentclientprotocol/claude-agent-acp` for the Claude Code adapter; do not use the SDK's `zed_claude_code()` constructor.**
    *Why:* the adapter has been renamed twice — `@zed-industries/claude-code-acp` (deprecated) → `@zed-industries/claude-agent-acp` (npm mirror) → `@agentclientprotocol/claude-agent-acp@0.31.0` (canonical). The Rust SDK's convenience constructors still hardcode the deprecated name. We pin the canonical name explicitly via `AcpAgent::from_str("npx -y @agentclientprotocol/claude-agent-acp@<pinned-version>")`, with a documented fallback to the npm mirror for environments behind older registries.

21. **Vendor (with attribution) two specific patterns from Zed's open-source `acp_thread` and `agent_servers` crates (Apache-2.0).**
    The two are: (a) the `handle_session_update` translator match-arms at `zed/crates/acp_thread/src/acp_thread.rs:1428-1504`, including the user-chunk dedup logic and provisional-title handling; (b) the `AcpConnection::stdio` task-ownership pattern at `zed/crates/agent_servers/src/acp.rs:526-902` showing four `Task<()>` fields (`io_task`, `dispatch_task`, `wait_task`, `stderr_task`) on the connection struct.
    *Why:* these match-arms encode a year of integration learnings against real agents (CC adapter, Codex, Gemini). Re-deriving them from spec alone would mean rediscovering the same edge cases. Zed-specific bits (GPUI `Entity<...>` types, workspace abstractions) are stripped during vendoring. License compliance: add `LICENSES/Apache-2.0-zed.txt`, prefix vendored files with `// Adapted from zed-industries/zed at <commit>, Apache-2.0.`, add workspace `NOTICE` file.

22. **Replicate Zed's undocumented `_meta` interop conventions for terminal embeds and capability extensions.**
    *Why:* the canonical `claude-agent-acp` adapter checks for `clientCapabilities._meta.terminal_output: true` and `clientCapabilities._meta["terminal-auth"]: true` — without these meta keys, terminal output streaming inside tool calls **does not work**, and there is no fallback. Tool-call terminal embeds also use `tool_call.meta.terminal_info` rather than a first-class spec field. Neither is documented in the ACP spec; both are read from Zed's source. Our `InitializeRequest` advertises both meta keys; our translator handles `tool_call.meta.terminal_info` via a pre-handle hook before the standard `handle_session_update` dispatch. **Without this, we don't interop with the largest agent in the registry.** Captured as a P2/P5 requirement, not a P4 nice-to-have.

23. **`agent-client-protocol-test` is unpublished (`publish = false`); P0's parity test harness is hand-built.**
    *Why:* an earlier optimistic framing assumed we could ride on the SDK's test crate. The crate is internal to the SDK workspace (verified 404 on crates.io) and contains only mock JSON-RPC types for SDK doctests, not a parity harness. P0 must build its own golden-trace replay against captured PTY+analyzer outputs, as planned in §11.1.

24. **ACP drivers own a side PTY in the same session, not a separate companion session.**
    *Why:* §4.6 cardinal rule. Decision 4 says *one driver per session*, not *one resource per driver* — a driver may own multiple resources (an ACP child and a side PTY) provided it surfaces them as one session to the rest of the system. Earlier framing (separate companion shell session) was rejected because it added a sidebar row, complicated lifecycle management, risked cwd/permission drift between the two sessions, and obscured a feature most users would benefit from. The unified design also generalizes: every driver advertises a `shell_pane` capability, and every driver that has one (PTY, CC-hooks, ACP) renders the same widget in the GUI.

25. **Driver control surface separates typed prompt from raw PTY input.**
    *Why:* a consequence of Decision 24. `DriverControl::send_prompt(content_blocks)` carries typed ACP prompts (or analogous typed input for hooks-driver chat). `DriverControl::send_pty_input(bytes)` carries raw bytes for any of the driver's PTYs (main or side, addressed by `PtySource`). PTY-only drivers expose only the second; ACP drivers expose both. Keeping them as distinct methods avoids an enum-discriminator dance at every call site and makes the typed-vs-raw distinction self-documenting.

---

## 7. Risks

1. **Trait-design tax.** `DriverEvent` and `DriverCapabilities` need to cover both PTY and ACP without becoming a mush. Wrong shape and every UI consumer ends up branching on driver kind, defeating the point. Mitigation: P0 lands the trait *with `PtyDriver` only*, before any ACP code. If the trait feels wrong, we change it then.

2. **`connection/mod.rs` refactor scope.** Touching the busiest file in the agent. Mitigation: P0 must be parity-testable with captured traces before merge; CI runs golden replays.

3. **ACP version churn.** SDK is 0.11.x with unstable features for things the spec calls "stable." Mitigation: pin exact version, opt into unstable features explicitly, document in `Cargo.toml`. Re-evaluate every minor release.

4. **Diff-review UI is a real build.** Multi-buffer with hunk-level accept/reject is the largest UX item. Mitigation: P4 phase reflects this; don't conflate P2/P3 (transport works) with P4 (UI is great).

5. **`fs/*` sandbox correctness.** ACP `fs/write_text_file` could in principle let an agent write outside the worktree if we're sloppy. Mitigation: hard path-validation against the worktree root; reject anything that resolves outside. P5 owns this; security-reviewer must sign off before merge.

6. **Server-mode ACP child process management.** Restart semantics, env vars, cwd, signal handling on the agent host need to work the same way RFC-003 launchers work. Mitigation: drivers reuse the launcher utilities for child-process plumbing.

7. **`@zed-industries/claude-agent-acp` is npm-distributed.** We need Node.js on agent hosts to run it. Mitigation: documented dependency; future binary distribution via the registry's `binary.<platform>` mechanism.

8. **Capability advertising drift.** An agent advertising `plan_tracking=true` but never sending a plan is fine; advertising `false` and sending plans anyway is not. Mitigation: drivers ignore protocol events for capabilities they advertised as off; warn-log on mismatch.

9. **Bulk content storms over WAN.** A single `cargo build` tool-call could push 50 MB of terminal output across the event channel before any throttling kicks in. Mitigation: per-event 256 KB inline cap (Decision 15) plus the `outputByteLimit` ring buffer required by ACP's `terminal/create` spec. Hard cap on total per-tool-call accumulated bytes (configurable, default 50 MB) with truncation indicator after.

10. **ACP child auth lifecycle on agent host.** API keys may rotate; OAuth tokens expire. The agent host must surface re-auth requests to the user (who is on the GUI). Mitigation: ACP `authenticate` request from child → driver → canonical `PermissionRequest`-style event surfaced in GUI as inline auth prompt. Auth completes agent-side, GUI never sees the credential. (Mostly future work — `unstable_auth_methods` blocks v1.)

11. **Sandbox bypass via symlinks or relative paths.** A maliciously-crafted prompt could try to make the agent emit `fs/write_text_file` to `../../../etc/passwd`. Mitigation: canonicalize the path, check it's a descendant of the worktree root *after* canonicalization. Reject symlinks pointing outside. P5 owns this; security-reviewer must sign off.

12. **GUI/CLI version skew.** Old GUI connecting to new agent that emits `Plan` / `AgentMessageChunk` variants. Mitigation: `#[serde(other)] Unknown` variant in `AgentMessage` discards them gracefully. New GUI connecting to old agent: capability flags advertise `plan_tracking=false` etc., GUI hides those panels. Both directions handled by the existing forward-compat pattern.

13. **De-facto-spec drift via undocumented `_meta` extensions.** Zed has effectively become the reference implementation by inventing `_meta` keys (`terminal_output`, `terminal-auth`, `tool_call.meta.terminal_info`) that mainstream agents now require. Future agents may invent new ones we don't know about until something silently doesn't work. Mitigation: monitor Zed's `agent_servers` crate at every release for new meta keys; track in a checklist file (e.g., `docs/research/acp-zed-meta-tracking.md`); adopt new keys reactively rather than waiting for spec updates. Reach out to upstream working group to push for spec-level standardization.

14. **npm-package name churn for adapters.** The Claude adapter has been renamed twice in <1 year. Other adapters likely will be too. Mitigation: pin canonical org name (`@agentclientprotocol/...`), document fallback to historical names in the profile editor, surface a startup warning when spawn falls back.

15. **Zed-vendored translator drifts from upstream.** Vendoring `handle_session_update` (Decision 21) means we now own a copy that doesn't auto-update with Zed's fixes. Mitigation: track Zed's `acp_thread.rs` between releases (we re-pull on each ACP minor bump); add prominent comment in the vendored file linking to upstream commit. Any change to upstream's translator is reviewed for our copy in the same PR that bumps the SDK pin.

---

## 8. Open Questions

The following items still need a decision from the user before implementation starts. Recommendations are the team-lead's default; the user can override.

1. **Default driver flip schedule.** When does `claude` profile default switch from `cc-hooks` → `claude-acp`? Options: (a) one minor release of soak with no regressions, (b) measurable parity metric (e.g., loop-state event fidelity), (c) explicit user opt-in via setting forever. Recommendation: (a) — one minor release.
2. **Long-term coexistence of `cc-hooks` and `claude-acp`.** If both work indefinitely, do we keep them both or remove `cc-hooks` after N releases? Recommendation: keep both for at least one major version after the default flip; remove only if `claude-acp` proves strictly better on every dimension.
3. **Auth UX for hosted agents in P3.** GitHub Copilot CLI / Cursor / Cline need OAuth or API keys. `unstable_auth_methods` blocks in-protocol auth in v1. Options: (a) ship P3 with "agent's own CLI handles auth externally before zremote sees it," (b) wait for `authenticate` to stabilize. Recommendation: (a). Document the dependency in the profile editor.
4. **Bulk content threshold tunability.** The 256 KB inline cap is a default. Per-project override? Per-tool-call kind override (e.g., diffs always inline, terminal output always referenced)? Recommendation: workspace-global setting with a sensible default; revisit if user feedback demands more granularity.
5. **Agent-spawned terminal cleanup.** When the parent ACP session ends, what happens to terminals the agent created via `terminal/create`? Spec says `terminal/release` is required cleanup. If agent crashes mid-turn before releasing, do we kill orphaned PTYs immediately or keep them for the user to inspect? Recommendation: keep for inspection, surface in sidebar with a "released" status; auto-kill after a configurable timeout (default 1 hour).
6. **Driver lifetime when ACP child exits.** ACP child sends `stopReason: "end_turn"` and the user has not closed the GUI panel. Does the driver hold the resources (PTY allocation if any, MCP server connections) for a possible follow-up `session/prompt`, or tear down? Recommendation: hold for follow-up. The user closing the panel triggers full teardown.
7. **Telegram and external notification mapping for ACP-only events.** `Plan` and `AgentMessageChunk` are new. Does the existing Telegram bot route them somewhere meaningful, or only consume `LoopStateUpdate` as today? Recommendation: keep Telegram on `LoopStateUpdate` only; new event types are GUI-rich-UI material.

The following were considered open earlier and are now resolved by §3.5 + Decisions 14–18:
- *MCP servers in `session/new`* → Decision 17 (auto for bundled, opt-in for user-configured)
- *Session resume policy* → Decision 18 (`resume` if child alive, `load` if restarted)
- *Headless / standalone mode parity* → §3.5.8 (one driver path across all three modes)

---

## 9. Tradeoffs vs. ACP-native GUIs (Zed, JetBrains)

The driver-on-host model means the GUI sees a curated, normalized view of an ACP session, never the raw protocol. This buys us multi-host / daemon / server mode for free; it costs us things ACP-native clients get out of the box. Honest accounting:

### Inherent (cannot be fixed without changing the model)

1. **`fs/read_text_file` cannot return unsaved editor buffers.** Spec §5.1 expects the client to return open dirty buffer content rather than disk. Zed does this; we cannot, because the user's editor is on the laptop and zremote isn't an editor. Agent always sees disk state on the remote host.
2. **No agent-following live cursor.** When a tool call carries `locations[]`, Zed moves its editor cursor to that path:line live. We render clickable links instead; there's no editor to follow.
3. **Edits don't flow through the user's undo stack.** `fs/write_text_file` lands directly on the agent host's disk. Undo for an agent edit is `git revert`, not Ctrl-Z.
4. **No LSP context in tool-call rendering.** Zed has local language servers, so diffs render with full LSP awareness. We can ship tree-sitter syntax highlighting from the agent host, but cross-symbol context is out of reach.
5. **Streaming-text latency is WAN-bound.** Token streaming feels slower than local (50–150 ms RTT typical). Throughput is fine; the *feel* is different.

### Fixable, but as ongoing UX work

6. **Forward-compat lag.** A new ACP feature in upstream means a four-step rollout for us — bump SDK pin → extend driver translator → add canonical event variant if needed → extend GUI rendering — vs. one step for Zed. Expect to be one minor release behind upstream.
7. **Translator fidelity loss for new content types.** When ACP adds, say, an "embedded notebook" tool-call content type, we have to decide: add it to `ExecutionNode`, drop it, or tunnel as opaque. Each new content type is a small design decision rather than free.
8. **Custom ACP extensions silently dropped by default.** `_zed.dev/*` methods and `_meta` fields are for vendor-specific data. Our translator strips them. Adding generic `_meta` passthrough is doable but extra work.
9. **Slash command UI, mode picker, config-option picker, image/audio rendering** all need to be built. Zed has these polished. Scoped to P4.
10. **No profile-level tool permissions.** Zed has granular per-tool permissions for its first-party agent; for external agents only runtime prompts. We have no first-party agent at all — runtime prompts via channel-permission flow are our only level.

### What zremote gains in exchange

Listed for balance — this is why we accept the costs above:

- ACP over the network (stdio-only protocol now reaches remote hosts via canonical events)
- Agent runs where the project lives — no file sync, no sshfs
- Multi-host fan-out from a single GUI/server
- Daemon mode survives laptop close/reconnect
- Credentials stay server-side
- Same session reachable from laptop GUI, CLI, Telegram bot

### The one inherent tradeoff

Everything in "Inherent" above traces to a single fact: **zremote GUI is not the user's editor.** It is the operator console for a remote development environment. ACP features that assume "the GUI holds the files in dirty buffers with an LSP attached" — `fs/read_text_file` unsaved-buffer semantics, agent-following cursor, undo integration, LSP-aware diffs — degrade for us by construction. That is the structural cost of solving remote development rather than local editing. The driver framing does not introduce this tradeoff; it merely makes it explicit.

### A separate tradeoff: same agent, ACP mode vs. native-CLI mode

For agents we already support natively (today: Claude Code via hooks), choosing the ACP driver loses **TUI affordances** that are agent-specific and have no protocol channel. Concretely for CC: `ESC, ESC` semantics, `!`-prefix bash mode, `/cost`, `/compact`, prompt-history arrows — these live in CC's TUI which the ACP adapter does not expose (it uses CC's SDK, not the binary).

Mitigations land in §4.6:

- The ACP driver provisions a side PTY in the same session, so users get a real shell on the same host and cwd as the agent. `!`-prefix-style ad-hoc shell needs are covered there.
- The GUI keystroke layer remaps known TUI shortcuts in the chat input to typed ACP equivalents (`ESC, ESC` → `session/cancel`, slash commands → `available_commands_update` matches, etc.).
- For CC-specific TUI features without any typed equivalent *and* not coverable by the side PTY, the user-controlled fallback is Decision 7 — the launcher dropdown lets them pick `cc-hooks` per session.

This tradeoff is **per agent**: ACP-only agents (Codex, Gemini, Goose, Junie, …) have nothing to lose because they have no native zremote integration to compare against. Only `cc-hooks` ↔ `claude-acp` carries the choice. As more agents move to ACP-as-primary upstream, the tradeoff narrows to legacy CC affordances specifically.

---

## 10. Out of Scope

- Custom transports for ACP itself. We don't ship our own HTTP/WS-ACP — we ship canonical events.
- Exposing ACP from `zremote-agent` to external editors (Zed, JetBrains). Possible future RFC.
- Replacing the GUI↔agent REST/WS protocol with ACP. Not justified.
- A user-facing plugin SDK for third-party drivers. v1 drivers are in-tree.
- `fs/read_binary_file` and other not-yet-stable ACP methods. Bound to spec stability.

---

## 11. Testing Strategy

The refactor in P0 + P1 plus the new ACP layer in P2+ create three distinct testing surfaces. Each phase blocks merge on its own gate.

### 11.1 P0 parity tests (golden-replay) — built in-house

The connection-state refactor must not change observable behaviour. Strategy:

- Capture canonical event traces from a representative set of current sessions (PTY, plain shell; PTY + CC hooks active; PTY with various agentic tool detections).
- After the refactor, replay the same PTY input bytes and shell timing through the new `PtyDriver` pipeline.
- Diff the resulting canonical event traces. Any divergence blocks merge. Allow only intentional differences (e.g., reordering of events that were never causally ordered in the first place — these need explicit acknowledgement).

**No upstream test crate to lean on** (Decision 23): `agent-client-protocol-test` is `publish = false` and contains only mock JSON-RPC types for SDK doctests, not a parity harness. We build this ourselves. Lives in `crates/zremote-agent/tests/golden_replay/`. Captured fixtures in `crates/zremote-agent/tests/fixtures/`.

### 11.2 Driver-trait conformance tests

A small test harness exercises every driver against the same scripted scenarios:

- Start, send simple prompt, receive at least one event, end cleanly
- Cancel mid-prompt, verify clean shutdown
- Reject permission, verify the agent does not proceed
- Capability advertising matches what the driver actually emits

Each driver must pass the suite. Lives in `crates/zremote-agent/src/session_driver/conformance/`.

### 11.3 ACP translator unit tests

The ACP→canonical translator is the most subtle component. Test against captured `session/update` JSON fixtures from real Claude Code adapter runs:

- Each ACP `sessionUpdate` variant maps to the expected canonical event(s)
- Tool-call content (text / diff / terminal embed) translates correctly into `ExecutionNode` shape
- Permission requests round-trip through the channel-permission policy
- Unknown / `_meta` fields are dropped silently with a debug log
- Capability advertising at `initialize` is preserved on the driver handle

Lives in `crates/zremote-agent/src/session_driver/acp/translator/tests/`.

### 11.4 End-to-end with real Claude Code adapter

In CI, a smoke test that spawns `@zed-industries/claude-agent-acp` with a recorded prompt against a stub model endpoint, drives one full prompt-turn through `ClaudeAcpDriver`, and asserts canonical events arrive in the expected shape and order. Skipped if Node.js is unavailable.

This catches: SDK-version mismatches, environment / cwd / args drift in our spawn code, permission flow integration bugs.

Lives in `crates/zremote-agent/tests/e2e_acp.rs`.

### 11.5 Server-mode tunneling test

Two-process test: spin up a `zremote-agent local` and a GUI mock that connects via the `zremote-client` SDK. Drive a `ClaudeAcpDriver` session from the mock GUI and verify all canonical events arrive over the WS bridge unchanged.

Lives in `crates/zremote-agent/tests/server_mode_acp.rs`.

### 11.6 Security / sandbox tests for `fs/*` (P5)

Adversarial inputs against the sandboxed file IO module:

- Absolute paths outside the worktree → reject
- Relative paths with `..` → reject after canonicalization
- Symlinks pointing outside the worktree root → reject
- Path encoding tricks (URL-encoded slashes, NUL bytes) → reject
- Reads on world-readable system files (`/etc/passwd`) when worktree is `/tmp/foo` → reject

Security-reviewer signs off before P5 merges.

Lives in `crates/zremote-agent/src/session_driver/acp/host_methods/fs/tests/`.

### 11.7 What's NOT tested

- Real model output. We use stub responses; testing actual LLM behaviour is out of scope.
- Network failure modes. Existing daemon-mode tests already cover WS reconnect; we ride on those.
- Performance / latency. Measured by deployment metrics, not asserted in CI.

---

## 12. Per-Crate Work Map

Where work lands across the workspace. Use this when sizing PRs and assigning reviewers.

| Crate | What changes | Phase |
|---|---|---|
| `zremote-protocol` | Two new `AgentMessage` variants (`Plan`, `AgentMessageChunk`); two new `ServerMessage` variants for ACP commands (`StartAcpSession`, `AnswerPermission` extensions if needed). New REST shape for bulk content fetch. **No removal of existing variants.** | P2 (events), P4 (REST endpoint), P5 (none) |
| `zremote-core` | Possibly: a `driver_id` column on session rows if we promote it from JSON blob in a later release. **Not in v1** (Decision 8). New event handlers for `Plan` / `AgentMessageChunk` to persist to DB. | P2, P4 |
| `zremote-client` | New convenience methods on the SDK for `StartAcpSession`, `AnswerPermission`. New polling helpers for new event variants. Optional: registry-fetch helper. | P2, P3 |
| `zremote-agent` | All driver work: new `session_driver/` module, refactor of `connection/mod.rs`, ACP child spawn, translator, host-side providers. **The bulk of the RFC.** | P0–P5 |
| `zremote-server` | Forward new `AgentMessage` variants to GUI subscribers via the existing event firehose. Same forward-compat pattern as today; no new logic. | P2 |
| `zremote-gui` | New views: tool-call cards (P4), multi-buffer diff reviewer (P4), plan list widget (P4), permission modal (P4), embedded-terminal widget (P4), launcher dropdown including ACP entries (P2). Capability-flag-driven degradation throughout. | P2 (basic), P4 (rich) |
| `Cargo.toml` (workspace) | Add `agent-client-protocol = "=0.11.1"` and `agent-client-protocol-tokio = "=0.11.1"`, both with `features = ["unstable"]`. **Do not** add `agent-client-protocol-test` (unpublished per Decision 23) or `agent-client-protocol-conductor` (proxy-chain protocol, wrong shape per reuse research). For dev only: `cargo install agent-client-protocol-trace-viewer` for inspecting JSON-RPC traces. | P2 |
| `LICENSES/Apache-2.0-zed.txt` (new) + workspace `NOTICE` (new) | Required by Apache-2.0 §4 for the vendored translator pattern from Zed (Decision 21). | P2 |
| `docs/research/acp-zed-meta-tracking.md` (new) | Living document tracking which `_meta` extension keys mainstream agents require, derived from Zed's `agent_servers` source (Risk #13). Updated whenever we bump the SDK pin. | P2 onwards |
| `docs/rfc/` | This file. Future amendments as decisions evolve. | — |
| `docs/research/` | Research lives here; don't move it into `rfc/`. | — |

### File-level hot spots

The files most likely to balloon in size or churn during this RFC:

- `crates/zremote-agent/src/connection/mod.rs` — refactored in P0; expect a 30–50% line reduction as the per-session HashMaps collapse
- `crates/zremote-agent/src/session_driver/mod.rs` (new) — the trait + dispatcher
- `crates/zremote-agent/src/session_driver/acp/translator.rs` (new) — vendored from Zed's `handle_session_update` (Decision 21); most subtle code; needs §11.3 unit-test density. Header comment links to upstream commit per Apache-2.0 §4.
- `crates/zremote-agent/src/session_driver/acp/connection.rs` (new) — modelled on Zed's `AcpConnection::stdio` task-ownership pattern (Decision 21); holds `_io_task`, `_dispatch_task`, `_wait_task`, `_stderr_task` as `Task<()>` fields per CLAUDE.md async-task convention.
- `crates/zremote-gui/src/views/agent_session/` (new) — most UX work; expect to live in a dedicated module

### Files explicitly NOT touched

- `crates/zremote-agent/src/mcp/` — MCP integration stays as-is. ACP-driven sessions configure MCP servers via `session/new` (Decision 17).
- `crates/zremote-agent/src/worktree/` — provides the FS sandbox root for P5 (RFC-009); no changes needed.
- `crates/zremote-agent/src/projects/`, `knowledge/`, `linear/`, `hooks/` (the project hooks system from RFC-008) — orthogonal to this RFC.
- Database schema — Decision 8.

---

## 13. References

- Research synthesis: [`docs/research/README.md`](../research/README.md)
- Driver-architecture proposal: [`docs/research/driver-architecture.md`](../research/driver-architecture.md)
- ACP spec deep-dive: [`docs/research/acp-spec.md`](../research/acp-spec.md)
- ACP ecosystem survey: [`docs/research/acp-ecosystem.md`](../research/acp-ecosystem.md)
- zremote integration points: [`docs/research/zremote-acp-integration-points.md`](../research/zremote-acp-integration-points.md)
- Adjacent RFCs:
  - RFC-001 (agentic loops fix)
  - RFC-003 (agent profiles + `LauncherRegistry`)
  - RFC-008 (project hooks)
  - RFC-009 (worktree UX) — provides FS sandbox root for P5
- External: [agentclientprotocol.com](https://agentclientprotocol.com), [agent-client-protocol crate](https://crates.io/crates/agent-client-protocol), [ACP Registry JSON](https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json), [`@zed-industries/claude-agent-acp`](https://github.com/zed-industries/claude-agent-acp)
