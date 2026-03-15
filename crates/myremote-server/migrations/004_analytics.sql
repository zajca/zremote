CREATE TABLE session_stats (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    total_bytes_in INTEGER DEFAULT 0,
    total_bytes_out INTEGER DEFAULT 0,
    total_commands INTEGER DEFAULT 0,
    duration_seconds INTEGER DEFAULT 0
);

-- Full-text search for transcripts
CREATE VIRTUAL TABLE transcript_fts USING fts5(
    content,
    content='transcript_entries',
    content_rowid='id'
);

-- Triggers to keep FTS in sync
CREATE TRIGGER transcript_fts_insert AFTER INSERT ON transcript_entries BEGIN
    INSERT INTO transcript_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER transcript_fts_delete AFTER DELETE ON transcript_entries BEGIN
    INSERT INTO transcript_fts(transcript_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;
