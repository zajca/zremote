# Development Guide

## Prerequisites

- [Nix](https://nixos.org/) with flakes (recommended) -- `nix develop` gives you everything
- Or install manually: Rust (stable), SQLite3

## First-Time Setup

```bash
git clone <repo-url> && cd myremote
nix develop                    # enter dev shell
./scripts/dev-setup.sh         # install deps, compile workspace
```

The setup script is idempotent -- safe to re-run anytime.

## Development

```bash
# Run the GPUI desktop client (connects to localhost:3000 by default)
cargo run -p zremote -- gui

# Run the agent in local mode
cargo run -p zremote -- agent local --port 3000
```

## Running Tests

```bash
# Rust tests
cargo test --workspace

# Lint
cargo clippy --workspace

# Full coverage check (slower)
./scripts/check-coverage.sh
```

## Simultaneous Dev + Production

You can develop ZRemote on a host that's already running a production agent. Local mode uses a separate port and its own SQLite database (`~/.zremote/local.db`), so there's no conflict.

```
Production agent  -->  connects to server via WebSocket (server mode)
Dev agent         -->  localhost:3000, own DB (local mode)
```

## Protocol Compatibility

When changing message types in `zremote-protocol`, follow these rules to avoid breaking running deployments:

| Change | Safe? | How |
|--------|-------|-----|
| Add optional field | Yes | Use `#[serde(default)]` |
| Add new message variant | Yes* | Old versions silently ignore unknown types |
| Add required field | **No** | Use `Option<T>` + `#[serde(default)]` instead |
| Rename or remove field | **No** | Add new field, deprecate old one |

\* Only safe if the receiving side uses `#[serde(other)]` or ignores unknown variants.

## Deployment Order

When deploying changes to production:

1. **Server first** -- agents auto-reconnect with exponential backoff, daemon sessions survive restart
2. **Agents rolling** -- update one at a time, verify reconnection before proceeding to next

## Project Structure

```
scripts/
  dev-setup.sh          # First-time setup
  dev-backend.sh        # Backend-only dev
  check-coverage.sh     # Coverage gate check

crates/
  zremote-gui/          # Native GPUI desktop client
  zremote-protocol/     # Shared message types (agent <-> server)
  zremote-core/         # Shared DB, queries, processing
  zremote-server/       # Multi-host server (Axum)
  zremote-agent/        # Agent library (local + server + MCP modes)
```

## Useful Commands

```bash
# Run agent with verbose logging
RUST_LOG=debug cargo run -p zremote -- agent local --port 3000
```
