# Phase 4: Commander Start

## Problem

Starting a Commander CC session requires multiple manual steps:
1. Run `zremote cli commander generate --write` to create the CLAUDE.md
2. Set environment variables (`ZREMOTE_OUTPUT`, `ZREMOTE_SERVER_URL`)
3. Launch `claude` with the right flags

This should be a single command.

## Goal

`zremote cli commander start` -- generates Commander CLAUDE.md, writes it to the project directory, and launches Claude Code with the correct environment.

## Command Interface

```
zremote cli commander start                              # Start in cwd
zremote cli commander start --dir /path/to/project       # Start in specific dir
zremote cli commander start --model opus                  # Specific model
zremote cli commander start --prompt "Process LIN-123"   # Initial prompt
zremote cli commander start --skip-permissions            # Autonomous mode
```

### Flags

| Flag | Description | Default |
|------|-------------|---------|
| `--dir <path>` | Working directory for CC | cwd |
| `--model <model>` | Claude model to use | (CC default) |
| `--prompt <text>` | Initial prompt for the Commander | (none, interactive) |
| `--skip-permissions` | Run CC with `--dangerously-skip-permissions` | false |
| `--no-regenerate` | Don't regenerate CLAUDE.md if it already exists and is < 5 min old | false |
| `--claude-path <path>` | Path to `claude` binary | auto-detect from PATH |

Standard global flags (`--server`, `--local`, `--host`) apply and are forwarded to the generated CLAUDE.md and CC environment.

### Commander Status

```
zremote cli commander status                             # Show commander state
```

Reports: is a commander.md present, when was it generated, what infrastructure state was captured, is a Commander CC currently running.

## Execution Flow

### Step 1: Generate CLAUDE.md

Call the same generation logic from Phase 3. Write to the project directory using the method determined by the CC file loading verification (see Phase 3 prerequisite).

If `--no-regenerate` is set and the file exists and is less than 5 minutes old, skip generation. Otherwise always regenerate (to pick up current infrastructure state). Uses the same 5-minute cache as Phase 3 to avoid redundant API calls.

### Step 2: Set Environment

Prepare environment variables for the spawned CC process:

```
ZREMOTE_OUTPUT=llm                          # Always LLM format
ZREMOTE_SERVER_URL=<from --server flag>     # Server connection
ZREMOTE_HOST_ID=<from --host flag>          # If specified
```

These ensure that when Commander CC runs `zremote cli ...` commands via Bash, they automatically use the right server and output format.

### Step 3: Locate Claude Code Binary

Look for the `claude` binary in this order:
1. `--claude-path` flag if provided
2. `CLAUDE_CODE_PATH` environment variable
3. `claude` in PATH (via `which claude`)
4. Common install locations: `~/.local/bin/claude`, `~/.npm/bin/claude`

If not found, exit with a clear error message including install instructions.

### Step 4: Launch Claude Code

Spawn `claude` as a child process using `std::process::Command`:

```
claude [--model <model>] [--dangerously-skip-permissions] [<prompt>]
```

- Working directory: `--dir` path
- Environment: inherited + ZRemote overrides from Step 2
- Stdin/stdout/stderr: inherited (interactive terminal)
- The process replaces the current process (exec) or the CLI waits for it to exit
- Exit code from `claude` is propagated to the caller

### Edge Cases

- **`claude` not found**: Error with install instructions and `--claude-path` hint
- **Server unreachable during generate**: Generate with `--no-dynamic` fallback, warn on stderr
- **Target directory doesn't exist**: Error (don't create project dirs implicitly)
- **`.claude/` directory doesn't exist**: Create it
- **Previous generated file exists**: Overwrite (unless `--no-regenerate` and fresh)

## Shell Quoting

When building the `claude` command, use single-quote shell escaping for the prompt argument (same pattern as `crates/zremote-agent/src/claude/mod.rs`). This is a small utility function in the commander module -- no need to depend on the agent crate.

## Files

- MODIFY `crates/zremote-cli/src/commands/commander.rs` -- add `start` and `status` handlers

## Testing

- Test that `start` creates the CLAUDE.md in the target directory
- Test that environment variables are set correctly in the spawned process
- Test `--no-regenerate` skips generation when file exists and is fresh
- Test error handling when `claude` binary is not found (including helpful message)
- Test that `--prompt` is correctly passed to the spawned process
- Test `--claude-path` overrides PATH detection
- Test `status` reports correct state
- Test exit code propagation from `claude` process
