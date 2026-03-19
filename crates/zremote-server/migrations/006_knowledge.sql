CREATE TABLE knowledge_bases (
    id TEXT PRIMARY KEY,
    host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'stopped',
    openviking_version TEXT,
    last_error TEXT,
    started_at TEXT,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(host_id)
);

CREATE TABLE knowledge_indexing (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'queued',
    files_processed INTEGER DEFAULT 0,
    files_total INTEGER DEFAULT 0,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    completed_at TEXT,
    error TEXT
);

CREATE INDEX idx_knowledge_indexing_project_id ON knowledge_indexing(project_id);

CREATE TABLE knowledge_memories (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    loop_id TEXT REFERENCES agentic_loops(id) ON DELETE SET NULL,
    key TEXT NOT NULL,
    content TEXT NOT NULL,
    category TEXT NOT NULL DEFAULT 'pattern',
    confidence REAL NOT NULL DEFAULT 0.0,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX idx_knowledge_memories_project_id ON knowledge_memories(project_id);
CREATE INDEX idx_knowledge_memories_loop_id ON knowledge_memories(loop_id);
CREATE INDEX idx_knowledge_memories_category ON knowledge_memories(category);

-- FTS for memory content search
CREATE VIRTUAL TABLE knowledge_memories_fts USING fts5(
    key,
    content,
    content='knowledge_memories',
    content_rowid='rowid'
);

CREATE TRIGGER knowledge_memories_fts_insert AFTER INSERT ON knowledge_memories BEGIN
    INSERT INTO knowledge_memories_fts(rowid, key, content) VALUES (new.rowid, new.key, new.content);
END;

CREATE TRIGGER knowledge_memories_fts_delete AFTER DELETE ON knowledge_memories BEGIN
    INSERT INTO knowledge_memories_fts(knowledge_memories_fts, rowid, key, content)
        VALUES ('delete', old.rowid, old.key, old.content);
END;
