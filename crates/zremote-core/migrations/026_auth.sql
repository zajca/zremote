-- RFC: docs/rfc/rfc-auth-overhaul.md §6 — auth foundations schema.

CREATE TABLE admin_config (
    id              INTEGER PRIMARY KEY CHECK (id = 1),  -- single row
    token_hash      TEXT NOT NULL,                        -- SHA-256(admin_token)
    oidc_issuer_url TEXT,
    oidc_client_id  TEXT,
    oidc_email      TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE auth_sessions (
    id          TEXT PRIMARY KEY,              -- UUID v7
    token_hash  TEXT NOT NULL UNIQUE,          -- SHA-256(session_token)
    created_at  TEXT NOT NULL,
    last_seen   TEXT NOT NULL,
    expires_at  TEXT NOT NULL,                 -- min(created_at + 90d, last_seen + 14d)
    issued_via  TEXT NOT NULL CHECK (issued_via IN ('admin_token', 'oidc')),
    user_agent  TEXT,
    ip          TEXT
);
CREATE INDEX auth_sessions_exp ON auth_sessions(expires_at);

ALTER TABLE hosts ADD COLUMN host_fingerprint TEXT;
CREATE UNIQUE INDEX hosts_fingerprint ON hosts(host_fingerprint)
    WHERE host_fingerprint IS NOT NULL;

CREATE TABLE agents (
    id            TEXT PRIMARY KEY,
    host_id       TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    secret_hash   TEXT NOT NULL,                  -- argon2id
    created_at    TEXT NOT NULL,
    last_seen     TEXT,
    revoked_at    TEXT,
    rotated_from  TEXT REFERENCES agents(id)
);
CREATE INDEX idx_agents_host_active ON agents(host_id) WHERE revoked_at IS NULL;

CREATE TABLE enrollment_codes (
    code_hash            TEXT PRIMARY KEY,    -- argon2id
    scope                TEXT NOT NULL DEFAULT 'host',
    expires_at           TEXT NOT NULL,
    consumed_at          TEXT,
    consumed_by_agent_id TEXT REFERENCES agents(id)
);

CREATE TABLE agent_sessions (
    id                   TEXT PRIMARY KEY,
    agent_id             TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    reconnect_token_hash TEXT NOT NULL,        -- SHA-256
    expires_at           TEXT NOT NULL
);

CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          TEXT NOT NULL,
    actor       TEXT NOT NULL,
    ip          TEXT,
    event       TEXT NOT NULL,
    target      TEXT,
    outcome     TEXT NOT NULL CHECK (outcome IN ('ok', 'denied', 'error')),
    details     TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX audit_ts ON audit_log(ts);
CREATE INDEX audit_event ON audit_log(event);
