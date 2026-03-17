-- Add columns for persistent terminal sessions via tmux
ALTER TABLE sessions ADD COLUMN suspended_at TEXT;
ALTER TABLE sessions ADD COLUMN tmux_name TEXT;
