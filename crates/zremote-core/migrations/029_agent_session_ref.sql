-- Durable record of an agent's native session id for a managed session.
-- Populated when AgentSessionRefCaptured is processed. RFC-013 reads this row
-- to build the resume command for a stopped agent.
ALTER TABLE sessions ADD COLUMN agent_kind TEXT;               -- 'claude' | 'codex' | NULL
ALTER TABLE sessions ADD COLUMN agent_session_ref TEXT;        -- native session id (opaque)
ALTER TABLE sessions ADD COLUMN agent_session_updated_at TEXT; -- ISO 8601 (RFC 3339)
