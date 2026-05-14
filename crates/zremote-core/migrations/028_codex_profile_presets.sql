-- Seed useful Codex presets. Existing profiles with the same per-kind name are
-- left untouched so user edits and earlier local experiments win.
INSERT INTO agent_profiles (
    id,
    name,
    description,
    agent_kind,
    is_default,
    sort_order,
    initial_prompt,
    skip_permissions,
    settings_json
)
SELECT
    lower(hex(randomblob(16))),
    'Review',
    'Read-only Codex review profile',
    'codex',
    0,
    10,
    'Review the current worktree for bugs, regressions, missing tests, and risky assumptions. Report findings first with file and line references.',
    0,
    '{"sandbox":"read-only","approval_policy":"on-request","no_alt_screen":true}'
WHERE NOT EXISTS (
    SELECT 1 FROM agent_profiles WHERE agent_kind = 'codex' AND name = 'Review'
);

INSERT INTO agent_profiles (
    id,
    name,
    description,
    agent_kind,
    is_default,
    sort_order,
    skip_permissions,
    settings_json
)
SELECT
    lower(hex(randomblob(16))),
    'Implement',
    'Workspace-write Codex implementation profile',
    'codex',
    0,
    20,
    0,
    '{"sandbox":"workspace-write","approval_policy":"on-request","no_alt_screen":true}'
WHERE NOT EXISTS (
    SELECT 1 FROM agent_profiles WHERE agent_kind = 'codex' AND name = 'Implement'
);

INSERT INTO agent_profiles (
    id,
    name,
    description,
    agent_kind,
    is_default,
    sort_order,
    skip_permissions,
    settings_json
)
SELECT
    lower(hex(randomblob(16))),
    'Autonomous',
    'Workspace-write Codex profile that asks after failures',
    'codex',
    0,
    30,
    0,
    '{"sandbox":"workspace-write","approval_policy":"on-failure","no_alt_screen":true}'
WHERE NOT EXISTS (
    SELECT 1 FROM agent_profiles WHERE agent_kind = 'codex' AND name = 'Autonomous'
);

INSERT INTO agent_profiles (
    id,
    name,
    description,
    agent_kind,
    is_default,
    sort_order,
    skip_permissions,
    settings_json
)
SELECT
    lower(hex(randomblob(16))),
    'Full Trust',
    'High-trust Codex profile that bypasses approvals and sandboxing',
    'codex',
    0,
    40,
    1,
    '{"no_alt_screen":true}'
WHERE NOT EXISTS (
    SELECT 1 FROM agent_profiles WHERE agent_kind = 'codex' AND name = 'Full Trust'
);
