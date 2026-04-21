# RFC: Authentication Overhaul

**Status:** Draft
**Date:** 2026-04-20
**Author:** team-lead@rfc-auth-overhaul

## Problem Statement

ZRemote ships with a **single shared `ZREMOTE_TOKEN`** used by every agent and the (nonexistent) admin layer, **completely unauthenticated REST endpoints**, and a known hostname-hijack hole (`crates/zremote-server/src/routes/agents/lifecycle.rs:177`). Adding a host requires SSHing into the target machine and pasting env vars by hand.

Since ZRemote grants PTY shell access to remote machines, this posture is unacceptable even for a single-user deployment.

We want auth that is:

1. **Safe** — defensible against the threat model in §Threats.
2. **Frictionless** — "Add host" takes ≤30 s, no SSH, no copy-paste of secrets.
3. **Modern login** — admin token always works (recovery, API), optional OIDC layer for convenient login from new devices without copying tokens.
4. **Single-user** — ZRemote is a single-owner system. No users table, no roles, no ACL, no multi-tenancy.

## Goals

1. Every REST/WS endpoint requires auth; strict 401/403.
2. Single admin authenticates with an admin token, optionally via OIDC pre-configured for one email.
3. Adding a host is one click in the GUI → one one-line install command on the target → agent auto-enrolls and appears online.
4. Per-agent long-lived credentials replace the global shared token; each can be revoked or rotated independently.
5. Audit log of all security-relevant events (forensics).
6. Local/standalone mode remains zero-config and loopback-only, with a local bearer token preventing shared-user escalation.
7. Protocol additions are backward-compatible (old `Register` accepted through one release with deprecation warning).

## Non-Goals

- **Multi-user, RBAC, sharing, ACL.** ZRemote is single-owner.
- **User signup, password reset email flows, MFA**, SAML, LDAP.
- **Telegram-as-actor** — Telegram bot stays notification-only, unchanged.
- **End-to-end encryption of PTY streams** — TLS at transport is the guarantee.
- **mTLS for agent↔server** — signed bearer + ed25519 challenge-response + pinned per-agent public key gives equivalent identity at lower ops cost.

## Architecture

```
+-------------- GUI (zremote-gui) ----------------+
|  LoginScreen                                    |
|    - Admin token field (always available)       |
|    - "Login with OIDC" button (if configured)   |
|    |                                            |
|    | session token (OS keyring)                 |
|    v                                            |
|  Authed views -> REST + WS + ws-tickets         |
+--------------------+----------------------------+
                     |  Authorization: Bearer <session>
                     v
+-------------- zremote-server (Axum) ------------+
|  /api/auth/admin-token   /api/auth/logout       |
|  /api/auth/oidc/init     /api/auth/oidc/callback|
|  /api/auth/ws-ticket     /api/auth/me           |
|  /api/admin/config       (rotate token, OIDC)   |
|  /api/hosts/enrollments  (issue one-time code)  |
|                                                 |
|  auth_mw -> AuthContext{session_id}             |
|  ticket_mw for /ws/events, /ws/terminal/:id     |
|                                                 |
|  admin_config / sessions / hosts / agents       |
|  enrollment_codes / agent_sessions / audit_log  |
+-----+-------------------------------------------+
      | WS /ws/agent  (post-upgrade challenge-response)
      v
+---- zremote-agent (on remote host) -------------+
|  `zremote agent enroll --code ABCD-EFGH ...`    |
|     -> persists agent_id + ed25519 signing key  |
|        in OS keyring (fallback ~/.zremote/      |
|        agent.key, mode 0600)                    |
|  `zremote agent run`                            |
|     -> ed25519 challenge-response on every connect |
|     -> short-lived reconnect_token for fast path|
+-------------------------------------------------+

Local mode: same bearer middleware, local token at ~/.zremote/local.token (0600),
            hard loopback bind unless --allow-remote.
```

## Threats

| ID | Threat | Mitigation (v1) |
|----|--------|-----------------|
| T-1 | Hostname hijack | `hosts.host_fingerprint` (machine-id + MAC) + per-agent `public_key` — server rejects re-enroll on existing fingerprint without explicit revoke |
| T-2 | Token exfiltration from env/logs | OS keyring + 0600 file fallback; no tokens in env/CLI args post-enroll; scrub `Debug`/`Display`; disable core dumps |
| T-3 | Enrollment code brute force | ≥64-bit entropy, 15-min TTL, single-use, 5 attempts/IP/min rate limit |
| T-4 | WS re-auth replay | Server nonce + ±30 s timestamp window + one-shot nonce cache per `agent_id` |
| T-5 | Unauthenticated REST | Bearer-required `auth_mw` on every `/api/*` except `/api/auth/admin-token`, `/api/auth/oidc/*`, `/api/health` |
| T-6 | OIDC abuse | Strict `iss`/`aud`/`nonce`/`exp` + JWKS verify; mandatory PKCE S256; exact-match redirect allowlist; verify configured email matches token claim |
| T-7 | Local-mode escalation by other OS user | Loopback hard bind; per-install `~/.zremote/local.token` (0600) required on every request |
| T-8 | Agent credential leak | Server stores only ed25519 `public_key` — a DB read no longer grants agent impersonation. Signing key never leaves the agent host. Rotation without re-enroll. |
| T-9 | DoS | `tower-governor` rate limits on auth + enroll; per-IP WS caps; body + frame size limits |
| T-10 | Forensics | Append-only audit log of auth/enrollment/PTY events — never log secret values |
| T-11 | Emergency revoke | `zremote admin revoke-host <id>` and `zremote admin rotate-token` invalidate immediately; sessions invalidated on token rotation |

## Design

### 1. Admin authentication

Single owner. Two methods, both produce the same opaque server-side session token.

**Method A — Admin token (always available, the bootstrap path):**

- On first server start, if `admin_config` is empty, generate 32 random bytes (`OsRng`) and base64url-encode. Print the plaintext to **stderr** inside a highly visible banner (bold + heavy-bar framing when stderr is a TTY, plain ASCII otherwise) and emit a single `tracing::info!` line **without the token** so log consumers see the bootstrap event. Do **not** write the plaintext to disk — `logs/` is a scraped directory and persisting secrets there regresses the threat model. A non-interactive bootstrap path is planned for Phase 5 (`zremote admin set-token --from-stdin`); recovery if the banner is missed is `zremote admin rotate-token` also in Phase 5.
- Store SHA-256 hash in `admin_config.token_hash`.
- GUI's setup screen accepts the token, exchanges via `POST /api/auth/admin-token { token }` → `{ session_token, expires_at }`.
- GUI persists `session_token` in OS keyring (`keyring` crate; fallback `~/.config/zremote/credentials.age` with passphrase on headless Linux).
- Token is rotatable: `zremote admin rotate-token` regenerates, prints new value, invalidates all live sessions.

**Method B — OIDC (optional, configured by admin):**

- Admin configures via `/api/admin/config` (auth required, of course): `oidc_issuer_url`, `oidc_client_id`, `oidc_email` (the single allowed email).
- Login flow: GUI → `POST /api/auth/oidc/init` → returns `{ auth_url, state }`. GUI opens system browser. Callback hits `http://127.0.0.1:<ephemeral>/callback` (ad-hoc loopback listener), exchanges code → ID token → server validates `iss`, `aud == client_id`, `nonce`, `exp`, signature via JWKS, **and `email == admin_config.oidc_email`** (constant-time). On success: same opaque session_token returned.
- PKCE S256 mandatory. Exact-match `redirect_uri` allowlist. ID token never accepted as access token.
- Disabling OIDC is just `UPDATE admin_config SET oidc_* = NULL`. Token method always works as fallback.

**Sessions — opaque server-side tokens, not JWT:**

- 32 random bytes → base64url. Stored SHA-256-hashed in `sessions.token_hash`.
- Sent as `Authorization: Bearer <token>`. No cookies in the native GUI.
- Idle expiry: 14 days sliding. Absolute expiry: 90 days. (Hardcoded in v1.)
- Revocation: `DELETE FROM sessions WHERE …`, immediate per-request check. Token rotation purges all rows.

**Multi-server in GUI:** one keyring entry per canonical server URL. `~/.config/zremote/servers.toml` lists known servers (URL + display name), no secrets.

### 2. Host enrollment (agent → server)

**User journey:**

1. In GUI, admin clicks **Add Host** → modal shows `AB12-CD34` (8 chars, Crockford base32, ≥64-bit entropy, 15-min TTL), one-liner, copy button:
   ```
   curl -sSL https://myzremote.example/enroll.sh \
     | ZREMOTE_CODE=AB12-CD34 bash
   ```
2. GUI opens `WS /ws/enrollments/:code` to wait for completion.
3. `enroll.sh` downloads matching-arch `zremote` binary from `/dist/`, installs systemd user unit (or launchd plist on macOS), runs `zremote agent enroll --code ... --server ...` once, then starts `zremote agent run`.
4. `Enroll` (including the agent's ed25519 `public_key`) → `EnrollAck` → agent persists `agent_id`; the ed25519 signing key stays on the agent host via `keyring` (fallback `~/.zremote/agent.key`, 0600).
5. Modal flips to "Host connected: hostname-foo" with green check. **≤30 s** on a warm box.

**Enrollment code:**

- CSPRNG 8-char Crockford base32 (no ambiguous glyphs), argon2id-hashed in DB.
- 15-min TTL, **single-use**. Atomic redemption sets `consumed_at` in a transaction.
- Rate limit: 5 attempts per IP per minute, exponential backoff.

**Per-agent credential (Phase 3 amendment — ed25519):**

- Agent generates an ed25519 keypair on first enroll. Sends the 32-byte `public_key` (base64url) in the `Enroll` message. Server stores it in `agents.public_key` in plaintext — a DB read no longer grants agent impersonation (signing key never leaves the agent host).
- Rotation (server-initiated, over already-authenticated WS): `RotateKey { new_public_key }` → agent generates new keypair, stores new signing key in keyring → `RotateAck { agent_id }` → old row revoked.
- Revocation: GUI per-row button → `agents.revoked_at`, force WS disconnect.

**Host identity (anti-hijack):**

- `host_fingerprint` = stable hash of `machine-id` (Linux) / `IOPlatformUUID` (macOS) + primary MAC, computed by agent on enroll.
- Unique key on `hosts(host_fingerprint)`. Re-enroll on same fingerprint rebinds to same `host_id`. Re-enroll with same hostname but different fingerprint creates a new host — no silent hijack.

### 3. Wire protocol (agent ↔ server)

Post-upgrade challenge-response. All messages JSON, `#[serde(tag="type")]`.

Enrollment (agent sends public key; server stores it, never a secret):
```
Agent  → Server: Enroll { code, hostname, host_fingerprint, agent_version, os, arch, public_key }
Server → Agent:  EnrollAck { agent_id, host_id }
                 | EnrollReject { reason: CodeExpired|CodeAlreadyUsed|CodeUnknown|RateLimited|InvalidPublicKey }
```

Runtime auth on every connection (ed25519 challenge-response, Phase 3 amendment):
```
Agent  → Server: Hello { version, agent_id, nonce_agent }
Server → Agent:  Challenge { nonce_server }
Agent  → Server: AuthResponse { signature }
                 // signature = Sign(ed25519_signing_key,
                 //   b"zremote-agent-auth-v1"   (21 bytes, domain tag)
                 //   || agent_uuid_bytes          (16 bytes)
                 //   || nonce_server_bytes        (32 bytes)
                 //   || nonce_agent_bytes         (32 bytes))
                 //   total payload: 101 bytes
Server → Agent:  AuthSuccess { session_id, reconnect_token }
                 | AuthFailure { reason }   // identical payload for unknown_agent & bad_sig, ≥100ms delay
```

Fast-path reconnect:
```
Agent  → Server: Resume { session_id, reconnect_token }
Server → Agent:  AuthSuccess { ... }   // or fall back to full Hello
```

Key rotation:
```
Server → Agent:  RotateKey { new_public_key }
Agent  → Server: RotateAck { agent_id }
```

**Choices:**
- **ed25519 > HMAC:** server stores only the public key — a DB read no longer grants agent impersonation. HMAC-SHA256 was incompatible with argon2id (one-way hash can't be used to verify a MAC). ed25519 eliminates the asymmetry: verify without the secret.
- **No session-key derivation:** TLS provides confidentiality.
- **Reconnect token:** opaque 32-byte, 1 h TTL, single-use, hashed at rest.
- **Constant-time everywhere:** `subtle::ConstantTimeEq` on every secret comparison. CI grep check forbids `==` on `Vec<u8>`/`[u8]` in `auth/`.

### 4. REST / GUI WebSocket auth

**REST: bearer.** `auth_mw` extracts `Authorization: Bearer <session>`, looks up `sessions`, populates `Extension<AuthContext { session_id, last_seen }>`. Routes register as `Router::new().nest("/api", protected.layer(from_fn_with_state(state, auth_mw)))`.

**WS auth — ticket exchange.** Never accept bearer in query string.

1. `POST /api/auth/ws-ticket` with `{ route, resource_id? }` → `{ ticket, expires_in: 30 }`
2. `GET /ws/terminal/:id` with header `Sec-WebSocket-Protocol: zremote.ticket.<base64url>`
3. Server redeems (single-use, 30 s TTL, bound to session + route + resource), echoes `zremote.ticket.ack` in response.

Tickets: 32 random bytes, SHA-256 hashed.

### 5. Local / standalone mode

- `--bind` parsed: reject non-loopback unless `--allow-remote` explicitly set. Fail loud.
- First run writes `~/.zremote/local.token` (32 random bytes, mode 0600). GUI reads the same file.
- Standalone (`zremote gui --local`) writes the file before spawning agent child; GUI inherits the path.
- Same `auth_mw` runs; bearer comparison constant-time.
- Local mode has no `admin_config` table — the token **is** the credential.
- If `--allow-remote` is set in local mode, refuse to start without `--require-admin-token` (forces creation of `admin_config` and the full server-mode auth) — closes the "accidentally exposed loopback" footgun.

### 6. Schema (new tables — SQLite)

```sql
CREATE TABLE admin_config (
    id              INTEGER PRIMARY KEY CHECK (id = 1),  -- single row
    token_hash      TEXT NOT NULL,                        -- SHA-256(admin_token)
    oidc_issuer_url TEXT,
    oidc_client_id  TEXT,
    oidc_email      TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE sessions (
    id          TEXT PRIMARY KEY,              -- UUID v7
    token_hash  TEXT NOT NULL UNIQUE,          -- SHA-256(session_token)
    created_at  TEXT NOT NULL,
    last_seen   TEXT NOT NULL,
    expires_at  TEXT NOT NULL,                 -- min(created_at + 90d, last_seen + 14d)
    issued_via  TEXT NOT NULL,                 -- 'admin_token' | 'oidc'
    user_agent  TEXT,
    ip          TEXT
);
CREATE INDEX sessions_exp ON sessions(expires_at);

ALTER TABLE hosts ADD COLUMN host_fingerprint TEXT;
CREATE UNIQUE INDEX hosts_fingerprint ON hosts(host_fingerprint);

CREATE TABLE agents (
    id            TEXT PRIMARY KEY,
    host_id       TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    public_key    TEXT NOT NULL,                  -- ed25519 verifying key, base64url(32 bytes)
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
    actor       TEXT NOT NULL,                 -- 'admin' | 'agent:<id>' | 'unknown'
    ip          TEXT,
    event       TEXT NOT NULL,                 -- 'login_ok'|'login_fail'|'enroll_issue'|'enroll_redeem'|'host_revoke'|'token_rotate'|'pty_spawn'
    target      TEXT,                          -- host_id | agent_id | session_id
    outcome     TEXT NOT NULL,                 -- 'ok'|'denied'|'error'
    details     TEXT                           -- JSON, never secrets
);
CREATE INDEX audit_ts ON audit_log(ts);
CREATE INDEX audit_event ON audit_log(event);
```

### 7. Crates

| Crate | Purpose |
|-------|---------|
| `openidconnect` 4.0.1 | OIDC discovery, PKCE, nonce — correct-by-default |
| `argon2` (RustCrypto) 0.5 | Code + secret hashing |
| `hmac` 0.12, `sha2` 0.10 | Agent challenge-response MAC |
| `subtle` 2.x | Constant-time comparisons |
| `rand` with `OsRng` | CSPRNG (never `thread_rng` for secrets) |
| `keyring` 3.x | GUI + agent secret storage, cross-platform |
| `age` 0.10 | Fallback encrypted store on headless Linux |
| `uuid` v7 | Time-ordered IDs |
| `axum-extra` `TypedHeader` | Bearer parsing |
| `tower-governor` | Rate limits on auth + enroll |

Rejected: `jsonwebtoken` (no JWT in v1 — opaque tokens). Prefer `rustcrypto` over `ring` — pure Rust, auditable, no bindgen.

### 8. Protocol versioning

- Bump `AGENT_PROTOCOL_VERSION = 2` in `zremote-protocol`.
- New messages (`Enroll`, `EnrollAck`, `EnrollReject`, `AuthHello`, `AuthChallenge`, `AuthResponse`, `AuthAccepted`, `AuthRejected`, `Resume`, `RotateSecret`, `RotateAck`) — all additive.
- Old `Register { token }` accepted for one release; server logs `warn!(agent_id, "deprecated Register auth, upgrade agent")` once per connection. Removed in next major.

### 9. Migration (existing `ZREMOTE_TOKEN` deployments)

Hybrid path:

- **Local/standalone mode** — auto-migrate. On first v2 server start with no `admin_config`, treat the existing `ZREMOTE_TOKEN` env var (if present) as the initial admin token: hash it, store in `admin_config`, log a warning advising rotation. User notices nothing in the GUI; setup screen is skipped because credentials work immediately.
- **Server mode** — force re-enrollment per agent. Old `Register { token }` keeps working for one release but the GUI marks each such host as `legacy: re-enrollment required`. Admin clicks "Re-enroll" → GUI generates a new code → user runs the one-liner on the host. The legacy `Register` path is removed in the next major.

Rationale: standalone has one box and one user — auto-migration is invisible. Server mode has a real shared secret that may be widely known; forcing per-agent re-enrollment is the cleanest break.

## Phase Plan

This is a team workflow (CLAUDE.md §Implementation Workflow).

### Phase 1 — Foundations

**Files:**
- `crates/zremote-core/migrations/002_auth.sql` — full schema above
- `crates/zremote-core/src/queries/{admin_config,sessions,agents,enrollment,audit}.rs` — CRUD + typed errors
- `crates/zremote-protocol/src/auth.rs` — new message enums (additive)
- `crates/zremote-server/src/auth/{mod,session,bearer,ws_ticket,admin_token}.rs` — server-side auth primitives

**Tests:** migrations round-trip; query isolation (in-memory SQLite); admin token bootstrap + rotation; session sliding/absolute expiry; WS ticket TTL + bind.

**Review:** `rust-reviewer` + `security-reviewer` (mandatory).

### Phase 2 — Auth endpoints + middleware

**Files:**
- `crates/zremote-server/src/auth/oidc.rs` — discovery, PKCE, JWKS verify, email allowlist check
- `crates/zremote-server/src/routes/auth.rs` — `/api/auth/{admin-token,oidc/init,oidc/callback,logout,me,ws-ticket}`
- `crates/zremote-server/src/routes/admin.rs` — `/api/admin/config` (read/update OIDC settings, rotate token)
- `crates/zremote-server/src/auth_mw.rs` — bearer extractor → `Extension<AuthContext>`
- Wire `.layer(auth_mw)` to every `/api/*` except public auth endpoints + `/api/health`.

**Tests:** OIDC flow (httpmock issuer); admin token exchange (ok/bad/locked); rotation invalidates sessions; auth_mw rejects 401; session expiry tested with frozen clock.

**Review:** `rust-reviewer`, `code-reviewer` (wiring), `security-reviewer`.

### Phase 3 — Host enrollment & agent auth

**Phase 3, Chunk 3a (server-side, implemented 2026-04-21) — ed25519 amendment:**

- `crates/zremote-core/migrations/027_agents_public_key.sql` — `ALTER TABLE agents RENAME COLUMN secret_hash TO public_key`
- `crates/zremote-core/src/queries/agents.rs` — `public_key` field; `mint_agent_session`; `update_public_key`; `create_rotated`
- `crates/zremote-protocol/src/auth.rs` — `AgentAuthMessage` / `ServerAuthMessage` enums; `build_auth_payload`; `AUTH_PAYLOAD_TAG`
- `crates/zremote-server/src/auth/agent_auth.rs` — `authenticate_agent`; `reject_after`; oracle-collapse latency floor (≥100 ms)
- `crates/zremote-server/src/routes/enrollment.rs` — `POST /api/admin/enroll/create`; `POST /api/enroll` (argon2id code verify; ed25519 key validate; upsert host; mint session)
- `crates/zremote-server/src/routes/agents/lifecycle.rs` — `register_agent_dispatch` v2/v1 shim; `register_agent_v2`; `register_agent_legacy`

**Phase 3, Chunk 3b (agent-side, pending):**
- `crates/zremote-agent/src/enroll.rs` — `agent enroll` subcommand; keyring write
- `crates/zremote-agent/src/connection/auth.rs` — ed25519 challenge-response client
- `crates/zremote-agent/src/config.rs` — read `agent_id` + signing key from keyring/file
- `crates/zremote-server/public/enroll.sh` — install script served at `/enroll.sh`

**Tests (3a):** code TTL & single-use; argon2id verify; ed25519 sign/verify; tampered sig; wrong agent_id; oracle-collapse token mapping; enrollment 400/201 paths; agent session minting.

**Review:** `rust-reviewer`, `security-reviewer` (focus T-1, T-3, T-4, T-8).

### Phase 4 — GUI login & Add Host

**Files:**
- `crates/zremote-gui/src/views/login.rs` — admin token field, "Login with OIDC" button (only if server reports OIDC configured via public `/api/auth/me/methods`)
- `crates/zremote-gui/src/views/hosts/add_host_modal.rs` — enrollment flow with live WS, copy-paste one-liner, status checkmark
- `crates/zremote-gui/src/auth/keyring.rs` — bearer storage with `age` fallback, multi-server keying
- `crates/zremote-gui/src/persistence.rs` — `servers.toml` schema, no secrets
- Router: protect all authed views; show login if no session.

**Tests:** visual via `/visual-test`; unit-test credential storage round-trip with mock keyring backend; modal state machine (pending → connected → error → timeout).

**Review:** `rust-reviewer`, UX review teammate.

### Phase 5 — Audit + admin CLI

**Files:**
- `crates/zremote-core/src/queries/audit.rs` — `log_event` helper invoked at every security boundary
- Wiring: every dispatch path (`spawn_pty`, `enroll`, `revoke`, `rotate-token`, login attempts) calls `audit::log_event`
- Admin CLI: `zremote admin {rotate-token, set-oidc, clear-oidc, revoke-host, revoke-session, list-sessions, list-hosts, audit-tail}`

**Tests:** audit row written for every event type; CLI commands hit DB correctly; rotate-token invalidates sessions live.

**Review:** `security-reviewer`.

### Phase 6 — Local mode hardening

**Files:**
- `crates/zremote-agent/src/local/mod.rs` — loopback hard check, `~/.zremote/local.token` at first run
- `crates/zremote-gui/src/local.rs` — read `local.token`, send as bearer
- Standalone spawn (`zremote gui --local`) — write + inherit token
- `--allow-remote` requires `--require-admin-token`

**Tests:** reject non-loopback bind without flag; reject requests missing token; `--allow-remote` requires admin token mode.

**Review:** `security-reviewer` (T-7).

### Phase 7 — Docs, migration guide, release

- `docs/auth.md` user-facing guide: enroll a host, configure OIDC, rotate admin token, revoke, emergency procedures
- `docs/admin.md` CLI reference
- RELEASE notes with upgrade order (server first per CLAUDE.md), `ZREMOTE_TOKEN` deprecation timeline, migration paths (auto for local, force re-enroll for server)

## Risk Assessment

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| `keyring` unavailable on headless Linux | High | Medium | `age` encrypted file fallback designed in; document UX |
| Server mode upgrade pain (force re-enroll) | Medium | Medium | Old `Register` accepted for one release; clear GUI signal "legacy: re-enroll"; one-click re-enroll flow |
| OIDC callback port blocked by local firewall | Low | Medium | Document; admin token always works as fallback; device-code is v2 |
| Clock skew on agent machines | Medium | Low | ±30 s window; if exceeded, error mentions clock sync |
| Admin loses both token AND OIDC access | Low | High | `zremote admin rotate-token` from any shell on the server host as recovery; documented runbook |

## Acceptance

- All `/api/*` endpoints reject unauthenticated requests.
- Admin can log in with admin token alone OR with OIDC (when configured).
- Add Host = ≤30 s, no env vars, no secrets typed by user on the target host.
- Per-agent credentials revoked/rotated independently from the GUI.
- Audit log entry exists for every event listed in T-10.
- Local mode auto-migrates `ZREMOTE_TOKEN` invisibly; server mode marks legacy hosts and offers one-click re-enrollment.
- Old `Register { token }` agents still connect for one release; deprecation warning logged per connection.

## Amendment: Phase 3 — ed25519 replaces HMAC (2026-04-21)

The original §2/§3 design specified `agent_secret` (32-byte random, base64url) stored argon2id-hashed in `agents.secret_hash`, authenticated via HMAC-SHA256. This was internally inconsistent: argon2id is a one-way function; the server cannot re-derive the secret from the stored hash to compute or verify a MAC.

**Resolution:** replaced with ed25519 asymmetric signature scheme.

- Agent generates an ed25519 keypair at enroll time. The signing key never leaves the agent host (OS keyring / 0600 file fallback).
- The 32-byte `public_key` (base64url) is sent in `Enroll` and stored in `agents.public_key` in plaintext. A database read no longer grants agent impersonation (eliminates T-8 entirely).
- Runtime auth: `Hello` → `Challenge { nonce_server }` → `AuthResponse { signature }`. Signed payload: `AUTH_PAYLOAD_TAG (21B) || agent_uuid_bytes (16B) || nonce_server (32B) || nonce_agent (32B)` = 101 bytes fixed-width. No length-confusion attack surface.
- Migration 027: `ALTER TABLE agents RENAME COLUMN secret_hash TO public_key`.
- `rand_core` version conflict (`argon2` uses rand_core 0.6, workspace rand uses 0.9): resolved by using `argon2::password_hash::rand_core::OsRng` for argon2id salt generation and `rand_core = "0.6"` dev-dep for ed25519-dalek tests in `zremote-protocol`.
