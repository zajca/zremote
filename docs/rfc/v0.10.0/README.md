# RFC: v0.10.0 — Agent Intelligence & Terminal Awareness

**Status:** Draft
**Date:** 2026-03-31
**Author:** zajca
**Inspiration:** Analysis of [Hermes IDE](https://github.com/hermes-hq/hermes-ide)

---

## Overview

v0.10.0 transforms ZRemote from a terminal session manager into a **provider-agnostic AI agent monitoring platform** with real-time telemetry, shell awareness, project intelligence, and structured communication with running agents.

### Problems Addressed

1. **Single-provider depth** — Deep integration only for Claude Code (via hooks). Aider, Codex, Gemini CLI detected by process name only, no telemetry.
2. **No real-time output analysis** — PTY output streams unanalyzed. No token tracking, cost monitoring, tool call logging, or phase detection.
3. **No shell awareness** — Raw PTY spawn without shell type detection, autosuggestion disabling, or environment injection.
4. **Shallow project intelligence** — Marker file discovery only. No framework, architecture, or convention extraction.
5. **Post-hoc knowledge only** — Knowledge extraction after loop completion. No real-time context delivery to running agents.
6. **No command-level tracking** — Only whole agentic loops tracked, not individual commands.

---

## Phases & Detailed RFCs

```
Phase 1-2: Output Analyzer ──→ output-analyzer.md       [DONE]
Phase 3:   Shell Integration ─→ shell-integration.md     [Draft, reviewed]
Phase 4:   Project Intelligence → project-intelligence.md [Draft, reviewed]
Phase 5:   Command Tracking ──→ command-tracking.md      [Draft, reviewed]
Phase 6:   Context Delivery ──→ context-delivery.md      [Draft, reviewed]
Phase 7:   Channel Bridge ───→ channel-bridge.md         [BLOCKED: CC Channels API preview]
```

| Phase | RFC | Status | Depends on | Complexity |
|-------|-----|--------|------------|------------|
| 1-2 | [Output Analyzer](output-analyzer.md) | **Done** (2026-03-31) | - | Medium |
| 3 | [Shell Integration](shell-integration.md) | Draft (reviewed) | - | Medium |
| 4 | [Project Intelligence](project-intelligence.md) | Draft (reviewed) | - | Low-medium |
| 5 | [Command Tracking](command-tracking.md) | Draft (reviewed) | Phase 1 | Low |
| 6 | [Context Delivery](context-delivery.md) | Draft (reviewed) | Phase 1, 4 | Medium-high |
| 7 | [Channel Bridge](channel-bridge.md) | Blocked | Phase 1, 6 | High |

Phases 1-4 are independent and parallelizable. Phase 5 extends the analyzer from Phase 1. Phase 6 depends on both Phase 1 (phase detection) and Phase 4 (project data). Phase 7 is a separate RFC blocked on CC Channels API stability.

---

## Out of Scope

| Feature | Reason |
|---------|--------|
| GUI changes for new telemetry | Separate RFC — needs dashboard for tokens, costs, timeline |
| Replacing Claude hooks | Hooks remain primary for Claude; analyzer supplements |
| LSP integration | Too large, separate effort |
| Session multiplexing (tabs/panes) | GUI-only concern, separate RFC |
| New AI provider API integrations | Only output parsing, no API calls |

---

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Regex patterns brittle across CLI versions | Loose patterns, log unmatched lines at TRACE level |
| Shell integration breaks user configs | Opt-in, source user config first, overrides additive only |
| Output analyzer adds PTY latency | Inline sync, sub-ms typical. Move to separate task if proven slow |
| Token estimation inaccuracy | Conservative budget, allow user override |
| ANSI stripping edge cases | `strip-ansi-escapes` crate (battle-tested) |

---

## Verification

### Per-phase
- `cargo test --workspace` passes
- `cargo clippy --workspace` clean
- New code has >80% test coverage

### End-to-end
1. Start local mode, launch Claude Code → agent detected, tokens tracked, phases visible
2. Launch Aider → adapter switches, token parsing works
3. `execution_nodes` table has command history
4. `echo $ZREMOTE_TERMINAL` returns `1` in AI session
5. Project scan returns frameworks and architecture
6. Modify pinned file → nudge delivered when agent idle

---

## References

- [Hermes IDE](https://github.com/hermes-hq/hermes-ide) — inspiration for output analyzer and shell integration
- [Output Analyzer RFC](output-analyzer.md) — detailed design for Phase 1-2
- [Shell Integration RFC](shell-integration.md) — detailed design for Phase 3
- [Project Intelligence RFC](project-intelligence.md) — detailed design for Phase 4
- [Command Tracking RFC](command-tracking.md) — detailed design for Phase 5
- [Context Delivery RFC](context-delivery.md) — detailed design for Phase 6
- [Channel Bridge RFC](channel-bridge.md) — detailed design for Phase 7
