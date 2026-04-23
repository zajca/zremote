# Authentication

ZRemote is a single-owner system: one admin, any number of agents. This guide
covers every day-to-day auth task: logging in, enrolling a new host, rotating
credentials, revoking access, and recovering from a lost admin token.

For the CLI reference see [`docs/admin.md`](admin.md). For the design rationale
and threat model see [`docs/rfc/rfc-auth-overhaul.md`](rfc/rfc-auth-overhaul.md).

## Quick reference

| Task                         | Command / UI action |
|------------------------------|---------------------|
| First-time server setup      | `zremote admin rotate-token` then copy the printed token into the GUI |
| Add a host (from GUI)        | **Hosts → Add host** → copy the one-line install command to the target |
| Rotate the admin token       | `zremote admin rotate-token` (also available as **Settings → Rotate admin token** in the GUI) |
| Configure OIDC login         | `zremote admin set-oidc --issuer … --client-id … --email …` |
| Disable OIDC login           | `zremote admin clear-oidc` |
| Revoke a host                | `zremote admin revoke-host --host <uuid-or-name>` |
| Revoke one browser session   | `zremote admin revoke-session --session <uuid>` |
| See active sessions          | `zremote admin list-sessions` |
| See enrolled hosts           | `zremote admin list-hosts` |
| Tail the audit log           | `zremote admin audit-tail --limit 100` |

## The three modes

ZRemote runs in three modes. Auth rules differ by mode:

| Mode        | Where auth lives                            | Who can connect |
|-------------|---------------------------------------------|-----------------|
| Standalone  | Per-agent `~/.zremote/local.token`          | The same local user |
| Local       | Same as standalone, plus optional `--allow-remote` | Loopback only by default |
| Server      | Admin token + OIDC + per-agent ed25519 keys | Any agent that completes enrollment; anyone with the admin token |

Standalone and local use the same mechanism: a 32-byte bearer written to
`~/.zremote/local.token` on first start with mode `0o600`. The GUI reads the
token from disk and sends it on every request.

## Logging into the GUI (server mode)

1. Launch the GUI: `zremote gui --server http://your-server:3000`
2. You will see the **Login** screen. Two options:
   - **Admin token** — always available. Paste the token you generated with
     `zremote admin rotate-token` (or the bootstrap token from first-time
     setup).
   - **Login with OIDC** — shown only if OIDC has been configured and your
     admin email matches. Opens the OIDC provider in your browser, redirects
     back to the GUI, and logs you in.
3. The session token is stored in the OS keyring (libsecret on Linux, Keychain
   on macOS). Logging out wipes it.

**Session lifetime:** sliding 14-day idle window, 90-day absolute ceiling.
Every request touches the session and pushes the idle deadline forward, but
the session can never outlive its creation by more than 90 days. Sessions
become invalid immediately after `rotate-token` or `revoke-session`.

**Session expiry UX:** if the session expires while the GUI is open you will
be bounced to the login screen with the error *Session expired — please log
in again*. No state is lost.

## First-time server setup

1. Start the server with **no** `ZREMOTE_TOKEN` environment variable
   (that variable is deprecated — see [Migrating](#migrating-from-zremote_token)).
2. On the server host, generate the first admin token:
   ```sh
   zremote admin rotate-token
   ```
   The plaintext token is printed **once** to stderr. Copy it to a password
   manager. It is not stored in plaintext anywhere after this — only its
   Argon2id hash is in the database.
3. Open the GUI, point it at your server, paste the token into the **Admin
   token** field, and click **Log in**.
4. Optional: configure OIDC for more convenient login from new devices:
   ```sh
   zremote admin set-oidc \
     --issuer https://accounts.google.com \
     --client-id 1234.apps.googleusercontent.com \
     --email you@example.com
   ```
   Only the email you set here will be allowed to complete an OIDC login.

## Enrolling a host

Hosts are machines that run an agent and appear as tabs in the GUI. Enrollment
is a one-click flow:

1. In the GUI: **Hosts → Add host**. The server generates a fresh enrollment
   code (10-minute TTL, single-use) and prints a one-line install command.
2. On the target machine, run the printed command. It looks like:
   ```sh
   ZREMOTE_ENROLL_CODE=abcdef0123456789 \
     zremote agent enroll \
     --server https://your-server:3000
   ```
   The code is passed via environment variable, not argv, so it does not leak
   into process listings or shell history (the command is deliberately written
   as `ENV=... zremote agent enroll`, not as `zremote agent enroll --code …`).
3. The agent:
   - generates an ed25519 keypair,
   - writes the signing key to `~/.zremote/agent.key` (mode `0o600`),
   - redeems the enrollment code over HTTPS,
   - receives a persistent `agent_id` from the server,
   - immediately completes a challenge-response handshake to prove it holds
     the signing key, and
   - connects as the new host.
4. The host appears in the GUI within a second or two. If anything fails the
   agent prints a diagnostic line and exits non-zero — the enrollment code is
   still single-use, so you can trigger a fresh one from the GUI and try again.

**Code TTL:** 10 minutes. Expired codes return the same opaque
`enrollment_failed` response as invalid codes — there is no oracle for code
validity.

**Key location:** override with `--key-file /path/to/key`. The default is
`~/.zremote/agent.key`.

**Re-enrolling a host** (e.g. after a disk replacement): revoke the old agent
first (`zremote admin revoke-host --host <uuid-or-name>`), then enroll again
from the GUI. Both the old and new agents would have the same hostname — the
revoke step is what forces the old credential offline.

## Rotating the admin token

Rotation invalidates **every live admin session** (including the one you may
be using in the GUI) and prints a fresh plaintext token exactly once.

```sh
zremote admin rotate-token
```

Rotate whenever:
- The token may have leaked (pasted into a log, committed to git, etc.).
- An admin-session device is lost.
- You bring OIDC online for the first time (defence in depth).
- Periodically on your own schedule (no hard TTL is enforced).

The response has `Cache-Control: no-store` and is never logged. Copy it into
your password manager immediately — there is no way to read it again from the
server.

**In the GUI:** Settings → Rotate admin token shows a confirm dialog, then
displays the plaintext token in a modal with a Copy button. Closing the modal
logs you out; log in again with the new token.

## Revoking access

### Revoke a host

Marks every agent credential for that host as revoked. The next time the
agent reconnects it will be refused and must re-enroll.

```sh
zremote admin revoke-host --host my-laptop       # by configured name
zremote admin revoke-host --host workstation     # by hostname
zremote admin revoke-host --host 0199b1d8-…      # by UUID (see list-hosts)
```

Any live WebSocket connection from that host is dropped immediately.

### Revoke a session

Invalidates one GUI session without touching the admin token. Useful if a
browser or laptop is lost but you do not want to force every other device to
log in again.

```sh
zremote admin list-sessions
zremote admin revoke-session --session 0199b2a5-…
```

### Disable OIDC

```sh
zremote admin clear-oidc
```

Reverts to admin-token-only login. Existing OIDC-issued sessions are revoked
in the same transaction.

## Emergency procedures

### Lost admin token

Run `zremote admin rotate-token` on the server host. The CLI talks directly
to the SQLite database — no session is required. Copy the new token and log
in again.

If you also lost shell access to the server host, recover that first (there
is no remote recovery path — this is a deliberate safety property: whoever
owns the DB file is the owner of the system).

### Suspected compromise

In order:

1. `zremote admin rotate-token` — invalidates every live admin session.
2. `zremote admin list-hosts` — note any host you do not recognise.
3. `zremote admin revoke-host --host <each-unknown>` — kill their agents.
4. `zremote admin list-sessions` — revoke any session older than your last
   intentional login.
5. `zremote admin audit-tail --limit 500` — review recent events; every login,
   enrollment, revoke, and `pty_spawn` is recorded.
6. `zremote admin set-oidc --email …` or `clear-oidc` — if OIDC may be part
   of the compromise vector, tighten it or turn it off.

### Database file compromised

The database stores only hashes:
- Admin token → Argon2id hash.
- Enrollment codes → Argon2id hash (rows expire after 10 minutes anyway).
- Session bearers → SHA-256 hash.
- Agent public keys → stored in clear (they are public).

No plaintext secret is on disk. A stolen database file still requires a
working admin token to authenticate new sessions. Rotate the admin token
anyway — the threat model is *"recoverable"*, not *"uncompromised"*.

## Migrating from `ZREMOTE_TOKEN`

Pre-auth-overhaul ZRemote used a single shared `ZREMOTE_TOKEN` for the server
and every agent. The new scheme replaces it with:

- **Admin:** a rotate-able admin token (`zremote admin rotate-token`).
- **Agents:** per-agent ed25519 keys issued by enrollment.

### Server upgrade order

1. Upgrade the **server** first. It still accepts the legacy
   `Register { token }` handshake for one release so your existing agents do
   not drop offline during the rolling restart.
2. Upgrade each **agent**, one at a time, verifying reconnection in the GUI
   before moving on. Each new agent switches to the ed25519 handshake
   automatically — no action needed.
3. Run `zremote admin rotate-token` to generate the admin token and log into
   the GUI.

### After migration

- The legacy `Register { token }` handler logs a **deprecation warning** on
  every acceptance. Plan to disable legacy handshakes in the next release by
  setting the new-config flag (see release notes).
- After the next release removes legacy support, any agent still connecting
  with the old handshake will be refused and must re-enroll from the GUI.

### Local / standalone upgrade

No action required. On first start the new agent creates
`~/.zremote/local.token` and the GUI reads it automatically. The old
single-token scheme continues to work for one release if `ZREMOTE_TOKEN` is
set, but you can remove that env var as soon as you upgrade the agent.

## Troubleshooting

**"Login failed" with the admin token.** Either the token is wrong, or it
was rotated out. Run `zremote admin rotate-token` on the server host and try
again. A stuck rate limit also presents this way — failed logins are capped
at 5 per minute per IP; wait 60 s.

**"Enrollment failed" on the target host.** The code expired, was already
used, the agent's clock is skewed, or you lost TLS to the server. The error
message is deliberately opaque to prevent distinguishing these cases from an
attacker. Generate a fresh code from the GUI and retry — the install command
always produces a fresh single-use code.

**Agent will not reconnect after rotate-token.** Agents do not use the admin
token. If they disconnect after a rotate, something else is wrong — check
`zremote admin audit-tail --event agent_auth_failed_invalid_signature`
(or `_timeout`, `_version_mismatch`, `_unknown_agent`, `_invalid_public_key`,
`_malformed`, `_internal`) and the agent log. A common
cause is the server's clock drifting more than the 5-minute challenge window.
The relevant audit events are `agent_auth_ok` and `agent_auth_failed_*`.

**GUI shows "agent offline" but the process is running.** The agent is
running but its credential has been revoked. Check `zremote admin list-hosts`
to confirm, then re-enroll.

**`~/.zremote/local.token` missing or unreadable.** The GUI will show a
**Local bootstrap failed** screen with the expected path instead of exiting.
On the same user session, restart the agent — it will recreate the file with
mode `0o600`.
