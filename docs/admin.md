# `zremote admin` — CLI reference

`zremote admin` is the direct-to-database administrative surface for the
server. It runs on the server host, opens the SQLite database named by
`DATABASE_URL` (or `--database-url`), and performs a single targeted write.
Every subcommand emits an `audit_log` row with `actor = "cli:<system-user>"`
so CLI actions stay forensically distinct from GUI/HTTP admin actions.

For user-facing auth tasks (login, enrollment, rotation) see
[`docs/auth.md`](auth.md). The design is in
[`docs/rfc/rfc-auth-overhaul.md`](rfc/rfc-auth-overhaul.md).

## Synopsis

```
zremote admin <SUBCOMMAND> [--database-url URL]

SUBCOMMANDS:
  rotate-token            Rotate the admin token
  set-oidc                Configure OIDC login (issuer, client id, email)
  clear-oidc              Remove OIDC configuration
  revoke-host             Revoke every credential for a host
  revoke-session          Revoke one admin session
  list-sessions           List live admin sessions
  list-hosts              List enrolled hosts
  audit-tail              Print recent audit-log rows
```

## Global flags

| Flag              | Env           | Default              | Description |
|-------------------|---------------|----------------------|-------------|
| `--database-url`  | `DATABASE_URL`| `sqlite:zremote.db`  | SQLite connection string. The default path is relative to the CWD — a stderr warning is printed the first time it fires. |

Every subcommand runs in one connection and exits with status `0` on success,
`1` on any error. Errors go to stderr.

## Subcommand reference

### `rotate-token`

Generate a fresh admin token, store its Argon2id hash, invalidate every live
session, print the plaintext token once to stderr. The command succeeds even
if no previous admin token existed — use this to bootstrap the first admin
after initial server install.

```sh
zremote admin rotate-token
```

**Output:**
```
new admin token: <64 base64url chars>
(store this now — it will not be shown again)
```

**Side effects:**
- `admin_config.admin_token_hash` is overwritten.
- Every row in `auth_sessions` is marked revoked.
- `audit_log` entry `admin_token_rotate` with outcome `ok`.

### `set-oidc`

Configure OIDC-based admin login. The issuer must be reachable from the
server at command time — the command fails fast if the discovery document
(`<issuer>/.well-known/openid-configuration`) is unreachable. Only the email
you set here will be allowed to complete an OIDC login.

```sh
zremote admin set-oidc \
  --issuer https://accounts.google.com \
  --client-id 1234.apps.googleusercontent.com \
  --email you@example.com
```

| Flag           | Required | Description |
|----------------|----------|-------------|
| `--issuer`     | yes      | OIDC issuer URL. Must be `https://`. |
| `--client-id`  | yes      | OIDC client id registered with the issuer. |
| `--email`      | yes      | Admin email. The only principal allowed to complete OIDC login. |

**Output:** `OIDC configured for <email> @ <issuer>` on success.

**Audit:** `admin_oidc_set` with the redacted issuer/email in `details`.

### `clear-oidc`

Remove OIDC configuration. Reverts to admin-token-only login. Every live OIDC
session is revoked in the same transaction so a stolen ID token cannot
outlive the config.

```sh
zremote admin clear-oidc
```

**Audit:** `admin_oidc_clear`.

### `revoke-host`

Mark every non-revoked agent credential for one host as revoked. The next
reconnect attempt from that host is refused; any live WebSocket is closed.

```sh
zremote admin revoke-host --host my-laptop
zremote admin revoke-host --host 0199b1d8-5bd4-7a2e-bdfb-6b3a53a1c3d1
```

| Flag       | Required | Description |
|------------|----------|-------------|
| `--host`   | yes      | Host UUID, hostname, or configured name. Lookup order: UUID → configured name → hostname. |

**Audit:** `admin_host_revoke` with the resolved host UUID in `target`.

### `revoke-session`

Invalidate one admin session without touching the admin token.

```sh
zremote admin revoke-session --session 0199b2a5-5bd4-7a2e-bdfb-6b3a53a1c3d1
```

| Flag          | Required | Description |
|---------------|----------|-------------|
| `--session`   | yes      | Session UUID from `list-sessions`. |

**Audit:** `admin_session_revoke` with the session UUID in `target`.

### `list-sessions`

Print a table of every live admin session.

```sh
zremote admin list-sessions
```

**Columns:** `id`, `method` (`admin_token` or `oidc`), `created_at`, `expires_at`,
`last_used_at`, `user_agent`, `ip`.

### `list-hosts`

Print a table of every enrolled host and its live agent credential count.

```sh
zremote admin list-hosts
```

**Columns:** `id`, `hostname`, `configured_name`, `live_agents`, `last_seen`.

### `audit-tail`

Print the most recent `audit_log` rows. Useful during incident response.

```sh
zremote admin audit-tail --limit 200
zremote admin audit-tail --limit 200 --event pty_spawn
zremote admin audit-tail --limit 50  --event login_fail
```

| Flag      | Default | Description |
|-----------|---------|-------------|
| `--limit` | `50`    | Maximum number of rows. |
| `--event` | —       | Filter by event name. See [event catalogue](#audit-event-catalogue). |

Rows are printed most-recent first.

## Audit event catalogue

Every security-relevant action writes one audit row. Fields:

- `ts` — ISO-8601 UTC timestamp.
- `actor` — session UUID for HTTP admin, `cli:<user>` for CLI, `agent:<uuid>` for agent-originated events, `oidc:<email>` for OIDC.
- `ip` — source IP, or `null` for CLI/agent-originated events.
- `event` — the event name below.
- `target` — the thing acted on (session UUID, host UUID, agent UUID, etc.) or `null`.
- `outcome` — `ok`, `denied`, or `error`.
- `details` — JSON object; never contains plaintext tokens.

| Event                      | Actor kind       | Meaning |
|----------------------------|------------------|---------|
| `login_ok`                 | session          | Admin login (admin-token or OIDC) succeeded. |
| `login_fail`               | `anon`           | Login attempt failed (wrong token, unknown email, OIDC error). |
| `token_rotate`             | session / cli    | Admin token rotated. Invalidates every session. |
| `set_oidc_config`          | session / cli    | OIDC configuration written. |
| `clear_oidc_config`        | session / cli    | OIDC configuration removed. |
| `session_revoke`           | session / cli    | One session revoked. |
| `host_revoke`              | session / cli    | Every agent credential for a host revoked. |
| `enroll_created`           | session / cli    | Enrollment code generated. |
| `enroll_used`              | agent            | Agent redeemed an enrollment code and was issued credentials. |
| `enroll_failed_code`       | `anon`           | Enrollment redemption failed: invalid/expired/consumed code. |
| `enroll_failed_race`       | `anon`           | Enrollment redemption failed: lost a race against a concurrent redeem on the same code. |
| `agent_auth_ok`            | agent            | Agent completed the ed25519 challenge and was issued a session. |
| `agent_auth_failed_*`      | agent            | Agent ed25519 challenge failed (variant-specific suffix records which step). |
| `pty_spawn`                | session / agent  | A terminal session was spawned. |

## Exit codes

| Code | Meaning |
|------|---------|
| `0`  | Success. |
| `1`  | Any runtime error (DB unreachable, invalid input, row not found, IO error). |
| `2`  | Invalid CLI usage caught by `clap` before the command runs. |

## Examples

**Bootstrap a fresh server:**
```sh
DATABASE_URL=sqlite:/var/lib/zremote/zremote.db zremote admin rotate-token
```

**Revoke every host named "staging-*":**
```sh
for h in $(zremote admin list-hosts --database-url sqlite:/var/lib/zremote/zremote.db \
             | awk '/staging-/ {print $1}'); do
  zremote admin revoke-host --host "$h"
done
```

**Tail authentication failures in real time:**
```sh
while true; do
  zremote admin audit-tail --limit 20 --event login_fail
  sleep 10
done
```

## See also

- [`docs/auth.md`](auth.md) — user-facing guide (login, enrollment, rotation).
- [`docs/rfc/rfc-auth-overhaul.md`](rfc/rfc-auth-overhaul.md) — design & threat model.
