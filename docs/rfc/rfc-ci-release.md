# RFC: GitHub CI + Release Pipeline

## Context

ZRemote has no CI/CD infrastructure. The project needs:
1. Automated quality checks on every push to `main` and on PRs
2. Cross-platform release builds triggered by git tags (`v*`) with GitHub Releases

## Problem: OpenSSL dependency

Currently `reqwest` (used by agent) and `teloxide` (used by server via `teloxide-core -> reqwest`) pull in `native-tls` / `openssl-sys` through default features. This breaks cross-compilation (especially `cross` for Linux aarch64) and adds a system dependency.

**Fix**: Switch both to `rustls` backend:
- `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }`
- `teloxide = { version = "0.17", default-features = false, features = ["macros", "rustls"] }`

## Changes

### 1. `Cargo.toml` (workspace root)

```toml
# Change reqwest line to:
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }

# Change teloxide line to:
teloxide = { version = "0.17", default-features = false, features = ["macros", "rustls"] }

# Add at end:
[profile.release]
strip = true
lto = "thin"
```

### 2. `.github/workflows/ci.yml`

Triggers: push to `main`, PRs targeting `main`.

Job graph:
```
[build-web] --> [lint]       (cargo fmt --check + cargo clippy)
     |      --> [test-rust]  (cargo test --workspace)
     +--------> [test-web]   (bun run typecheck + bun run test)
```

`build-web` builds the frontend and uploads as artifact (needed by Rust compilation due to rust-embed).

### 3. `.github/workflows/release.yml`

Triggers: push tags matching `v*`.

Job graph:
```
[build-web] --> [build] (matrix: 4 targets, 2 binaries each)
                   |
               [release] (GitHub Release + SHA256 checksums)
```

Build matrix:
| Target | Runner | Method |
|---|---|---|
| `x86_64-unknown-linux-musl` | `ubuntu-latest` | `cross` |
| `aarch64-unknown-linux-musl` | `ubuntu-latest` | `cross` |
| `x86_64-apple-darwin` | `macos-13` | native `cargo` |
| `aarch64-apple-darwin` | `macos-14` | native `cargo` |

Each build produces two binaries: `zremote-server` and `zremote-agent`, packaged as `.tar.gz` archives named `zremote-{target}.tar.gz`.

The `release` job collects all archives, generates SHA256 checksums, and creates a GitHub Release with all assets.

## Files

| File | Action |
|---|---|
| `Cargo.toml` | MODIFY (rustls, release profile) |
| `Cargo.lock` | REGENERATE (cargo update after dep changes) |
| `.github/workflows/ci.yml` | CREATE |
| `.github/workflows/release.yml` | CREATE |
| `docs/rfc/rfc-ci-release.md` | CREATE (this file) |

## Verification

1. `cargo build --workspace` passes after rustls switch
2. `openssl-sys` no longer appears in `cargo tree`
3. CI workflow runs on push/PR
4. Release workflow creates GitHub Release with 4 archives + checksums on `v*` tag
