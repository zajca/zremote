# Phase 2: Commander Generate

## Problem

To use ZRemote as a meta-orchestrator, a Claude Code instance needs comprehensive instructions on:
- What ZRemote CLI commands exist and how to use them
- What hosts and projects are available
- How to read/write shared context (TigerFS or CLI fallback)
- Common workflow patterns (task dispatch, memory sync, Linear integration)

Manually writing and maintaining this CLAUDE.md is impractical -- it needs dynamic data (available hosts, project states) and must stay in sync with CLI changes.

## Goal

A `zremote cli commander generate` command that produces a complete CLAUDE.md for a Commander CC instance. The output includes static CLI reference, dynamic infrastructure state, and workflow recipes.

## Command Interface

```
zremote cli commander generate                    # Output to stdout
zremote cli commander generate --write            # Write to .claude/commander.md in cwd
zremote cli commander generate --write --dir /p   # Write to /p/.claude/commander.md
```

### Flags

| Flag | Description |
|------|-------------|
| `--write` | Write to `.claude/commander.md` instead of stdout |
| `--dir <path>` | Target directory for `--write` (default: cwd) |
| `--no-dynamic` | Skip live API queries, output static template only |

The standard global flags (`--server`, `--local`, `--host`, `--output`) apply. The generator uses the API connection to fetch dynamic data.

## Generated Document Structure

### 1. Identity and Role

```markdown
# ZRemote Commander

You are a ZRemote Commander. Your role is to orchestrate Claude Code instances
across remote machines managed by ZRemote. You accept high-level tasks and
break them down into operations executed via `zremote cli`.

Always use `--output llm` for all zremote commands (set via ZREMOTE_OUTPUT=llm).
```

### 2. CLI Command Reference

Compact reference -- one line per command, showing flags and output shape. Generated from a static template (not from clap introspection in v1). Organized by domain:

- **Infrastructure**: host list, host get, status
- **Sessions**: session list, session create, session close, session attach
- **Projects**: project list, project get, worktree create/list/delete
- **Tasks**: task create, task get, task list, task resume, task discover
- **Context**: memory list, memory update, memory delete, knowledge extract, knowledge search
- **Monitoring**: loop list, loop get, events
- **Config**: config get, config set, settings get, settings set
- **Actions**: action list, action run

Each entry includes: command, key flags, expected output fields in LLM format.

### 3. Shared Context Protocol

Dynamic section -- adapts based on which projects have TigerFS enabled.

**For TigerFS-enabled projects:**
```markdown
Project "myapp" has shared filesystem at ~/.zremote/projects/myapp/shared/
- Read memories: cat ~/.zremote/projects/myapp/shared/memories/*.md
- Write memory: echo "content" > ~/.zremote/projects/myapp/shared/memories/key.md
- Changes sync across all hosts automatically (ACID via PostgreSQL)
```

**For projects without TigerFS:**
```markdown
Project "other-app" uses API-based memory:
- Read: zremote cli memory list <project_id> --output llm
- Write: zremote cli memory update <project_id> <memory_id> --content "..."
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
- myapp (dev-box) -- /home/user/myapp, rust, branch: main [TigerFS: enabled]
- frontend (dev-box) -- /home/user/frontend, node, branch: develop
- api (staging) -- /opt/api, rust, branch: main [TigerFS: enabled]
```

### 5. Workflow Recipes

Pre-built patterns for common Commander tasks. These are documentation, not code.

**Task Dispatch:**
How to create a CC task on a remote host, monitor it, and collect results.

**Memory Sync (without TigerFS):**
How to read memories before a task, inject into prompt, extract after completion.

**Linear Task Processing:**
How to take a Linear issue and turn it into a ZRemote task with proper context.

**Multi-Host Deployment:**
How to coordinate changes across multiple hosts.

**Project Review:**
How to inspect active loops, check task costs, review worktree state.

### 6. Error Handling

```markdown
## Error Handling

- Commands return exit code 0 on success, 1 on failure
- Errors go to stderr, data goes to stdout
- If a host is offline, task creation will fail -- check host status first
- If a task gets stuck, check the agentic loop status with `loop list`
```

## Dynamic Data Fetching

The generator makes these API calls (all via existing `ApiClient`):

1. `client.get_mode_info()` -- server mode and version
2. `client.list_hosts()` -- all hosts with status
3. For each online host: `client.list_projects(host_id)` -- projects with settings
4. For each project: `client.get_settings(project_id)` -- check TigerFS config

If `--no-dynamic` is used or API is unreachable, the dynamic sections are omitted with a note.

## Output Size Budget

Target: under 3000 tokens for the full CLAUDE.md. This is roughly 10-12KB of markdown. The CLI reference is the largest section -- keep it compact (one line per command, no examples beyond the output shape).

## Testing

- Test that `generate` produces valid markdown
- Test that `--no-dynamic` produces output without API calls
- Test that dynamic section correctly reflects host/project state (mock API responses)
- Test that TigerFS-enabled projects get filesystem instructions, others get CLI instructions
- Test output size stays under budget
