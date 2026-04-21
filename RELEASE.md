# Release Notes

## Unreleased — Authentication overhaul

This release replaces the pre-auth-overhaul shared-token scheme with a
proper single-owner auth system: admin login (token + optional OIDC),
per-agent ed25519 credentials with one-click enrollment, and a loopback-
hardened local mode. Full design in
[`docs/rfc/rfc-auth-overhaul.md`](docs/rfc/rfc-auth-overhaul.md).

### Highlights

- **Every REST/WS endpoint now requires auth.** The old "nonexistent admin
  layer, everything wide open" posture is gone.
- **Single admin, two login paths.** Admin token (always available) plus
  optional OIDC for convenient login from new devices.
- **One-click host enrollment.** GUI → **Hosts → Add host** produces a
  one-line install command with a single-use 10-minute enrollment code. The
  agent generates its own ed25519 keypair and is issued per-agent credentials
  on enrollment.
- **Ed25519 challenge-response on every agent reconnect.** Stolen agent
  credentials cannot be replayed.
- **Audit log.** Every login, enrollment, revoke, rotate, and PTY spawn is
  recorded and queryable via `zremote admin audit-tail`.
- **Admin CLI.** `zremote admin {rotate-token, set-oidc, clear-oidc,
  revoke-host, revoke-session, list-sessions, list-hosts, audit-tail}` —
  recovery path when the GUI is unreachable. See
  [`docs/admin.md`](docs/admin.md).
- **Local mode hardening.** Defaults to loopback-only. A per-agent bearer
  in `~/.zremote/local.token` (mode `0o600`) is now required on every
  `/api/*` and `/ws/*` call. Exposing the local API to the network requires
  `--allow-remote --require-admin-token`; the startup validator rejects
  either flag alone.

### New user-facing docs

- [`docs/auth.md`](docs/auth.md) — login, enrollment, rotation, revocation,
  emergency procedures.
- [`docs/admin.md`](docs/admin.md) — CLI reference and audit event catalogue.

### Upgrade order

Deploy in the order below. Agents auto-reconnect with backoff, so daemon
sessions survive the server restart window.

1. **Server first.** The new server still accepts the legacy
   `Register { token }` handshake from old agents **for this release only**
   so existing agents do not drop offline during the rolling upgrade. Each
   acceptance logs a deprecation warning.
2. **Agents rolling, one at a time.** Verify the host returns online in the
   GUI before moving to the next. No manual action is required on the
   agent host: the new agent binary preserves the old `ZREMOTE_TOKEN` path
   for this release.
3. **Admin bootstrap.** Once the server is up, run on the server host:
   ```sh
   zremote admin rotate-token
   ```
   Copy the printed plaintext admin token into a password manager. Log into
   the GUI with **Admin token**.
4. **Optional: enable OIDC.**
   ```sh
   zremote admin set-oidc --issuer <URL> --client-id <ID> --email <you@…>
   ```
5. **Re-enroll agents.** For each existing host, **Hosts → Add host** in the
   GUI and run the printed one-liner on the target machine. This replaces
   the legacy shared-token credential with per-agent ed25519 keys.

### Deprecations

- **`ZREMOTE_TOKEN` environment variable** is deprecated. The next release
  (one release after this one) removes:
  - Server acceptance of the legacy `Register { token }` handshake.
  - Agent fallback to `ZREMOTE_TOKEN` when no ed25519 key is present.

  Until then both paths keep working so you are not forced to re-enroll
  every host in one window. Budget for the re-enrollment before the next
  release.

- **Server `--token` / `ZREMOTE_TOKEN`** for starting the server remain
  in place for **this release** only — the server accepts but ignores it
  when the new admin-config row exists. After the next release it will be
  a hard error to set the variable. The migration path is to remove the
  variable and run `zremote admin rotate-token` to bootstrap the admin
  token.

### Migration paths

| Mode        | What changes                                            | Manual action |
|-------------|---------------------------------------------------------|---------------|
| Standalone  | Agent generates `~/.zremote/local.token` on first start | None — GUI reads it automatically. |
| Local       | Same as standalone + optional `--allow-remote` gate     | Drop the `ZREMOTE_TOKEN` env var once upgraded. `--allow-remote` now requires `--require-admin-token`. |
| Server      | Admin login + per-agent ed25519 enrollment              | Rotate the admin token on the server host, then re-enroll each host from the GUI before the deprecation window closes. |

### Breaking changes

- **Local mode CLI:** `--allow-remote` without `--require-admin-token` now
  exits `2` before any database or filesystem work. The startup validator
  surfaces this instead of silently binding to a routable interface with
  no auth.
- **Server routing:** every REST endpoint under `/api/*` except
  `/health`, `/api/mode`, `/api/auth/admin-token`, `/api/auth/oidc/*`, and
  `/api/enroll` now requires a valid `Authorization: Bearer <session>`.
  Tooling that scraped the old unauthenticated endpoints must now log in.
- **WebSocket tickets:** `/ws/terminal/{id}` and `/ws/events` require a
  short-lived WS ticket from `POST /api/auth/ws-ticket`. The GUI handles
  this transparently; external WebSocket clients must mint tickets.
- **Audit schema:** new `audit_log` table is created by migration. Existing
  databases are migrated in place; no rollback without backing up first.

### Security properties

- **No plaintext admin token on disk.** Argon2id hash only; rotation prints
  the new plaintext to stderr exactly once and the response body has
  `Cache-Control: no-store`.
- **Enrollment-code oracle collapse.** Expired, already-consumed, and
  unknown codes all return an identical opaque `enrollment_failed` with
  the same minimum wall-clock latency.
- **Constant-time comparisons** (`subtle::ConstantTimeEq`) on every token
  verification.
- **Per-agent private keys on disk with mode `0o600`**, never transmitted
  after enrollment. Only the public key is stored server-side.
- **No secrets in logs.** The HTTP tracing layer strips the query string
  from `uri`, the local-mode bearer is never included in the make-span,
  and every debug format on `AdminToken`/`EnrollmentCode` prints
  `<redacted>`.
- **OIDC restricted to one email.** `sub`/`email` claim mismatch is a hard
  reject; every attempt is audited.

### Rate limits

- Auth and enrollment routes are globally capped at **10 req / min / IP**
  (via `tower_governor`), catching both brute-force admin-token attempts
  and enrollment-code guessing in the same limiter.
- WS-ticket issuance is inside the authed rate-limit layer — a stolen
  session bearer cannot amplify a DoS past this ceiling.

### Known issues

- `pty_spawn` audit rows currently log `ip = null` when the originating
  request came from a session. The `ConnectInfo` extractor in axum 0.8 is
  non-optional and would panic in existing oneshot tests; the IP field
  will be populated in a follow-up release.
- Legacy `Register { token }` acceptance emits a `tracing::warn!` but is
  not yet rate-limited separately from enrollment. An attacker with the
  leaked shared `ZREMOTE_TOKEN` can still reconnect until the next
  release removes the handler entirely. Rotate the shared token, upgrade
  as above, and re-enroll promptly.

### Rollback

If you need to roll back, restore the server's SQLite database from a
pre-upgrade backup before downgrading the binary. The migrations are
append-only; downgrading the binary without restoring the database will
fail at startup because the new `audit_log`, `admin_config`,
`auth_sessions`, and `enrollment_codes` tables are present. See the
migration files in `crates/zremote-core/migrations/` for the exact schema.
