# SessionDriver architecture — ACP as a driver, not a special case

**Date:** 2026-04-25
**Builds on:** [`README.md`](./README.md), [`zremote-acp-integration-points.md`](./zremote-acp-integration-points.md)
**Status:** proposal (no implementation)

---

## TL;DR

Treat ACP not as a parallel pipe to bolt on, but as **one of several pluggable drivers** for a session. A `SessionDriver` abstraction sits where today's `connection/mod.rs` god-object dispatches per-session state. Each driver owns its loop-event production and emits a canonical event stream that the existing UI / DB / Telegram consumers already understand. ACP, CC-via-hooks, CC-via-ACP, plain-PTY, and any future agent integration become **siblings** instead of conditional branches. This framing also doubles as the `connection/mod.rs` refactor we already need.

---

## 1. The framing

Today, "what runs in this session" is implicit and scattered:

- A PTY is always created.
- A process detector polls every 1s on top of every PTY.
- An OutputAnalyzer is attached to most PTYs.
- A CC hooks sidecar may or may not be running per-connection.
- A handful of HashMaps in `connection/mod.rs:222-919` track which sessions opted into which feature.

What the user is proposing: make this **explicit and exclusive**. Every session is owned by exactly one driver, picked at start time. The driver knows *how* to talk to the underlying agent and *what events* to emit. The rest of the system (UI, DB, Telegram, activity panel, permission policy) consumes a single canonical event stream regardless of who produces it.

```
                  +---------------------------+
                  |  SessionDriver::start()   |
                  +---------------------------+
                   |          |       |
   +---------------+   +------+--+    +-----------------+
   |  PtyDriver    |   | CcHooks |    | AcpDriver       |
   | (raw shell)   |   | Driver  |    |  + variant ids: |
   |  ─ process    |   | (PTY +  |    |    claude-acp,  |
   |    scan       |   | hooks   |    |    codex-acp,   |
   |  ─ analyzer   |   | sidecar |    |    gemini-acp,  |
   |    (regex)    |   | side-   |    |    generic-acp  |
   |               |   | channel)|    |                 |
   +-------+-------+   +----+----+    +--------+--------+
           |                |                   |
           +-------+--------+-------+-----------+
                   |
                   v
          canonical event stream
   (Output bytes, LoopStateUpdate, ExecutionNode,
    PermissionRequest, Plan, AgentMessageChunk,
    SessionEnded {stop_reason})
                   |
                   v
   +--------+----+--------+--------+---------+
   |   UI   | DB | Tele-  | Hooks  | Channel |
   |        |    | gram   | policy | bridge  |
   +--------+----+--------+--------+---------+
```

The picture is a thin trait above many implementations, not a fat trait + many capability gates.

---

## 2. The trait (sketch)

Lives in `crates/zremote-agent/src/session_driver/mod.rs` (new). The shape, not the final API:

```rust
/// What the GUI/server already consumes today, normalized.
pub enum DriverEvent {
    /// Raw PTY-like bytes for the terminal panel. PTY drivers stream this
    /// continuously; ACP drivers emit it only when the agent embeds a terminal
    /// in a tool call.
    Output { bytes: Bytes },

    /// Canonical loop state — the Working/WaitingForInput/RequiresAction
    /// pipeline that already exists at `crates/zremote-protocol/src/agentic.rs:14-23`.
    LoopState(LoopStateUpdate),

    /// One row in the existing ExecutionNode history.
    ToolCall(ExecutionNode),

    /// Routes through the existing channel permission flow at
    /// `crates/zremote-protocol/src/channel.rs:127-131`.
    PermissionRequest(PermissionRequest),

    /// ACP-only today. PTY drivers stay None.
    Plan(Vec<PlanEntry>),

    /// ACP-only: streaming model output token-by-token. PTY drivers stay None.
    AgentMessageChunk { text: String },

    /// Terminal end with reason: end_turn / cancelled / failed / shell_exit.
    SessionEnded { stop_reason: StopReason },
}

/// Capabilities the driver advertises so the UI can degrade gracefully.
#[derive(Clone, Copy, Debug, Default)]
pub struct DriverCapabilities {
    pub structured_loop_state: bool,   // false = best-effort regex; true = authoritative
    pub typed_tool_calls:      bool,   // tool_call has structured kind/locations
    pub typed_diffs:           bool,   // (path, oldText, newText)
    pub permission_requests:   bool,   // can ask before destructive actions
    pub streaming_text:        bool,   // agent_message_chunk
    pub plan_tracking:         bool,
    pub clean_cancel:          bool,   // session/cancel vs Ctrl-C
    pub session_resume:        bool,
    pub slash_commands:        bool,
    pub embedded_terminals:    bool,
}

#[async_trait]
pub trait SessionDriver: Send + Sync + 'static {
    fn id(&self) -> DriverId;                       // "pty" | "cc-hooks" | "claude-acp" | ...
    fn capabilities(&self) -> DriverCapabilities;

    /// Spawn whatever underlying process(es) this driver needs (PTY, ACP child,
    /// hooks sidecar, ...) and return a control handle + event receiver.
    async fn start(&self, params: StartParams) -> Result<DriverHandle>;
}

pub struct DriverHandle {
    pub events: tokio::sync::mpsc::Receiver<DriverEvent>,
    pub control: Box<dyn DriverControl>,
}

#[async_trait]
pub trait DriverControl: Send + Sync {
    async fn send_user_input(&self, input: UserInput) -> Result<()>; // PTY bytes OR ACP prompt
    async fn answer_permission(&self, request_id: Uuid, decision: PermissionDecision) -> Result<()>;
    async fn cancel_turn(&self) -> Result<()>;                       // clean cancel; PTY does Ctrl-C
    async fn shutdown(self: Box<Self>) -> Result<()>;
}
```

`DriverId` is a string slug stored on the session row and on `AgentProfileData.settings_json` as the new field `driver_id`. No SQL migration: profile settings are already a free-form JSON blob (RFC-003 §1).

---

## 3. The four concrete drivers (mapping of existing code)

| Driver | Wraps | New / mostly reuse | Capabilities advertised |
|---|---|---|---|
| **PtyDriver** | `pty/`, `agentic/detector.rs`, `agentic/analyzer.rs`, `agentic/manager.rs` (process scan) | mostly reuse; emits `LoopStateUpdate` from `AnalyzerEvent::PhaseChanged` and `Output` from PTY bytes | `structured_loop_state=false` (best-effort), `typed_tool_calls=false`, `permission_requests=false`, everything else `false` |
| **ClaudeHooksDriver** | PtyDriver + `hooks/handler.rs` sidecar | reuses today's CC integration in full; sidecar becomes an internal detail of *this driver only* | `structured_loop_state=true`, `typed_tool_calls=partial` (no diffs), `permission_requests=true`, `streaming_text=false`, `plan_tracking=false`, `clean_cancel=false` |
| **ClaudeAcpDriver** | spawns `@zed-industries/claude-agent-acp` over stdio; reuses ACP SDK | new (depends on `agent-client-protocol = "=0.11.1"`); translator emits `LoopStateUpdate` + `ExecutionNode` from `session/update` so existing UI consumers don't change | all true |
| **GenericAcpDriver** | spawns any ACP registry binary over stdio with profile-supplied command | same code as ClaudeAcpDriver, parameterized | all true (subject to agent's own `agentCapabilities`) |

What stays unchanged because of the framing:

- `crates/zremote-protocol/src/agentic.rs` — `AgenticAgentMessage`, `LoopStateUpdate`, `ExecutionNode` are already the canonical event types. Drivers emit them.
- `crates/zremote-protocol/src/channel.rs` — `PermissionRequest`/`PermissionResponse` keep their current shape; ACP `session/request_permission` translates *into* them.
- The existing GUI sidebar / activity panel / Telegram / permission UI all keep consuming the same events.
- Per-project `permission-policy` keeps its single decision point.

What gets cleaner:

- `connection/mod.rs:222-919` becomes `Connection { sessions: HashMap<SessionId, ActiveDriver> }` — 8+ HashMaps collapse into one.
- `hooks/handler.rs` is no longer a mid-`connection/` lifecycle hook; it's owned by the `ClaudeHooksDriver` impl. The "should I install hooks?" check (`connection/mod.rs:368-388`) goes away — the driver decides at construction.
- `agentic/manager.rs` (process scan) only runs for PTY-driver sessions. ACP and CC-hooks sessions don't need it because they get explicit start/end events.

---

## 4. Selection: which driver runs my session?

Source of truth at start time, in priority order:

1. Explicit user choice in the launcher dropdown ("Run with: Claude Code (ACP) / Claude Code (hooks) / Codex (ACP) / plain shell").
2. `AgentProfileData.driver_id` from the profile (RFC-003 settings_json).
3. Default for the agent kind (e.g., `claude` → `cc-hooks` for backwards compat in v1, switching to `claude-acp` once ACP-CC is proven).
4. Plain `PtyDriver` for "just give me a shell".

This makes the migration trivial: every existing session implicitly uses `PtyDriver` or `ClaudeHooksDriver` based on what's already happening. New sessions can opt into ACP. No flag day.

---

## 5. Why this is better than the synthesis recommendation

The synthesis ([README.md §3](./README.md)) treated ACP as "case (A): a new launcher inside the existing AgentLauncher trait." That works, but leaves the existing CC hooks pipeline as a parallel-life code path with hard-to-reason-about race conditions. The driver framing fixes that:

| Risk from research synthesis | How drivers fix it |
|---|---|
| **Double-driver races** between hooks sidecar and ACP for the same session ([§6.2](./zremote-acp-integration-points.md#6-risks)) | By construction impossible — exactly one driver per session. Selection at start is exclusive. |
| **`connection/mod.rs` is 1000+ lines / 8+ HashMaps** ([§6.3](./zremote-acp-integration-points.md#6-risks)) | The driver abstraction *is* the refactor. Per-session state is owned by the driver, not by global HashMaps. |
| **Permission policy double-source** ([§6.5](./zremote-acp-integration-points.md#6-risks)) | All drivers translate their permission needs into the canonical `PermissionRequest`. Single decision point at the channel layer. |
| **PTY ownership when GUI talks ACP directly** ([§6.6](./zremote-acp-integration-points.md#6-risks)) | The driver owns transport and translation. ACP frames produce `ExecutionNode`s through the driver-internal translator; UI feeds keep working without "ACP-aware" branches anywhere downstream. |
| **Per-launcher transport gate at `connection/mod.rs:368-388`** ([§7](./zremote-acp-integration-points.md#7-recommended-phasing)) | Disappears. Hooks install lives inside `ClaudeHooksDriver::start`, not in shared connection logic. |

It also reframes the unique zremote value prop from §2.4 of the synthesis: an `AcpDriver` running on the agent host with the GUI consuming the canonical event stream over our existing WS *is* "ACP over the network" — no need for a special `AgentMessage::AcpFrame` tunneling variant. Drivers run agent-side, events flow agent→server→GUI through the existing event channel.

---

## 6. Tradeoffs to call out

1. **Trait-design tax.** Designing `DriverEvent` and `DriverCapabilities` so they cover both PTY and ACP without becoming a mush is real work. Get the capability flags right or every UI consumer ends up branching on `driver_id`.
2. **Capability degradation UX.** When `streaming_text=false`, what does the chat panel do? When `typed_diffs=false`, what does the diff reviewer do? Per-feature fallback policies need to be documented up front, not retrofitted.
3. **Event ordering vs. backpressure.** Drivers should emit on a single `mpsc::Sender` per session so total ordering is preserved. The buffer size and how we handle slow consumers (drop? coalesce LoopState updates? wait?) is a per-event decision.
4. **Driver registry is a new public surface.** A trait + dynamic registration mechanism (probably keyed by `DriverId` strings) means we have to think about ABI/version pinning across crates. Recommendation: drivers live in `zremote-agent` crate only, no external plugin loading in v1.
5. **Two seam refactor.** This collapses `LauncherRegistry` (RFC-003) and `connection/mod.rs` per-session bookkeeping into one driver abstraction. That's good long-term but means RFC-003's `AgentLauncher::build_command` becomes one of two ways drivers spawn things — we either keep `AgentLauncher` as a helper *used by* drivers (cleanest) or fold it into the trait (bigger churn).

---

## 7. Phasing under the driver framing

Cleaner than the synthesis's Phases 1–5 because the refactor and the ACP work share a foundation.

| Phase | Scope | Notes |
|---|---|---|
| **P0 — Driver skeleton** | Define `SessionDriver` trait, `DriverEvent`, `DriverCapabilities`, `DriverHandle`. Wire one driver — `PtyDriver` — that wraps existing PTY + analyzer. `connection/mod.rs` is refactored to dispatch through the driver. No behaviour change, no ACP yet. | Pure refactor. Test parity with golden traces of PTY output and analyzer events. Lands the per-session struct cleanup the codebase already needs. |
| **P1 — ClaudeHooksDriver** | Move `hooks/handler.rs` ownership into a driver impl. The "install hooks sidecar?" decision becomes "did we instantiate `ClaudeHooksDriver`?" | No new user-facing behaviour. Deletes some HashMaps in `connection/mod.rs`. |
| **P2 — ClaudeAcpDriver (spike + ship)** | Pull in `agent-client-protocol = "=0.11.1"` (with `unstable_session_resume`, `unstable_session_close`). Spawn `@zed-industries/claude-agent-acp`. Translate ACP frames into the canonical event stream. New launcher entry "Claude Code (ACP)" in the GUI alongside the existing one. | First user-facing ACP capability. Validates the trait and the translator. CC remains usable via hooks, so we can flip per-profile. |
| **P3 — GenericAcpDriver** | Reuse the ACP transport from P2, parameterize the binary + args via `AgentProfileData`. Pull the [registry JSON](https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json) for one-click agent install in the profile editor. | Codex / Gemini / Goose / Junie / Cline / Cursor / OpenCode / etc. light up at once. |
| **P4 — Rich UI features** | Tool-call cards, multi-buffer diff review, plan list, permission modal, embedded terminal cards — driven by the canonical events. Capability flags decide what to show per session. | This is most of the UX cost; reusable across all ACP drivers. |
| **P5 — `terminal/*` and `fs/*` providers** | The agent host implements ACP's host-side methods backed by zremote PTY + worktree (RFC-009) sandbox. Lets ACP agents run shell commands and edit files on the *remote* host. | Where the "Zed for remote hosts" angle becomes user-visible. |
| **Defer** | ACP-as-server (case B in synthesis), internal protocol replacement (case C). | Both are XL with no current pain. |

---

## 8. Open questions specific to this framing

1. **Where does `LauncherRegistry` (RFC-003) end up?** Recommendation: keep it as a helper that builds `LaunchCommand` for drivers that need to spawn things. Drivers consume it. This avoids re-litigating RFC-003.
2. **Default driver for `claude` kind in v1.** Stay on `cc-hooks` for safety, with `claude-acp` opt-in? Or flip after a soak? Recommendation: stay on hooks for at least one minor release, ship `claude-acp` as opt-in with a feature flag in the profile editor, then flip the default once parity is confirmed.
3. **Driver lifetime vs. session lifetime.** A PTY session can outlive the agent's loop (user keeps the shell open after `claude` exits). Does the driver get torn down at `SessionEnded` or stays until the user closes the panel? Recommendation: driver stays as long as the underlying transport (PTY/stdio child) is alive; emits `SessionEnded` as a soft signal but does not self-destruct.
4. **Translator placement for ACP→ExecutionNode.** Inside the `AcpDriver` (private)? Or shared in a helper crate so external consumers can reuse it? Recommendation: private in v1, lift if we see a second consumer.
5. **Driver capabilities and the GUI.** Does the GUI fetch capabilities once at session start and cache, or react to live updates? ACP has `session/update` for plan / commands / mode but capabilities themselves are immutable for the session. Recommendation: immutable per session, set at construction.

---

## 9. What this changes in the synthesis recommendation

- **Recommendation flips from "ACP launcher fits into AgentLauncher" → "introduce SessionDriver; ACP/CC-hooks/PTY are sibling drivers."**
- **Phase ordering changes** — driver skeleton (P0) is the prerequisite, then ClaudeHooks driver (P1, refactor only), then ACP work.
- **Server-mode tunneling concern dissolves** — drivers run agent-side, events flow over the existing event WS, no new `AgentMessage::AcpFrame` variant needed.
- **Effort estimate shifts.** The total work is similar (P0+P1 add 2-3 weeks of refactor) but the second and third drivers are much cheaper than they would be as bolt-ons.
- **Risk list shrinks** — three of the six top risks in the synthesis become non-issues by construction.

This proposal supersedes [README §3 and §6.2/6.3](./README.md) of the synthesis as the recommended target architecture, while keeping all evidence-level material (spec, ecosystem, integration points) unchanged.
