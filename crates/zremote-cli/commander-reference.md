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
- `task create --host <id> --project-path <path> [--model <m>] [--prompt <text>]` → creates CC task
- `task list [--host <id>] [--status <s>]` → `{"_t":"task","id","st","model","project","cost"}`
- `task get <id>` → single task with full details
- `task resume <id>` → resume paused task

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
- `settings list <project_id>` → project settings JSON
- `settings set <project_id> <key> <value>` → update setting

### Actions
- `action list <project_id>` → `{"_t":"action","n","command"}`
- `action run <project_id> <action_name>` → execute project action
