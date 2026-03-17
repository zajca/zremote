ALTER TABLE knowledge_bases ADD COLUMN memories_since_regen INTEGER NOT NULL DEFAULT 0;
ALTER TABLE knowledge_bases ADD COLUMN last_regenerated_at TEXT;
ALTER TABLE knowledge_bases ADD COLUMN last_claude_md_hash TEXT;
