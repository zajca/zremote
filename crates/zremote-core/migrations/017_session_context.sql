-- Add real-time metrics columns from Claude Code status line
ALTER TABLE claude_sessions ADD COLUMN context_used_pct REAL;
ALTER TABLE claude_sessions ADD COLUMN context_window_size INTEGER;
ALTER TABLE claude_sessions ADD COLUMN rate_limit_5h_pct INTEGER;
ALTER TABLE claude_sessions ADD COLUMN rate_limit_7d_pct INTEGER;
ALTER TABLE claude_sessions ADD COLUMN lines_added INTEGER DEFAULT 0;
ALTER TABLE claude_sessions ADD COLUMN lines_removed INTEGER DEFAULT 0;
ALTER TABLE claude_sessions ADD COLUMN cc_version TEXT;
