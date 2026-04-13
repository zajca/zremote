-- Track why a task was interrupted (agent_disconnected, timeout, etc.)
ALTER TABLE claude_sessions ADD COLUMN disconnect_reason TEXT;
