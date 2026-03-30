# Phase 3: Commander Generate

## Problem

To use ZRemote as a meta-orchestrator, a Claude Code instance needs comprehensive instructions on:
- What ZRemote CLI commands exist and how to use them
- What hosts and projects are available
- How to read/write shared context via CLI memory commands
- Common workflow patterns (task dispatch, memory sync, Linear integration)

Manually writing and maintaining this CLAUDE.md is impractical -- it needs dynamic data (available hosts, project states) and must stay in sync with CLI changes.

## Goal

A `zremote cli commander generate` command that produces a complete CLAUDE.md for a Commander CC instance. The output includes static CLI reference, dynamic infrastructure state, and workflow recipes.

## Prerequisite: Claude Code File Loading

Before implementation, verify how Claude Code loads project instructions. CC loads `.claude/CLAUDE.md` by default. The generated file should be written to a location CC actually reads. Options to verify:
- Does CC load all `*.md` files in `.claude/` directory?
- Does CC support `@` imports in CLAUDE.md?
- Should the generated content be appended to existing CLAUDE.md with a marker section?

The implementation must adapt based on this verification. If CC only reads `CLAUDE.md`, the generator should either append to it (with a marker like `<!-- ZRemote Commander (auto-generated, do not edit below) -->`) or use CC's import mechanism.

## Command Interface

```
zremote cli commander generate                    # Output to stdout
zremote cli commander generate --write            # Write to project dir (method TBD based on CC behavior)
zremote cli commander generate --write --dir /p   # Write to /p/ (method TBD)
```

### Flags

| Flag | Description |
|------|-------------|
| `--write` | Write to project directory instead of stdout |
| `--dir <path>` | Target directory for `--write` (default: cwd) |
| `--no-dynamic` | Skip live API queries, use cache or output static template only |

The standard global flags (`--server`, `--local`, `--host`, `--output`) apply. The generator uses the API connection to fetch dynamic data.

## Generated Document Structure

### 1. Identity and Role

```markdown
# ZRemote Commander

You are a ZRemote Commander. Your role is to orchestrate Claude Code instances
across remote machines managed by ZRemote. You accept high-level tasks and
break them down into operations executed via `zremote cli`.

Always use `--output llm` for all zremote commands (set via ZREMOTE_OUTPUT=llm).

Only one Commander should run per project at a time (no concurrency support).
```

### 2. CLI Command Reference

Compact reference -- one line per command, showing flags and output shape. The CLI reference should be maintained as a separate checked-in file (`crates/zremote-cli/commander-reference.md`) included verbatim by the generator. This makes it:
- Reviewable in PRs
- Testable for staleness (compare with `clap` output in CI)
- Editable without code changes

Organized by domain:

- **Infrastructure**: host list, host get, status
- **Sessions**: session list, session create, session close, session attach
- **Projects**: project list, project get, worktree create/list/delete
- **Tasks**: task create, task get, task list, task resume, task discover
- **Context**: memory list, memory update, memory delete, knowledge extract, knowledge search
- **Monitoring**: loop list, loop get, events
- **Config**: config get, config set, settings get, settings set
- **Actions**: action list, action run

Each entry includes: command, key flags, expected output fields in LLM format.

### 3. Context Protocol

Instructions for CLI-based memory operations:

```markdown
## Shared Context

Before dispatching a task, load shared memories for the target project:
  zremote cli memory list <project_id> --output llm

Include relevant memories in the task prompt so the dispatched CC instance
has context from previous work.

After a task completes, extract and save learnings:
  zremote cli knowledge extract <project_id> --loop-id <loop_id> --save
```

### 4. Dynamic Infrastructure State

Queried live from ZRemote API at generation time:

```markdown
## Current Infrastructure

Server: http://myserver:3000 (server mode, v0.9.0)

### Hosts
- dev-box (a1b2c3d4) -- online, agent v0.9.0, 3 projects
- staging (e5f6g7h8) -- online, agent v0.9.0, 1 project
- ci-runner (i9j0k1l2) -- offline

### Projects
- myapp (dev-box) -- /home/user/myapp, rust, branch: main
- frontend (dev-box) -- /home/user/frontend, node, branch: develop
- api (staging) -- /opt/api, rust, branch: main

Note: This is a snapshot from generation time. Use `zremote cli status`
and `zremote cli host list --output llm` for current state.
```

**Caching:** To avoid many API calls on every generation, the generator caches the dynamic section with a 5-minute TTL at `~/.zremote/commander-cache.json`. Use `--no-dynamic` to skip API calls entirely (uses cache or omits the section).

### 5. Error Handling

```markdown
## Error Handling

- Commands return exit code 0 on success, 1 on failure
- With --output llm, errors produce: {"_t":"error","code":"...","msg":"..."}
- If a host is offline, task creation will fail -- check host status first
- If a task gets stuck, check the agentic loop status with `loop list`
```

### 6. Workflow Recipes

Pre-built patterns for common Commander tasks. These are documentation, not code.

**Task Dispatch:**
How to create a CC task on a remote host, monitor it, and collect results.

**Memory Sync:**
How to read memories before a task, inject into prompt, extract after completion.

**Linear Task Processing:**
How to take a Linear issue and turn it into a ZRemote task with proper context. The Commander CC can access Linear via a Linear MCP tool, curl to Linear's GraphQL API, or user-provided task descriptions.

**Multi-Host Coordination:**
How to coordinate changes across multiple hosts.

**Error Recovery:**
When a task fails midway (worktree created, task dispatched, task fails), clean up:
- Check worktree status, delete if work was not committed
- Report error with context for the user

**Project Review:**
How to inspect active loops, check task costs, review worktree state.

### 7. Limitations

```markdown
## Limitations

- Only one Commander should run per project at a time
- Infrastructure state in this document is a snapshot -- use CLI for current state
- Cost tracking: monitor task costs with `task get` -- no automatic budget limits in v1
```

## Dynamic Data Fetching

The generator makes these API calls (all via existing `ApiClient`):

1. `client.get_mode_info()` -- server mode and version
2. `client.list_hosts()` -- all hosts with status
3. For each online host: `client.list_projects(host_id)` -- projects

If `--no-dynamic` is used, API is unreachable, or cache is fresh (< 5 min), the dynamic section uses cached data or is omitted with a note.

## Output Size Budget

Target: under 6000 tokens for the full CLAUDE.md (~20-24KB of markdown).

| Section | Budget |
|---------|--------|
| Identity + setup | ~200 tokens |
| CLI reference | ~2000 tokens |
| Context protocol | ~300 tokens |
| Dynamic infrastructure | ~500-1500 tokens (scales with hosts/projects) |
| Error handling | ~200 tokens |
| Workflow recipes | ~1500 tokens |
| Limitations | ~100 tokens |

For setups with many hosts (10+) and projects (20+), truncate to top 20 projects by recent activity to stay within budget.

## Files

- CREATE `crates/zremote-cli/src/commands/commander.rs` -- generate subcommand
- CREATE `crates/zremote-cli/commander-reference.md` -- static CLI reference (checked in, included by generator)
- MODIFY `crates/zremote-cli/src/commands/mod.rs` -- add `pub mod commander;`
- MODIFY `crates/zremote-cli/src/lib.rs` -- add `Commander` variant to `Commands` enum + match arm

## Testing

- Test that `generate` produces valid markdown within token budget
- Test that `--no-dynamic` produces output without API calls
- Test that dynamic section correctly reflects host/project state (use in-memory SQLite + real API handlers, no mocks)
- Test output size stays under 6000 tokens for typical setups (3 hosts, 10 projects)
- Test caching: second call within 5 min uses cache, not fresh API calls
- Test staleness detection for CLI reference
