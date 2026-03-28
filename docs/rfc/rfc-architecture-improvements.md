# RFC: Architecture Improvements

**Status:** Draft
**Date:** 2026-03-28
**Author:** Architectural audit by Claude

---

## 1. Problem Statement

An architectural audit of the ZRemote codebase identified several structural issues that increase maintenance burden, risk subtle bugs, and limit scalability:

1. **Core crate couples to Axum** -- `zremote-core` depends on `axum` solely for HTTP response traits, forcing all consumers (including GUI) to pull in a web framework
2. **26 `status: String` fields across 9 files** -- session/host/loop/knowledge statuses are string literals compared manually, with no compiler assistance
3. **Duplicated WebSocket handlers** -- terminal and events handlers are copy-pasted between server and local mode (890 lines total)
4. **Register timeout mismatch** -- server waits 5s, agent waits 10s, causing silent reconnection loops
5. **Large files mixing concerns** -- 5 files exceed 2,000 lines each
6. **Security gaps** -- no WS rate limiting, path validation inconsistencies, unsafe `--bind 0.0.0.0` in local mode
7. **Test coverage gaps** -- core query modules (0 tests), local mode routes (0 tests), no integration tests

---

## 2. Scope

**In scope:**
- Remove axum dependency from zremote-core (or feature-gate it)
- Replace string status fields with typed enums
- Deduplicate server/local WebSocket handlers via shared trait
- Fix register timeout mismatch
- Decompose large files into sub-modules
- Add WS rate limiting and path validation
- Add local mode `--bind` safety warning
- Improve test coverage for core queries and local routes

**Out of scope:**
- REST API authentication middleware (separate effort)
- Per-agent token management
- GUI end-to-end tests

---

## 3. Current Architecture Issues

### 3.1 Core Crate Axum Coupling

`zremote-core/Cargo.toml` line 17:
```toml
axum = { workspace = true, features = ["ws"] }
```

`zremote-core/src/error.rs` lines 1-4:
```rust
use axum::extract::FromRequest;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
```

`AppError` implements `IntoResponse` and `AppJson<T>` uses `FromRequest`. This means `zremote-gui` (which depends on `zremote-client` -> `zremote-core`) transitively pulls in axum + hyper + tower + h2 -- ~50 crates it never uses.

### 3.2 String Status Fields

26 occurrences of `status: String` across 9 files:
- `zremote-client/src/types.rs` (10 occurrences)
- `zremote-core/src/state.rs` (9 occurrences)
- `zremote-core/src/queries/*.rs` (5 occurrences across hosts, sessions, loops, knowledge, claude_sessions)
- `zremote-core/src/processing/agentic.rs` (1)
- `zremote-server/src/routes/agents.rs` (1)

Status values used as string literals (40+ comparisons found):
- Sessions: `"creating"`, `"active"`, `"closed"`, `"suspended"`
- Hosts: `"online"`, `"offline"`
- Agentic loops: `"active"`, `"waiting_for_input"`, `"error"`, `"paused"`, `"auto_approve"`
- Claude tasks: `"starting"`, `"active"`, `"completed"`, `"error"`
- Knowledge: `"ready"`, `"indexing"`, `"error"`

### 3.3 Duplicated WebSocket Handlers

Terminal handler duplication:
- `crates/zremote-server/src/routes/terminal.rs` (537 lines)
- `crates/zremote-agent/src/local/routes/terminal.rs` (353 lines)

Both define identical `BrowserInput` enum (lines 18-27) and near-identical `ws_handler` + `handle_socket` functions.

Events handler duplication:
- `crates/zremote-server/src/routes/events.rs` (57 lines)
- `crates/zremote-agent/src/local/routes/events.rs` (99 lines)

### 3.4 Register Timeout Mismatch

- Server: `REGISTER_TIMEOUT = 5s` (`crates/zremote-server/src/routes/agents.rs:20`)
- Agent: `register_timeout = 10s` (`crates/zremote-agent/src/connection.rs:109`)

### 3.5 Large Files

| File | Lines | Concern mix |
|------|-------|-------------|
| `gui/views/command_palette.rs` | 3,385 | UI rendering + fuzzy + action dispatch + keybindings |
| `agent/local/routes/projects.rs` | 3,083 | CRUD + git + worktree + scanning + settings |
| `server/routes/agents.rs` | 2,785 | WS lifecycle + message dispatch + heartbeat + sessions |
| `agent/connection.rs` | 2,081 | Shell resolve + WS + message handling + PTY |
| `server/routes/projects.rs` | 2,072 | CRUD + relay + timeout handling |

---

## 4. Proposed Changes

### Phase 1: Quick Wins (S effort each)

#### 4.1.1 Fix Register Timeout

**Files:** MODIFY `crates/zremote-server/src/routes/agents.rs`, `crates/zremote-agent/src/connection.rs`

Create shared constant in `zremote-protocol`:
```rust
// zremote-protocol/src/lib.rs
pub const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);
```

Both server and agent import and use this constant.

#### 4.1.2 Move `BrowserInput` to Shared Location

**Files:** MODIFY `crates/zremote-core/src/state.rs`, MODIFY both `terminal.rs` files

Move `BrowserInput` enum to `zremote-core::state` (where `BrowserMessage` already lives). Remove duplicated definitions.

#### 4.1.3 Extract Hardcoded Timeouts

**Files:** MODIFY `crates/zremote-server/src/routes/projects.rs`, `knowledge.rs`

```rust
const AGENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
```

Replace 10+ instances of `Duration::from_secs(10)`.

#### 4.1.4 Local Mode Bind Safety

**Files:** MODIFY `crates/zremote-agent/src/local/mod.rs`

When `--bind` is not `127.0.0.1` or `::1`, log a warning:
```
WARN: Binding to {addr} exposes all APIs without authentication. Use only on trusted networks.
```

### Phase 2: Core Architecture (M-L effort)

#### 4.2.1 Remove Axum from Core

**Approach:** Feature flag `axum` on core crate:
```toml
[features]
default = []
axum = ["dep:axum"]

[dependencies]
axum = { workspace = true, features = ["ws"], optional = true }
```

`zremote-server` and `zremote-agent` (with local feature) enable `zremote-core/axum`. GUI doesn't.

#### 4.2.2 Status Enums

Define `SessionStatus`, `HostStatus`, `KnowledgeStatus` in `zremote-protocol/src/status.rs`. Reuse existing `ClaudeTaskStatus` and `AgenticStatus`. Wire to DB fields with `sqlx::Type`.

**Protocol compatibility:** JSON output remains identical (`snake_case` strings). SQLite stores as text. No migration needed.

### Phase 3: Deduplication (L effort)

#### 4.3.1 Shared Terminal WebSocket Handler

Define `TerminalBackend` trait in `zremote-core`. Both `AppState` and `LocalAppState` implement it. Generic handler in core.

#### 4.3.2 Shared Events WebSocket Handler

Extract events broadcast loop into `zremote-core/src/events_ws.rs`.

### Phase 4: File Decomposition (M effort per file)

Split large files into sub-modules by concern (details in plan file).

### Phase 5: Security Hardening

- WebSocket rate limiting via `tower::limit::ConcurrencyLimitLayer`
- Path validation function in `zremote-core/src/validation.rs`

### Phase 6: Test Coverage (parallel)

- Core query module tests (in-memory SQLite)
- Local mode route tests
- Processing module tests

---

## 5. Risk Assessment

| Change | Risk | Mitigation |
|--------|------|------------|
| Remove axum from core | LOW | Feature flag preserves backward compat |
| Status enums | LOW | Serde output identical, compiler catches all |
| Shared WS handlers | MEDIUM | Trait methods clearly separate concerns |
| File decomposition | LOW | Pure structural, no logic change |
| Path validation | LOW | Additive only |

---

## 6. Verification

After each phase:
1. `cargo build --workspace`
2. `cargo test --workspace`
3. `cargo clippy --workspace`
4. Phase 2.2: serde roundtrip tests for status enums
5. Phase 3: manual test of both server and local mode terminal
6. Phase 5: test rate limiting and path validation

---

## 7. Implementation Order

```
Phase 1 (quick wins) ──────────────────► can start immediately
Phase 2.1 (axum from core) ───────────► after Phase 1
Phase 2.2 (status enums) ─────────────► after Phase 1 (independent of 2.1)
Phase 3 (dedup) ───────────────────────► after Phase 2.2
Phase 4 (decomposition) ──────────────► after Phase 3
Phase 5 (security) ───────────────────► after Phase 1 (independent)
Phase 6 (tests) ──────────────────────► parallel from Phase 2 onward
```
