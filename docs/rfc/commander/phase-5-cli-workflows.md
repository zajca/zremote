# Phase 5: CLI Gaps + Workflow Recipes

## Problem

Two gaps prevent the Commander from being fully functional:

1. **Knowledge extract not in CLI**: The API method `extract_memories()` exists in `zremote-client`, but the `knowledge` CLI command group doesn't expose it. The Commander needs this to extract learnings after a task completes.

2. **Linear integration not exposed**: The agent has a full Linear GraphQL client (`crates/zremote-agent/src/linear/`) used internally by the Telegram bot. The Commander can't access Linear data through ZRemote -- it must use external tools (curl, MCP) or the user must provide task descriptions manually.

## Part A: Knowledge Extract CLI

### Current State

The `knowledge` CLI command group (`crates/zremote-cli/src/commands/knowledge.rs`) currently supports:
- `knowledge status <project_id>` -- knowledge base status
- `knowledge index <project_id>` -- trigger indexing
- `knowledge search <project_id> --query "..."` -- search knowledge base
- `knowledge service <action>` -- control knowledge service

Missing: `knowledge extract` -- extract memories from a loop transcript.

### What to Add

```
zremote cli knowledge extract <project_id> --loop-id <loop_id>
```

This calls the existing `client.extract_memories()` API endpoint, which analyzes a Claude Code loop transcript and extracts reusable memories (patterns, decisions, pitfalls, conventions).

### Output

The extracted memories are returned and displayed using the existing `memories` formatter method. In LLM format:

```
{"_t":"memory","key":"auth-pattern","cat":"pattern","content":"Use JWT with refresh tokens","confidence":0.85}
{"_t":"memory","key":"test-convention","cat":"convention","content":"Always use in-memory SQLite for test isolation","confidence":0.92}
```

### Flags

| Flag | Description |
|------|-------------|
| `<project_id>` | Target project (positional) |
| `--loop-id <id>` | Agentic loop to extract from |
| `--session-id <id>` | Alternative: extract from session's latest loop |
| `--save` | Automatically save extracted memories to the project |

Without `--save`, memories are displayed but not persisted. With `--save`, they're saved via the API and also written to TigerFS mount if available.

## Part B: Linear Workflow Recipes

### Current State

The agent has a Linear GraphQL client (`crates/zremote-agent/src/linear/`) with:
- `viewer()` -- authenticated user
- `list_issues(team_key, filter, limit)` -- list issues with filters
- `get_issue(issue_id)` -- single issue details
- `list_teams()` -- list teams
- `list_projects(team_id)` -- list projects
- `list_cycles(team_id)` -- list cycles
- `active_cycle(team_id)` -- current sprint

This client is only used internally by the Telegram bot. It's not exposed via REST API or CLI.

### V1 Approach: Recipes Only (No Code)

For v1, the Commander CLAUDE.md includes workflow recipes that instruct CC to use Linear's API directly. This avoids adding new code while still enabling the workflow.

The Commander CC can access Linear via:
- **Linear MCP tool** (if the user has one configured in CC)
- **curl** to Linear's GraphQL API (CC can construct GraphQL queries)
- **User-provided task descriptions** (Commander asks the user for details)

### Recipe: Process a Linear Task

```markdown
## Workflow: Process Linear Task

Input: Linear issue identifier (e.g., "ENG-142") or description from user.

### Steps

1. **Get task details**
   - If you have a Linear MCP tool, use it to fetch the issue
   - Otherwise, ask the user for the task description and acceptance criteria

2. **Find the matching ZRemote project**
   zremote cli project list --output llm
   Match by project name or path to the repository mentioned in the task.

3. **Load shared context**
   If TigerFS: cat ~/.zremote/tigerfs/<project>/memories/*.md
   Otherwise: zremote cli memory list <project_id> --output llm

4. **Create an isolated worktree**
   zremote cli worktree create <project_id> --branch feat/<issue-key>

5. **Dispatch CC task**
   Compose a prompt that includes:
   - The Linear task description and acceptance criteria
   - Relevant shared memories from step 3
   - Instructions to follow project conventions

   zremote cli task create \
     --project-path <worktree_path> \
     --prompt "<composed prompt>" \
     --skip-permissions

6. **Monitor progress**
   zremote cli task get <task_id> --output llm
   zremote cli loop list --project <project_id> --output llm

   Wait for task to complete. Check periodically.

7. **Extract and save learnings**
   zremote cli knowledge extract <project_id> --loop-id <loop_id> --save

   Or with TigerFS: review task output and write key learnings to
   ~/.zremote/tigerfs/<project>/memories/

8. **Cleanup**
   If the task created a PR, report the PR URL.
   Optionally clean up the worktree:
   zremote cli worktree delete <project_id> <worktree_id>
```

### Future: Linear CLI Commands (V2)

In a later phase, expose the Linear client as CLI commands:

```
zremote cli linear issues --team ENG --status started
zremote cli linear issue get ENG-142
zremote cli linear teams
zremote cli linear cycles --team ENG --active
```

This requires:
- Moving the Linear client from `zremote-agent` to `zremote-core` or `zremote-client` (shared crate)
- Adding a `linear` command group to `zremote-cli`
- Adding `LINEAR_API_TOKEN` to CLI env/config

This is a separate RFC -- not in scope for Commander v1.

## Testing

### Knowledge Extract

- Test `knowledge extract` subcommand with mock API responses
- Test `--save` flag persists extracted memories
- Test LLM output format for extracted memories
- Test error handling when loop-id doesn't exist

### Linear Recipes

- No code to test -- recipes are documentation in the generated CLAUDE.md
- Manual validation: start Commander, ask it to process a Linear task, verify it follows the recipe
