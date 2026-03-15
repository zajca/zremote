# MyRemote

## Vision

The core idea is a central server with a web UI that acts as a hub for remote machines. Machines connect to the server and register themselves, and from the UI you can see all connected machines, their status, and manage terminal sessions running on them.

Terminal sessions are first-class citizens. You can spawn a new session on any connected machine, see its output in real time from the server UI, and interact with it. But the sessions aren't locked to the UI — you can also attach to them directly from the machine itself, whether you're sitting at it physically, SSHed in, or connecting through any other means. Think of it like tmux/screen, but orchestrated centrally and accessible from anywhere.

The primary use case driving this is running terminal-based AI tools like Claude Code on remote machines. You want to kick off an agentic coding session on a powerful remote box, monitor it from the server UI, and step in when needed — all without worrying about SSH sessions dropping or losing context.

But it goes beyond just being a fancy remote terminal. For agentic loops specifically, the server should understand what's happening inside them. It should expose controls to pause, resume, or stop an agentic run. It should surface specialized actions — approve a tool call, reject a suggestion, provide input when the agent asks for it. The UI becomes not just a terminal viewer but an agentic loop control panel.

Everything in the UI is organized around a clear hierarchy: **Hosts → Projects → Sessions / Agentic Loops**. The sidebar gives you a bird's-eye view of all your machines, what's running where, and lets you drill down to any level with a click. The rest of this document walks through that structure and the actions available at every level.

## UI: Sidebar Hierarchy

The sidebar is the main navigation element. It presents a tree that mirrors the real topology of your infrastructure — which hosts are connected, what projects live on each host, and what sessions or agentic loops are running inside each project.

```
Sidebar
├── Host: devbox-01 (online)
│   ├── Projects
│   │   ├── /home/user/myremote
│   │   │   ├── Sessions (2)
│   │   │   │   ├── session-abc (bash, idle)
│   │   │   │   └── session-def (zsh, running)
│   │   │   └── Agentic Loops (1)
│   │   │       └── loop-ghi (Claude Code, working)
│   │   └── /home/user/other-project
│   │       ├── Sessions (0)
│   │       └── Agentic Loops (0)
│   └── Host Management
│       ├── Claude Code Login (valid, expires 3d)
│       ├── API Keys
│       ├── MCP Servers
│       └── Agent Settings
├── Host: gpu-server (online)
│   ├── Projects ...
│   └── Host Management ...
└── Host: laptop (offline, last seen 2h ago)
```

### Host Level

Each top-level node represents a connected (or previously connected) machine.

- **Displays:** hostname, connection status (online / offline / reconnecting), last seen timestamp, agent version
- **Actions:**
  - Rename host
  - Remove host from the server
  - Force reconnect
  - View connection history
  - View system info (OS, architecture, CPU, memory, disk)

### Project Level

Projects live under a host and correspond to directories on the remote machine.

- **Identified by:** absolute path on the remote host (e.g. `/home/user/myremote`)
- **Displays:** directory name (or custom name from `.claude/` config), count of active sessions, count of active agentic loops
- **Actions:**
  - Open a new terminal session in this directory
  - Start a new agentic loop in this directory
  - View / edit the project's `CLAUDE.md`
  - Manage project-level permissions
  - Manage plugins
  - Remove project from sidebar

### Session Level

Sessions are terminal instances running inside a project directory.

- **Displays:** shell type (bash, zsh, fish, ...), status (idle / running / paused), start time, duration, PID
- **Actions:**
  - Open terminal (xterm.js live view)
  - Attach / detach
  - Pause / resume
  - Kill session
  - View transcript (scrollback history)
  - Rename session

### Agentic Loop Level

Agentic loops are AI-driven coding sessions — Claude Code, Codex, or any other terminal-based agent.

- **Displays:** tool name (Claude Code, Codex, ...), status (working / waiting / paused / error / completed), current step description, model in use, context window usage, running time
- **Actions:**
  - View live terminal (xterm.js)
  - Pause / resume / stop the loop
  - Approve or reject pending tool executions
  - Provide input when the agent asks for it
  - Cancel a currently running tool
  - Set tool-level permissions (always allow, always deny, ask)
  - Switch model mid-run
  - View token usage and cost breakdown
  - View full conversation transcript
- **Sub-items visible in sidebar:**
  - Pending actions — tool calls waiting for approval, shown as a badge
  - Tool history — list of executed tool calls with results
  - Teams / sub-agents — child agents spawned by the loop (see Swarm section)

## How Projects Appear

Projects are discovered automatically. You don't need to configure them upfront.

- When you open a terminal session in a directory, that directory becomes a project
- When you start an agentic loop targeting a directory, it becomes a project
- When the agent scans the filesystem and finds a `.claude/` configuration directory, it registers the parent as a project

Each project is uniquely identified by the tuple `(host_id, directory_path)`. You can also add projects manually — point the agent at a path and it shows up in the sidebar.

## Relationships: Host → Project → Session → Agentic Loop

The hierarchy is strict and reflects reality:

- A **host** contains zero or more **projects**
- A **project** contains zero or more **sessions** and zero or more **agentic loops**
- An **agentic loop** may spawn zero or more **sub-agents** (see Swarm)

Sessions and agentic loops are siblings under a project, not nested inside each other. An agentic loop has its own terminal output but is conceptually different from a plain session — it has structured state (steps, tool calls, context usage) that the server understands and exposes through dedicated controls.

## Agentic Loop Control Panel

When you click on an agentic loop in the sidebar, the main panel becomes a dedicated control surface.

- **Terminal view** — live xterm.js rendering of the agent's terminal output, scrollable, searchable
- **Action bar** — prominent buttons for the most common actions: Approve, Reject, Provide Input, Pause, Stop
- **Tool execution queue** — a list of pending and recently executed tool calls, each with its name, arguments, status, and result preview. Pending calls have Approve / Reject buttons inline
- **Context usage bar** — visual indicator showing how much of the model's context window is consumed
- **Cost tracker** — running total of tokens used and estimated cost, broken down by model
- **Conversation transcript** — the full structured conversation between you and the agent, separate from the raw terminal output

## Credential & Login Management

Each host has a **Host Management** section in the sidebar for managing credentials and agent configuration.

- **Claude Code Login:**
  - Shows OAuth status: valid, expiring soon, or expired
  - Displays expiry countdown (e.g. "expires in 3 days")
  - Allows triggering login / refresh flow directly from the UI
  - Proactive alerts when tokens are nearing expiry (configurable threshold, default 24h)
- **API Keys:**
  - List all configured API keys on the host
  - Add new keys, rotate existing ones, revoke compromised keys
- **MCP Servers:**
  - List configured MCP server connections with their status (connected / disconnected / error)
  - View and edit server configurations
- **Agent Settings:**
  - Reconnect behavior (auto-reconnect, backoff strategy)
  - Heartbeat interval
  - Log level
- **System Info:**
  - OS, architecture, CPU count, memory, disk usage
- **Credential status dashboard:**
  - Traffic-light indicators for all credentials at a glance — green (valid), yellow (expiring soon), red (expired / missing)

## Swarm: Teams and Multi-Agent Orchestration

When an agentic loop spawns sub-agents (e.g. Claude Code's team feature), the UI shows them as children of the parent loop in the sidebar.

- Each sub-agent displays its own status, assigned task, model, and token usage
- **Actions on a swarm:**
  - Create a new team with defined roles
  - Delete a team
  - Send messages to individual agents or broadcast to the team
  - Pause / resume individual agents or the entire team
  - Task management — assign tasks, set dependencies between agents, view progress
- The parent loop's control panel includes a "Teams" tab showing the full agent tree with status indicators

## History, Analytics, and Cost Tracking

You want to know what happened, how much it cost, and where the resources went.

- **Session statistics:** total sessions created, total time, messages sent, tool calls executed
- **Token usage:** broken down by model, by day, by host, by project
- **Cost tracking:** estimated spend per host, per project, per agentic loop, with daily/weekly/monthly aggregation
- **Conversation history:** searchable archive of all agentic loop transcripts, filterable by host, project, date range, and keywords

## Configuration and Settings

Configuration exists at three levels, with more specific levels overriding more general ones:

- **Global (server level):** default permissions, notification settings, UI preferences, user accounts
- **Per-host (agent level):** agent behavior, reconnect policy, credential management, log level
- **Per-project:** `.claude/` directory on the remote host — project-specific instructions, permission rules, hooks, plugins

The UI provides:

- A settings page for global and per-host configuration
- A permission rules editor — define which tools are auto-approved, which require confirmation, which are blocked
- A hooks viewer — see what hooks are configured and their trigger conditions
- Plugin management — install, enable, disable, configure plugins per project

## Telegram Integration

Telegram serves as a mobile control plane so you can stay in the loop without being at the web UI.

- **Notifications:**
  - Errors and failures in agentic loops
  - Agent waiting for user input (tool approval, question)
  - Credential expiry warnings
  - Agentic loop completed
- **Commands:**
  - `/sessions` — list active sessions and loops across all hosts
  - `/preview <session>` — get a snapshot of a session's current output
  - `/hosts` — list connected hosts with status
- **Inline keyboard:** approve / reject tool calls directly from the notification
- **Reply-based input:** reply to an "agent needs input" notification to provide the answer directly

## Real-Time Monitoring

Everything updates in real time. No polling, no refresh buttons.

- **WebSocket streaming** of terminal output — what the agent types and sees, you see instantly
- **Progress indicators** for long-running tool executions
- **Pulsing badge** on sidebar items that are waiting for user input — impossible to miss
- **Error toasts** — immediate notification when something goes wrong, with context and suggested actions
- **Heartbeat monitoring** — the server tracks agent heartbeats and surfaces connection issues before they become session-breaking problems
