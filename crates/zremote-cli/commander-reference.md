## CLI Reference

All commands: `zremote cli [--server URL] [--host ID] [--output llm] <command>`

### Infrastructure
- `host list` → `{"_t":"host","id","n","st","v","hostname"}`
- `host get <id>` → single host details
- `status` → `{"_t":"status","mode","v","hosts","online"}`

### Sessions
- `session list [--all]` → `{"_t":"session","id","n","st","shell","dir"}`
- `session create --host <id> [--shell <path>] [--name <name>]` → session ID
- `session close <id>` → closes terminal session
- `session attach <id>` → interactive terminal (not for Commander use)

### Projects
- `project list` → `{"_t":"project","id","n","path","type","branch","dirty"}`
- `project get <id>` → single project details
- `project scan` → trigger project discovery
- `worktree list <project_id>` → `{"_t":"worktree","path","branch","dirty"}`
- `worktree create <project_id> --branch <name>` → create worktree
- `worktree delete <project_id> <path>` → remove worktree

### Tasks
- `task create --host <id> --project-path <path> [--model <m>] [--prompt <text>] [--print] [--channel <spec>]` → creates CC task
- `task list [--host <id>] [--status <s>]` → `{"_t":"task","id","sid","st","model","project","cost"}`
- `task get <id>` → single task with full details (includes error_message on failure)
- `task resume <id>` → resume paused task
- `task log <id>` → get task output (ANSI-stripped scrollback)
- `task send <id> <message> [--priority normal|high|urgent]` → send message to running task via channel
- `task approve <id> <request_id> yes|no [--reason <text>]` → approve/deny permission request
- `task cancel <id> [--force]` → cancel running task (graceful abort, then kill)
- `task input <id> --text <text>` → send text + CR to task's PTY stdin
- `task input <id> --raw <escape_seq>` → send raw bytes to PTY (supports \r, \n, \t, \e, \xNN)

#### Task creation flags
- `--print` — non-interactive mode: task answers the prompt and exits (no TUI). Use for fire-and-forget work.
- `--channel <spec>` — load a development channel into the task's Claude Code session. The spec is a tagged identifier (e.g. `plugin:zremote@local`). Can be repeated for multiple channels. When a channel is active, you can interact with the running task via `task send` and `task approve`.

#### Channel workflow
1. Create a task with `--channel plugin:zremote@local` to enable bidirectional communication
2. Use `task send <id> <message>` to send instructions or context to the running task
3. Use `task approve <id> <request_id> yes|no` to approve/deny permission requests from the task
4. Without `--channel`, the task runs autonomously — `task send` and `task approve` are not available

#### PTY input (low-level)
`task input` sends raw bytes directly to a task's PTY stdin. Use it for:
- Confirming TUI dialogs (dev channel warning, workspace trust)
- Sending keyboard shortcuts (Ctrl+C via `\x03`, Escape via `\e`)
- Any interactive prompt that `task send` (channel message) cannot handle

Key: `\r` is Enter for TUI dialogs (carriage return), not `\n`.

#### Interactive task workflow
1. Create task with channel: `task create --host <id> --project-path <path> --channel plugin:zremote@local --prompt "..."`
2. Dev channel dialog auto-approved (agent handles it)
3. Send instructions: `task send <id> <message>`
4. Approve permissions: `task approve <id> <request_id> yes`
5. Send PTY input if needed: `task input <id> --raw "\r"`

#### When to use --print vs interactive
- `--print` — simple questions, code generation, one-shot tasks. Output goes to stdout (not visible in `task log`).
- Interactive (no --print) + `--channel` — implementation tasks, multi-step work. Use `task send` for instructions, `task input` for TUI interaction.
- Interactive without channel — task runs in TUI but cannot receive remote messages. Only `task input` works.

### Context
- `memory list <project_id>` → `{"_t":"memory","id","key","cat","content"}`
- `memory update <project_id> <memory_id> --content <text>` → update memory
- `memory delete <project_id> <memory_id>` → remove memory
- `knowledge extract <project_id> --loop-id <id> [--save]` → extract memories from loop
- `knowledge search <project_id> <query> [--tier L0|L1|L2]` → search knowledge base

### Monitoring
- `loop list [--host <id>] [--status <s>]` → `{"_t":"loop","id","session","st","tool","task"}`
- `loop get <id>` → single loop details
- `events [--filter <types>]` → real-time event stream (NDJSON)

### Config
- `config get <key>` → `{"_t":"config","key","v"}`
- `config set <key> <value>` → set global config
- `settings get <project_id>` → project settings JSON
- `settings save <project_id> --file <path>` → save project settings from JSON file
- `settings configure <project_id>` → configure project with Claude AI

### Actions
- `action list <project_id>` → `{"_t":"action","n","command"}`
- `action run <project_id> <action_name>` → execute project action
