-- Add git metadata columns to projects table
ALTER TABLE projects ADD COLUMN git_branch TEXT;
ALTER TABLE projects ADD COLUMN git_commit_hash TEXT;
ALTER TABLE projects ADD COLUMN git_commit_message TEXT;
ALTER TABLE projects ADD COLUMN git_is_dirty INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN git_ahead INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN git_behind INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN git_remotes TEXT;
ALTER TABLE projects ADD COLUMN git_updated_at TEXT;

-- Worktree-as-child-project: FK back to parent project
ALTER TABLE projects ADD COLUMN parent_project_id TEXT REFERENCES projects(id) ON DELETE CASCADE;
CREATE INDEX idx_projects_parent ON projects(parent_project_id);
