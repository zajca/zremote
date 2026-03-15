# MyRemote

The core idea is a central server with a web UI that acts as a hub for remote machines. Machines connect to the server and register themselves, and from the UI you can see all connected machines, their status, and manage terminal sessions running on them.

Terminal sessions are first-class citizens. You can spawn a new session on any connected machine, see its output in real time from the server UI, and interact with it. But the sessions aren't locked to the UI - you can also attach to them directly from the machine itself, whether you're sitting at it physically, SSHed in, or connecting through any other means. Think of it like tmux/screen, but orchestrated centrally and accessible from anywhere.

The primary use case driving this is running terminal-based AI tools like Claude Code on remote machines. You want to kick off an agentic coding session on a powerful remote box, monitor it from the server UI, and step in when needed - all without worrying about SSH sessions dropping or losing context.

But it goes beyond just being a fancy remote terminal. For agentic loops specifically, the server should understand what's happening inside them. It should expose controls to pause, resume, or stop an agentic run. It should surface specialized actions - approve a tool call, reject a suggestion, provide input when the agent asks for it. The UI becomes not just a terminal viewer but an agentic loop control panel.

OAuth credential management is another important piece. Tools like Claude Code use OAuth tokens that expire. The server should monitor these credentials, know when they're about to expire (say 24 hours before), and proactively notify you. When a token is nearing expiry, it should provide a way to go through the login/refresh flow so the running sessions don't suddenly break because a token expired in the middle of a long coding run.

Telegram integration ties everything together for on-the-go management. When a session hits an error or an agent needs user input, you get a Telegram notification. You can list your active sessions, get a preview of what's happening in any of them, and even reply directly to agent prompts - all from your phone. This means you can have multiple agentic sessions running across different machines and stay in control without being glued to the web UI.
