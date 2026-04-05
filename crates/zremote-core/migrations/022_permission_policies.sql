CREATE TABLE IF NOT EXISTS permission_policies (
    project_id TEXT PRIMARY KEY,
    auto_allow TEXT NOT NULL DEFAULT '[]',
    auto_deny TEXT NOT NULL DEFAULT '[]',
    escalation_timeout_secs INTEGER NOT NULL DEFAULT 30,
    escalation_targets TEXT NOT NULL DEFAULT '["gui"]',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
