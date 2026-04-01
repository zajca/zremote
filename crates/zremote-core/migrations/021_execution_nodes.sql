CREATE TABLE execution_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    loop_id TEXT,
    timestamp INTEGER NOT NULL,
    kind TEXT NOT NULL,
    input TEXT,
    output_summary TEXT,
    exit_code INTEGER,
    working_dir TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
CREATE INDEX idx_execution_nodes_session ON execution_nodes(session_id, timestamp);
CREATE INDEX idx_execution_nodes_loop ON execution_nodes(loop_id);
