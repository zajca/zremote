CREATE TABLE agentic_loops (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    project_path TEXT,
    tool_name TEXT NOT NULL,
    model TEXT,
    status TEXT NOT NULL DEFAULT 'working',
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    ended_at TEXT,
    total_tokens_in INTEGER DEFAULT 0,
    total_tokens_out INTEGER DEFAULT 0,
    estimated_cost_usd REAL DEFAULT 0.0,
    end_reason TEXT,
    summary TEXT
);

CREATE TABLE tool_calls (
    id TEXT PRIMARY KEY,
    loop_id TEXT NOT NULL REFERENCES agentic_loops(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    arguments_json TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    result_preview TEXT,
    duration_ms INTEGER,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    resolved_at TEXT
);

CREATE TABLE transcript_entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    loop_id TEXT NOT NULL REFERENCES agentic_loops(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    tool_call_id TEXT,
    timestamp TEXT NOT NULL
);

CREATE TABLE permission_rules (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL DEFAULT 'global',
    tool_pattern TEXT NOT NULL,
    action TEXT NOT NULL DEFAULT 'ask'
);

CREATE INDEX idx_agentic_loops_session_id ON agentic_loops(session_id);
CREATE INDEX idx_tool_calls_loop_id ON tool_calls(loop_id);
CREATE INDEX idx_transcript_entries_loop_id ON transcript_entries(loop_id);
