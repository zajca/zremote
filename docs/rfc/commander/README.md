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
               +-----+------+  +-----+------+
                     |                |
              [TigerFS mount]  [TigerFS mount]   (optional, per-project)
                     |                |
              +------v----------------v------+
              |        PostgreSQL            |
              |   (shared project context)   |
              +------------------------------+
```

## Phases

| Phase | RFC | Description |
|-------|-----|-------------|
| 1 | [LLM Output Format](phase-1-llm-output.md) | Compact `--output llm` format for token-efficient CLI output |
| 2 | [Commander Generate](phase-2-commander-generate.md) | CLAUDE.md generator with ZRemote instructions and dynamic context |
| 3 | [Commander Start](phase-3-commander-start.md) | Launch CC with generated Commander context |
| 4 | [TigerFS Integration](phase-4-tigerfs.md) | Optional per-project shared filesystem via TigerFS |
| 5 | [CLI Gaps + Workflows](phase-5-cli-workflows.md) | Knowledge extract CLI, Linear workflow recipes |

## Key Decisions

- **CLI via Bash** for all orchestration (not MCP). CLI already covers full API surface, new commands automatically available.
- **TigerFS optional per-project**. ZRemote core stays on SQLite. Projects that need cross-host sharing enable TigerFS.
- **Memory dual path**. With TigerFS: read/write files. Without: CLI memory commands. Commander CLAUDE.md adapts automatically.
- **Phases 1-3 ship independently** of Phase 4. Commander works without TigerFS using CLI-based memory sync.
