-- Claude task sessions (wraps terminal sessions with Claude-specific metadata)
CREATE TABLE IF NOT EXISTS claude_sessions (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    host_id TEXT NOT NULL REFERENCES hosts(id),
    project_path TEXT NOT NULL,
    project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
    model TEXT,
    initial_prompt TEXT,
    claude_session_id TEXT,
    resume_from TEXT,
    status TEXT NOT NULL DEFAULT 'starting',
    options_json TEXT,
    loop_id TEXT REFERENCES agentic_loops(id) ON DELETE SET NULL,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    ended_at TEXT,
    total_cost_usd REAL DEFAULT 0.0,
    total_tokens_in INTEGER DEFAULT 0,
    total_tokens_out INTEGER DEFAULT 0,
    summary TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_claude_sessions_host ON claude_sessions(host_id);
CREATE INDEX IF NOT EXISTS idx_claude_sessions_project ON claude_sessions(project_path);
CREATE INDEX IF NOT EXISTS idx_claude_sessions_status ON claude_sessions(status);
CREATE INDEX IF NOT EXISTS idx_claude_sessions_cc_session ON claude_sessions(claude_session_id);
