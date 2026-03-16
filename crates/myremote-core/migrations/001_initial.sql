CREATE TABLE hosts (
    id TEXT PRIMARY KEY,           -- UUID as text
    name TEXT NOT NULL,            -- display name (defaults to hostname)
    hostname TEXT NOT NULL,        -- machine hostname from agent
    auth_token_hash TEXT NOT NULL, -- SHA256 hash of the agent token
    agent_version TEXT,
    os TEXT,
    arch TEXT,
    status TEXT NOT NULL DEFAULT 'offline',  -- 'online' | 'offline'
    last_seen_at TEXT,            -- ISO 8601 timestamp
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,           -- UUID as text
    host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    shell TEXT,
    status TEXT NOT NULL DEFAULT 'creating', -- 'creating' | 'active' | 'closed'
    working_dir TEXT,
    pid INTEGER,
    exit_code INTEGER,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    closed_at TEXT
);

CREATE INDEX idx_sessions_host_id ON sessions(host_id);
CREATE INDEX idx_sessions_status ON sessions(status);
