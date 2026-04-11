CREATE TABLE IF NOT EXISTS agent_profiles (
    id               TEXT PRIMARY KEY,
    name             TEXT NOT NULL,
    description      TEXT,
    agent_kind       TEXT NOT NULL,
    is_default       INTEGER NOT NULL DEFAULT 0,
    sort_order       INTEGER NOT NULL DEFAULT 0,

    model            TEXT,
    initial_prompt   TEXT,
    skip_permissions INTEGER NOT NULL DEFAULT 0,
    allowed_tools    TEXT NOT NULL DEFAULT '[]',
    extra_args       TEXT NOT NULL DEFAULT '[]',
    env_vars         TEXT NOT NULL DEFAULT '{}',

    settings_json    TEXT NOT NULL DEFAULT '{}',

    created_at       TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- One default profile per agent_kind (partial unique index).
CREATE UNIQUE INDEX IF NOT EXISTS agent_profiles_default_per_kind
    ON agent_profiles(agent_kind) WHERE is_default = 1;

-- Name must be unique within a tool; same name allowed across kinds.
CREATE UNIQUE INDEX IF NOT EXISTS agent_profiles_name_per_kind
    ON agent_profiles(agent_kind, name);

CREATE INDEX IF NOT EXISTS agent_profiles_sort_idx
    ON agent_profiles(sort_order, name);

-- Seed a usable first-run default for the `claude` launcher.
INSERT INTO agent_profiles (id, name, description, agent_kind, is_default, sort_order, settings_json)
VALUES (
    lower(hex(randomblob(16))),
    'Default',
    'Plain claude CLI',
    'claude',
    1,
    0,
    '{"development_channels":[],"print_mode":false}'
);
