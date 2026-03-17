# Phase 6: Telegram Integration

**Goal:** Receive notifications about agentic activity via Telegram bot, approve/reject tool calls from mobile, and query status of hosts/sessions/loops via bot commands.

**Dependencies:** Phase 4 (agentic events)

---

## 6.1 Bot Core

**Files:** `crates/zremote-server/src/telegram/{mod.rs, state.rs}`

- [ ] Init `teloxide` bot from `TELEGRAM_BOT_TOKEN` env var
  - Telegram integration is optional: if env var is missing, skip bot startup with INFO log
  - If env var is present but invalid, fail fast with clear error
- [ ] User ID whitelist from `TELEGRAM_ALLOWED_USERS` env var (comma-separated Telegram user IDs)
  - Check on EVERY message and callback -- fail-closed (empty list = reject all)
  - Log rejected attempts at WARN level (user_id, message preview)
- [ ] Use long polling (not webhooks) -- simpler, no public endpoint needed
- [ ] Run bot in separate `tokio::spawn`, pass `Arc<AppState>` for server state access
- [ ] Bot crash doesn't bring down server -- catch panics, log error, attempt restart
- [ ] `TelegramState` struct:
  - `pending_inputs: HashMap<MessageId, AgenticLoopId>` -- track reply-based inputs
  - Rate limiter: max 1 notification per 5s per chat to prevent spam

---

## 6.2 Server Event Bus

**Files:** `crates/zremote-server/src/events.rs`

- [ ] Create `ServerEvent` enum:
  - `HostConnected { host_id, hostname }`
  - `HostDisconnected { host_id, hostname }`
  - `LoopStatusChanged { loop_id, session_id, status, tool_name }`
  - `LoopEnded { loop_id, reason, summary, cost }`
  - `ToolCallPending { loop_id, tool_call_id, tool_name, arguments_preview }`
  - `CredentialExpiring { host_id, credential_type, expires_in }`
- [ ] `tokio::broadcast` channel (buffer 1024) in AppState
- [ ] Emit events from:
  - Agent WS handler (connect/disconnect/heartbeat timeout)
  - Agentic message handlers (loop state changes, tool calls)
  - Session handlers (create/close)
- [ ] Consumers subscribe to the broadcast channel:
  - Telegram bot
  - Browser WS `/ws/events` endpoint (refactor from Phase 3.4 to use shared event bus)
  - Future: email, webhooks

---

## 6.3 Notifications

**Files:** `crates/zremote-server/src/telegram/{notifications.rs, format.rs}`

- [ ] Events that trigger Telegram notifications:
  - Agentic loop error -> "Loop errored on {host}: {tool_name} -- {error_message}"
  - Waiting for input -> "Loop on {host} needs input: {tool_name} wants to use {tool_call_name}" + inline keyboard
  - Credential expiry (<24h) -> "Credential {type} on {host} expires in {time}"
  - Loop completed -> "Loop completed on {host}: {tool_name} -- {summary_preview} ($cost)"
  - Host disconnected (unexpected) -> "Host {hostname} disconnected"
- [ ] Each notification type has configurable toggle (on/off) stored in global config
- [ ] `format.rs` -- Telegram HTML message formatting:
  - Use `<b>`, `<code>`, `<pre>` tags (Telegram HTML mode)
  - Truncate messages to 4096 chars (Telegram limit)
  - Escape HTML entities in user-generated content
  - Include host name in all messages for multi-machine context
- [ ] Batch rapid-fire events:
  - Buffer events for 2s before sending
  - Collapse multiple tool calls into single message: "3 tool calls pending on {host}"
  - Never send more than 1 notification per 5s per chat

---

## 6.4 Commands

**Files:** `crates/zremote-server/src/telegram/commands.rs`

- [ ] `/sessions` -- list active sessions and loops across all hosts
  - Format: host name, session shell, loop status icon, tool name
  - Group by host
- [ ] `/preview <session_id>` -- last 20 lines of terminal output
  - Render as monospace block (`<pre>` tag)
  - Truncate if exceeds message limit
  - Error if session not found or closed
- [ ] `/hosts` -- list connected hosts with status
  - Format: status emoji, hostname, OS/arch, last seen
- [ ] `/help` -- list available commands with descriptions
- [ ] Error handling:
  - Unknown command -> show help text
  - Invalid arguments -> show usage example
  - Internal error -> generic "Something went wrong" message

---

## 6.5 Interactive Actions

**Files:** `crates/zremote-server/src/telegram/callbacks.rs`

- [ ] Inline keyboard on "waiting for input" notifications:
  - [Approve] [Reject] buttons
  - Callback data format: `approve:{loop_id}:{tool_call_id}` / `reject:{loop_id}:{tool_call_id}`
- [ ] Callback handler:
  - Parse callback data
  - Validate tool call is still pending (not already resolved)
  - Send `ServerMessage::AgenticLoopUserAction` to agent via ConnectionManager
  - Update Telegram message text: "Approved by {username}" / "Rejected by {username}" (edit original message)
  - If already resolved: answer callback with "Already resolved"
- [ ] Reply-based text input:
  - When "Provide Input" action is needed, send message with "Reply to this message with your input"
  - Track in `pending_inputs: HashMap<MessageId, AgenticLoopId>`
  - When user replies -> extract text -> send as `ProvideInput` action to agent
  - Confirm: "Input sent: {first 50 chars}..."
  - Timeout: remove from pending_inputs after 5 minutes

---

## Verification Checklist

1. [ ] Server starts with TELEGRAM_BOT_TOKEN set -> bot starts, logs "Telegram bot connected"
2. [ ] Server starts without TELEGRAM_BOT_TOKEN -> server runs normally, logs "Telegram bot disabled"
3. [ ] Unauthorized user sends /hosts -> rejected, logged at WARN
4. [ ] Authorized user sends /hosts -> list of connected hosts
5. [ ] Agentic loop waits for input -> Telegram notification arrives with [Approve] [Reject] buttons
6. [ ] Tap Approve -> tool call approved on server -> agent continues -> Telegram message updated
7. [ ] /preview with valid session -> last 20 lines shown in monospace
8. [ ] Rapid tool calls -> batched into single notification (not spammed)
9. [ ] Bot crashes -> server continues running, bot restarts

## Review Notes

- User ID whitelist checked on EVERY incoming message/callback, not just commands
- Never include sensitive data in Telegram messages (API keys, terminal content with passwords)
- All Telegram operations have timeouts -- server MUST NOT be affected by Telegram API failures
- Rate limiting prevents notification spam during rapid agentic activity
- Long polling vs webhooks: long polling is correct for server behind VPN
- teloxide crate risk: if abandoned, fallback is raw HTTP to Telegram API
- Event bus pattern decouples notification logic from core server -- clean separation
