# Phase 3: Commander Start

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
| `--no-regenerate` | Don't regenerate CLAUDE.md if it already exists | false |

Standard global flags (`--server`, `--local`, `--host`) apply and are forwarded to the generated CLAUDE.md and CC environment.

## Execution Flow

### Step 1: Generate CLAUDE.md

Call the same generation logic from Phase 2. Write to `{dir}/.claude/commander.md`.

If `--no-regenerate` is set and the file exists, skip generation. Otherwise always regenerate (to pick up current infrastructure state).

### Step 2: Set Environment

Prepare environment variables for the spawned CC process:

```
ZREMOTE_OUTPUT=llm                          # Always LLM format
ZREMOTE_SERVER_URL=<from --server flag>     # Server connection
ZREMOTE_HOST_ID=<from --host flag>          # If specified
```

These ensure that when Commander CC runs `zremote cli ...` commands via Bash, they automatically use the right server and output format.

### Step 3: Launch Claude Code

Spawn `claude` as a child process using `std::process::Command`:

```
claude [--model <model>] [--dangerously-skip-permissions] [<prompt>]
```

- Working directory: `--dir` path
- Environment: inherited + ZRemote overrides from Step 2
- Stdin/stdout/stderr: inherited (interactive terminal)
- The process replaces the current process (exec) or the CLI waits for it to exit

### Edge Cases

- **`claude` not found**: Error with helpful message ("Claude Code CLI not found in PATH")
- **Server unreachable during generate**: Generate with `--no-dynamic` fallback, warn on stderr
- **`.claude/` directory doesn't exist**: Create it
- **Previous commander.md exists**: Overwrite (unless `--no-regenerate`)

## Shell Quoting

When building the `claude` command, use single-quote shell escaping for the prompt argument (same pattern as `crates/zremote-agent/src/claude/mod.rs`). This is a small utility function in the commander module -- no need to depend on the agent crate.

## Testing

- Test that `start` creates `.claude/commander.md` in the target directory
- Test that environment variables are set correctly in the spawned process
- Test `--no-regenerate` skips generation when file exists
- Test error handling when `claude` binary is not found
- Test that `--prompt` is correctly passed to the spawned process
