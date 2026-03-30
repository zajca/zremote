# RFC: ZRemote Commander

Meta-orchestration layer for ZRemote -- Claude Code acts as a high-level controller, managing CC instances across remote machines via ZRemote CLI.

## Overview

Commander is a CC instance with an injected CLAUDE.md that knows how to use ZRemote. It accepts high-level tasks ("process Linear task for project X") and orchestrates remote CC instances, shared context, and workflow automation.

## Architecture

```
                    +-------------------+
                    |    Commander CC   |
                    | (local machine)   |
                    +--------+----------+
                             |
                    zremote cli --output llm
                             |
                    +--------v----------+
                    |   ZRemote Server  |
                    |   (Axum + SQLite) |
                    +---+----------+---+
                        |          |
               +--------v--+  +---v--------+
               |  Agent A   |  |  Agent B   |
               |  (host-1)  |  |  (host-2)  |
               +------------+  +------------+
```

TigerFS integration (optional per-project shared filesystem) is tracked in a separate RFC: [TigerFS Integration](phase-4-tigerfs.md).

## Prerequisites

Before any Commander work, the `ServerEvent` enum in `zremote-protocol/src/events.rs` must be extended with `#[serde(other)] Unknown` variant. Currently it uses `#[serde(tag = "type")]` without a fallback, which means any new event variant breaks deserialization on older clients. This is required by the project's own protocol compatibility rules.

## Phases

Commander v1 ships Phases 1-4. Phase 5 (TigerFS) is a separate RFC for v2.

| Phase | RFC | Description |
|-------|-----|-------------|
| 1 | [LLM Output Format](phase-1-llm-output.md) | Compact `--output llm` format for token-efficient CLI output |
| 2 | [Knowledge Extract CLI](phase-2-knowledge-extract.md) | Fill CLI gap: `knowledge extract` command |
| 3 | [Commander Generate](phase-3-commander-generate.md) | CLAUDE.md generator with ZRemote instructions and dynamic context |
| 4 | [Commander Start](phase-4-commander-start.md) | Launch CC with generated Commander context |
| 5 | [TigerFS Integration](phase-5-tigerfs.md) | Optional per-project shared filesystem via TigerFS (separate RFC, v2) |

## Implementation Order

```
Prerequisite: Add #[serde(other)] Unknown to ServerEvent
Phase 1 (LLM output)          -- no dependencies
Phase 2 (knowledge extract)   -- no dependencies, can parallel with Phase 1
Phase 3 (commander generate)  -- depends on Phase 1 + Phase 2
Phase 4 (commander start)     -- depends on Phase 3
Phase 5 (TigerFS)             -- separate RFC, independent of Phases 1-4
```

## Key Decisions

- **CLI via Bash** for all orchestration (not MCP). CLI already covers full API surface, new commands automatically available.
- **Memory sync via CLI** for v1. Commander reads memories before task dispatch, extracts new memories after completion. All through existing `memory` and `knowledge` CLI commands.
- **TigerFS deferred to v2.** It is the most complex phase with the most external dependencies (Go binary, PostgreSQL, FUSE privileges). Commander v1 is fully functional without it. If CLI-based memory sync proves insufficient in practice, TigerFS can be added later.
- **Concurrency: single Commander per project in v1.** Running multiple Commanders simultaneously is unsupported and may cause conflicts (duplicate worktrees, conflicting tasks). Future versions may add advisory locking.
- **Local mode supported.** Commander works in both server mode (multi-host) and local mode (single-host automation). Workflow recipes adapt based on detected mode.
