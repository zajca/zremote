ALTER TABLE sessions ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
CREATE INDEX idx_sessions_project_id ON sessions(project_id);

-- Retroactive: link existing sessions by matching working_dir to project path
UPDATE sessions SET project_id = (
    SELECT p.id FROM projects p
    WHERE p.host_id = sessions.host_id
    AND sessions.working_dir IS NOT NULL
    AND (sessions.working_dir = p.path OR sessions.working_dir LIKE p.path || '/%')
)
WHERE working_dir IS NOT NULL;
