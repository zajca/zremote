-- Add token metrics to agentic_loops
ALTER TABLE agentic_loops ADD COLUMN input_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agentic_loops ADD COLUMN output_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agentic_loops ADD COLUMN cost_usd REAL;
