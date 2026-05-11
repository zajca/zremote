-- Seed a first-run default for the `codex` launcher. Existing installs that
-- already created a codex profile keep their current setup.
INSERT INTO agent_profiles (
    id,
    name,
    description,
    agent_kind,
    is_default,
    sort_order,
    settings_json
)
SELECT
    lower(hex(randomblob(16))),
    'Default',
    'Plain codex CLI',
    'codex',
    1,
    0,
    '{"no_alt_screen":true}'
WHERE NOT EXISTS (
    SELECT 1 FROM agent_profiles WHERE agent_kind = 'codex'
);
