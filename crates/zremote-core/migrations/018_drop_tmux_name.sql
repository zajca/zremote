-- Remove unused tmux_name column (tmux backend removed in 0.7.6).
ALTER TABLE sessions DROP COLUMN tmux_name;
