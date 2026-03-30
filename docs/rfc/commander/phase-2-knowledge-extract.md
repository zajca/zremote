# Phase 2: Knowledge Extract CLI

## Problem

The API method `extract_memories()` exists in `zremote-client`, but the `knowledge` CLI command group doesn't expose it. The Commander needs this to extract learnings after a task completes. Without it, the Commander CLAUDE.md (Phase 3) would reference a command that does not exist.

This phase must ship before Phase 3 (Commander Generate) because the generated CLAUDE.md includes workflow recipes that reference `knowledge extract`.

## Current State

The `knowledge` CLI command group (`crates/zremote-cli/src/commands/knowledge.rs`) currently supports:
- `knowledge status <project_id>` -- knowledge base status
- `knowledge index <project_id>` -- trigger indexing
- `knowledge search <project_id> --query "..."` -- search knowledge base
- `knowledge service <action>` -- control knowledge service

Missing: `knowledge extract` -- extract memories from a loop transcript.

## What to Add

```
zremote cli knowledge extract <project_id> --loop-id <loop_id>
```

This calls the existing `client.extract_memories()` API endpoint, which analyzes a Claude Code loop transcript and extracts reusable memories (patterns, decisions, pitfalls, conventions).

## Command Interface

```
zremote cli knowledge extract <project_id> --loop-id <loop_id>
zremote cli knowledge extract <project_id> --session-id <session_id>
zremote cli knowledge extract <project_id> --loop-id <loop_id> --save
```

### Flags

| Flag | Description |
|------|-------------|
| `<project_id>` | Target project (positional, required) |
| `--loop-id <id>` | Agentic loop to extract from |
| `--session-id <id>` | Alternative: extract from session's latest loop |
| `--save` | Automatically save extracted memories to the project |

Either `--loop-id` or `--session-id` must be provided. Without `--save`, memories are displayed but not persisted. With `--save`, they are saved via the existing API.

## Output

The extracted memories are returned and displayed using the existing `memories` formatter method. In LLM format:

```
{"_t":"memory","key":"auth-pattern","cat":"pattern","content":"Use JWT with refresh tokens","confidence":0.85}
{"_t":"memory","key":"test-convention","cat":"convention","content":"Always use in-memory SQLite for test isolation","confidence":0.92}
```

## Files

- MODIFY `crates/zremote-cli/src/commands/knowledge.rs` -- add `Extract` subcommand

## Testing

- Test `knowledge extract` subcommand with in-memory SQLite and real API handlers (no mocks per project convention)
- Test `--save` flag persists extracted memories
- Test LLM output format for extracted memories
- Test error handling when loop-id doesn't exist
- Test that either `--loop-id` or `--session-id` is required
