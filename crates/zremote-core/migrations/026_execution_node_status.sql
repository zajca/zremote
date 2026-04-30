-- Wipe legacy rows. Activity history before the schema upgrade is discarded.
DELETE FROM execution_nodes;

ALTER TABLE execution_nodes ADD COLUMN status TEXT NOT NULL DEFAULT 'completed';
ALTER TABLE execution_nodes ADD COLUMN tool_use_id TEXT NOT NULL DEFAULT '';

-- Now drop the defaults (SQLite trick: column added with DEFAULT cannot
-- have it removed; we accept the default in the schema and rely on the
-- application layer to always supply explicit values going forward).
CREATE UNIQUE INDEX idx_execution_nodes_tool_use_id
  ON execution_nodes (session_id, tool_use_id)
  WHERE tool_use_id != '';

CREATE INDEX idx_execution_nodes_running
  ON execution_nodes (session_id, status)
  WHERE status = 'running';
