# Research: Similar Projects to ZRemote

## Context
ZRemote = central server with web UI for managing remote machines, terminals, agentic sessions, OAuth credentials and Telegram notifications. This document maps existing projects that cover parts of this vision.

---

## 1. Claude Code Remote Control (Anthropic - official)
- **URL**: https://code.claude.com/docs/en/remote-control
- **What**: Official Anthropic feature (launched Feb 25, 2026). Continue Claude Code sessions from phone/tablet/browser.
- **How**: Sync layer between local CLI and mobile/web app. Files and MCP servers stay on local machine, only chat and tool results flow through encrypted bridge. Session survives sleep/network drops.
- **Availability**: Max and Pro subscribers.
- **Overlap with ZRemote**: Covers "control session from phone" part. BUT: single session only, Claude Code only, no central multi-machine management, no OAuth monitoring, no Telegram.

## 2. Claude-Code-Remote (JessyTsui)
- **URL**: https://github.com/JessyTsui/Claude-Code-Remote
- **What**: Open-source tool for remote Claude Code control via email, Discord and Telegram.
- **How**: Start task locally, get notification when Claude finishes, reply to email/Telegram to send next command.
- **Overlap with ZRemote**: Telegram integration, notifications, remote control. BUT: not a central server, no web UI, no multi-machine management, no OAuth monitoring, no real-time terminal view.

## 3. CloudCLI / Claude Code UI (siteboon)
- **URL**: https://github.com/siteboon/claudecodeui
- **What**: Open-source web UI/GUI for managing Claude Code sessions and projects remotely. Also works with Cursor CLI, Codex, Gemini CLI.
- **How**: Auto-discovers sessions from ~/.claude, chat interface, integrated shell terminal, file explorer, git explorer. Self-hostable.
- **Overlap with ZRemote**: Web UI for session management, mobile access. BUT: runs on single machine (not a multi-machine hub), no agentic loop controls, no OAuth monitoring, no Telegram.

## 4. claude-code-webui (sugyan)
- **URL**: https://github.com/sugyan/claude-code-webui
- **What**: Web interface for Claude Code CLI with streaming chat responses. React frontend + Deno/Node backend + Claude Code SDK.
- **How**: Real-time streaming, project directory selection, conversation history, tool permission management.
- **Overlap with ZRemote**: Web UI for Claude Code. BUT: single machine, no multi-machine orchestration, no specific agentic controls, no OAuth/Telegram.

## 5. Moshi (getmoshi.app)
- **URL**: https://getmoshi.app/
- **What**: Mobile SSH/MOSH terminal app optimized for AI agents (Claude Code, Codex). iOS app.
- **How**: SSH/Mosh client with push notifications when agent needs input, voice control (local Whisper), connects to tmux sessions.
- **Overlap with ZRemote**: Mobile access to agentic sessions, push notifications. BUT: just an SSH client (not a central server), no own session management, no OAuth monitoring, no Telegram bot.

## 6. Web-based terminals (self-hosted)
- **Nexterm** (https://noted.lol/nexterm/) - server management with SSH/VNC/RDP, 2FA, session management
- **Termix** - self-hosted SSH via browser, split-screen
- **Wetty** - terminal over HTTP/HTTPS, xterm.js + websockets
- **Webmux** (https://ronreiter.github.io/webmux/) - web-based terminal multiplexer
- **Overlap with ZRemote**: Web UI for terminals on remote machines. BUT: no awareness of agentic loops, no OAuth monitoring, no chatbot integration.

## 7. Agent orchestration (general)
- **CrewAI** (https://crewai.com/) - multi-agent orchestration, open-source
- **Composio Agent Orchestrator** (https://github.com/ComposioHQ/agent-orchestrator) - parallel coding agents, CI fixes, code reviews
- **Overlap with ZRemote**: Agent orchestration. BUT: focused on AI model orchestration, not terminal session management and infrastructure.

---

## Summary - what NOBODY does

No existing project combines all aspects of ZRemote:

| Feature | Claude RC | JessyTsui | CloudCLI | Moshi | Web terms | ZRemote |
|---|---|---|---|---|---|---|
| Central multi-machine hub | - | - | - | - | partial | **YES** |
| Web UI for sessions | - | - | YES | - | YES | **YES** |
| Attach from local (tmux-style) | - | - | - | via SSH | - | **YES** |
| Agentic loop controls | - | - | - | - | - | **YES** |
| OAuth monitoring + notifications | - | - | - | - | - | **YES** |
| Telegram bot integration | - | YES | - | - | - | **YES** |
| Mobile access | YES | YES | YES | YES | - | **YES** |

**Closest competitors**: CloudCLI (web UI) + JessyTsui (Telegram) + Anthropic Remote Control (mobile access). But each solves only a partial piece. ZRemote is unique in being a central hub for multiple machines with full integration of agentic controls, OAuth and Telegram.
