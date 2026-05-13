# Codebase Quality Audit - 2026-05-13

Audit started: 2026-05-13T14:53:59+02:00

Scope requested:

- Review overall project quality and health.
- Identify refactoring that is needed.
- Measure how often `mut` is used.
- Evaluate Rust best practices.
- Write findings to this file progressively.

## Running Notes

- Repository root: `/home/zajca/Code/Me/myremote`.
- Current working tree already contains unrelated local changes in terminal-related files. This audit does not revert or normalize those changes.
- The project is a Rust workspace with multiple crates for CLI, GUI, protocol, client, core, agent, server, and the `zremote` binary.

## Initial Measurements

- Rust files under `crates`: 256.
- Rust lines under `crates`: 131,040 total; `src` only: 127,589.
- `mut` occurrences under `crates`: 2,222 total; `src` only: 2,209.
- `let mut` occurrences: 1,043.
- `&mut` occurrences: 923.
- `mut self` occurrences: 259.
- `static mut` occurrences: 0.
- `unsafe` token occurrences: 18.
- `.unwrap(` occurrences under `crates`: 4,148 total; `src` only: 3,916.
- `.expect(` occurrences under `crates`: 410 total; `src` only: 396.
- `todo!`/`unimplemented!`/`panic!` macro occurrences under `crates`: 126.
- Explicit `#[allow(...)]` or `#[expect(...)]` attributes under `crates`: 112.

Adjusted production-oriented counts, excluding `tests.rs` files and content after `#[cfg(test)]`:

- `mut`: 1,372.
- `let mut`: 561.
- `&mut`: 717.
- `mut self`: 244.
- `.unwrap(`: 15.
- `.expect(`: 38.

The raw `.unwrap(` count is therefore mostly test code. Production code is much better than the raw number suggests, though some explicit panics/expectations remain in startup and integration boundaries.

Top files by `mut` count:

| File | `mut` count |
| --- | ---: |
| `crates/zremote-agent/src/connection/dispatch.rs` | 172 |
| `crates/zremote-gui/src/views/terminal_panel.rs` | 137 |
| `crates/zremote-gui/src/views/sidebar.rs` | 126 |
| `crates/zremote-gui/src/views/settings/agent_profiles_tab.rs` | 122 |
| `crates/zremote-gui/src/views/main_view.rs` | 82 |
| `crates/zremote-agent/src/agentic/analyzer.rs` | 73 |
| `crates/zremote-gui/src/views/command_palette/mod.rs` | 67 |
| `crates/zremote-gui/src/persistence.rs` | 52 |
| `crates/zremote-agent/src/hooks/handler.rs` | 50 |
| `crates/zremote-gui/src/views/terminal_bench.rs` | 49 |

Top production-oriented files by `mut`, excluding test modules/files:

| File | `mut` count |
| --- | ---: |
| `crates/zremote-gui/src/views/sidebar.rs` | 126 |
| `crates/zremote-gui/src/views/settings/agent_profiles_tab.rs` | 108 |
| `crates/zremote-gui/src/views/main_view.rs` | 82 |
| `crates/zremote-gui/src/views/terminal_panel.rs` | 81 |
| `crates/zremote-gui/src/views/command_palette/mod.rs` | 65 |
| `crates/zremote-gui/src/views/worktree_create_modal.rs` | 40 |
| `crates/zremote-agent/src/connection/mod.rs` | 35 |
| `crates/zremote-agent/src/agentic/analyzer.rs` | 30 |

Interpretation so far: `mut` usage is not inherently bad in Rust, but concentration in large dispatch/view modules is a maintainability signal. The codebase has no `static mut`, which is good.

## Workspace And Tooling

- Workspace uses resolver `2`, edition `2024`, stable toolchain, and shared workspace dependencies.
- `rustfmt.toml` sets edition `2024`, `max_width = 100`, and field init shorthand.
- Workspace lints deny `unsafe_code`.
- Workspace clippy configuration denies `clippy::all` and warns on `clippy::pedantic`, with reasonable exceptions for documentation-heavy pedantic lints.
- All inspected crate manifests opt into `[lints] workspace = true`.

Positive Rust-practice signals:

- No `static mut` was found.
- `unsafe_code = "deny"` is configured at workspace level.
- The project uses typed crates for protocol/client/core/agent/server/gui separation rather than a single binary crate.
- The code uses `CancellationToken`, `tokio`, `tracing`, typed request/response structs, and shared protocol types instead of untyped ad-hoc JSON at most public boundaries.

Health concerns visible from structure:

- `zremote-agent` and `zremote-server` both carry route trees with many matching files. `diff -qr` shows corresponding route files differ across nearly every shared endpoint. This is the highest refactoring priority because behavior can drift between local and server modes.
- Several modules are very large: `server/src/routes/agents/dispatch.rs` is 3,995 lines, `agent/src/local/routes/projects/tests.rs` is 3,052 lines, `agent/src/connection/dispatch.rs` is 2,979 lines, `gui/src/views/settings/agent_profiles_tab.rs` is 2,976 lines, and `gui/src/views/command_palette/mod.rs` is 2,617 lines.
- `zremote-agent/src/lib.rs` and `zremote-server/src/lib.rs` suppress multiple pedantic lints at crate level, including `too_many_lines`, `items_after_statements`, and `dead_code`. This keeps the tree moving, but it hides useful design pressure.

## Verification Commands

Commands were run through `nix develop` because plain `cargo` is not available on PATH outside the project dev shell.

| Command | Result |
| --- | --- |
| `nix develop -c cargo fmt --all -- --check` | Passed |
| `nix develop -c cargo clippy --workspace --all-targets -- -D warnings` | Failed |
| `nix develop -c cargo clippy --workspace --all-targets --all-features --exclude zremote-gui --exclude zremote` | Passed with warnings |
| `nix develop -c cargo test --workspace --quiet` | Passed |
| `nix develop -c cargo audit` | Failed |

Clippy failure:

- `crates/zremote-client/src/terminal.rs:160`: `TerminalInput::Data(data)` and `TerminalInput::PaneData { data, .. }` have identical match bodies. With `-D warnings`, `clippy::match_same_arms` blocks the lint gate. This file is also currently modified in the working tree, so the issue may belong to in-progress local work rather than committed baseline.

Current CI clippy command, as defined in `.github/workflows/ci.yml`, does not use `-D warnings` and excludes `zremote-gui` and `zremote`. That CI-shaped command passes locally but still emits:

- `crates/zremote-client/src/terminal.rs:160`: `clippy::match_same_arms`.
- `crates/zremote-agent/src/connection/mod.rs:1033`: `clippy::unused_async` in a test helper.

Test summary from `cargo test --workspace --quiet`:

- Passed: 2,781 tests.
- Failed: 0.
- Ignored: 17.
- Doc tests: all zero-test doc-test crates passed.

Security/dependency audit summary:

- `cargo audit` scanned 883 crate dependencies and failed with 4 vulnerabilities.
- It also printed repeated timeout errors while checking yanked packages against the registry, so the yanked-package part is not fully verified.
- Reported vulnerabilities:
  - `RUSTSEC-2023-0071`: `rsa 0.9.10`, medium severity, via `sqlx-mysql`.
  - `RUSTSEC-2026-0098`, `RUSTSEC-2026-0099`, `RUSTSEC-2026-0104`: `rustls-webpki 0.103.10`; fixed by upgrading to at least `0.103.13` in the 0.103 line or the listed 0.104 alpha.
- Reported warnings include unmaintained `async-std`, `core2`, `instant`, `paste`, `proc-macro-error`, `rustls-pemfile`, plus `rand` unsound advisories for `0.8.5` and `0.9.2`.
- The `rsa` path is suspicious for this project because the workspace only uses SQLite directly. The root `sqlx` dependency should likely set `default-features = false` and explicitly enable only `runtime-tokio`, `sqlite`, and `migrate` to avoid pulling MySQL/Postgres transitive dependencies.

## Rust Best Practices Assessment

Overall: good foundation, but too much complexity is concentrated in a few files and mode-specific copies.

What is strong:

- Typed protocol crate and shared client/protocol types reduce API drift.
- Workspace lints are consistently inherited by crate manifests.
- Formatting is clean.
- Test coverage by count is strong, with a large number of unit and route tests passing locally.
- Async/blocking boundaries are often handled intentionally with `spawn_blocking`, timeouts, `CancellationToken`, and bounded channels.
- Unsafe usage is constrained. The observed real unsafe blocks are test-only environment-variable mutation in Rust 2024; no `static mut` exists.

What is weak:

- CI allows pedantic warnings to survive. The workspace says pedantic lints matter, but GitHub does not enforce warning-free clippy.
- Crate-level `#![allow(...)]` blocks in `zremote-agent`, `zremote-server`, and `zremote-gui` are broad. They hide `too_many_lines`, `dead_code`, `unused_imports`, and other design signals that should be burned down module by module.
- The local/server route duplication creates two implementations for the same concepts. Tests help, but this is still a structural risk.
- Some large async dispatch functions mix protocol routing, DB updates, filesystem/git work, hook execution, event broadcasting, and error mapping. This makes correctness review hard.
- GUI view modules carry state management, validation, rendering, and event dispatch in the same large files.
- Dependency health is not currently enforced in CI. `cargo-audit` exists in the Nix shell but no audit/deny workflow is configured.

## Refactoring Priorities

1. Split duplicated local/server route behavior.

   Start with projects, sessions, knowledge, and agent profiles. Move shared request validation, response mapping, SQL/query calls, timeout policy, and structured error conversion into `zremote-core` service modules. Keep only transport-specific glue in `zremote-server/src/routes` and `zremote-agent/src/local/routes`.

2. Decompose dispatch modules into feature handlers.

   `crates/zremote-agent/src/connection/dispatch.rs` and `crates/zremote-server/src/routes/agents/dispatch.rs` should become thin routers over smaller handlers: terminal/session, worktree/git, knowledge, channel bridge, agent task lifecycle, and telemetry/agentic loops. This will reduce `mut` pressure, clone pressure, and the need for `too_many_lines` allowances.

3. Split GUI view state from rendering.

   `sidebar.rs`, `main_view.rs`, `terminal_panel.rs`, `settings/agent_profiles_tab.rs`, and `command_palette/mod.rs` are the highest `mut` and `.clone()` hotspots. Extract state reducers/view models and validation helpers first; then render functions become smaller and easier to test.

4. Tighten dependency configuration.

   Add `default-features = false` where appropriate, starting with `sqlx`, and verify that unused MySQL/Postgres dependency paths disappear. Update or patch dependency paths causing `rustls-webpki` advisories. Add a CI job for `cargo audit` or `cargo deny` with explicit accepted exceptions for unavoidable transitive warnings.

5. Burn down crate-level allows.

   Replace broad crate-level allowances with narrow module/function allowances and short comments. First targets: `too_many_lines`, `dead_code`, `unused_imports`, and `items_after_statements`.

6. Decide whether pedantic warnings should fail CI.

   If yes, change CI to `cargo clippy ... -- -D warnings` and fix the current warnings. If no, keep pedantic as informational but do not treat a strict local clippy failure as a release blocker.

7. Keep test volume, improve test organization.

   Many tests live inline in large modules. Keep the coverage, but move bulky route tests into clearer `tests/` modules or helper fixtures to reduce production file noise and make production metrics easier to read.

## `mut` Assessment

`mut` is not excessive across the project as a whole. The adjusted production count is 1,372 across roughly 127k `src` lines, and most of it is expected in GUI state updates, terminal emulation, session management, and async request setup.

The problem is concentration, not raw frequency:

- GUI files mutate view state heavily.
- Dispatch files clone and mutate request-local data before `tokio::spawn` and `spawn_blocking`.
- Tests use `mut` and `unwrap` heavily, which is normal but inflates raw metrics.

Recommended rule: do not chase `mut` mechanically. Refactor when `mut` appears together with long functions, nested matches, multiple ownership handoffs, or repeated clone/mutate/send patterns.

## Health Verdict

The project is functional and actively maintained: formatting passes, CI-shaped clippy passes, and the full workspace test suite passes locally. The main health risks are architectural, not immediate correctness failures.

Current health rating: medium-good.

Reasons:

- Good: typed workspace structure, strong tests, formatting clean, no production unsafe pattern found, clear use of Rust async primitives.
- Medium risk: large files, broad lint allowances, route duplication between local/server modes, dependency audit failures, and clippy warnings that CI permits.
- Immediate action: fix dependency audit findings and remove mode-duplicated business logic before adding more features to these areas.

## Follow-Up Implementation - 2026-05-13

First high-impact refactor completed:

- Target selected from priority 1: duplicated local/server route behavior.
- Implemented the safest first slice: `agent_profiles`.
- Branch: `worktree-audit-route-refactor`.
- Commit: `52b9e75 refactor agent profile routes`.
- Draft PR: <https://github.com/zajca/zremote/pull/62>.

What changed:

- Added `zremote_core::services::agent_profiles`.
- Moved shared profile DTOs, common profile validation, list/get/create/update/delete/default CRUD flow, duplicate-name conflict mapping, and default promotion into `zremote-core`.
- Kept mode-specific behavior at the route edge:
  - server mode passes `validate_settings_for_kind` from core;
  - local mode passes launcher-registry validation from `zremote-agent`.
- Reduced both route files to HTTP extraction/state glue plus their settings-validator adapter.

Validation evidence:

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --all-features --exclude zremote-gui --exclude zremote -- -D warnings`: passed.
- `cargo test -p zremote-core services::agent_profiles --quiet`: passed.
- `cargo test -p zremote-server routes::agent_profiles --quiet`: passed.
- `cargo test -p zremote-agent local::routes::agent_profiles --quiet`: passed.
- `cargo test --workspace --quiet`: passed.
- Pre-commit hook also reran `cargo fmt`, `cargo clippy`, and `cargo test`; all passed.

Impact:

- This removes a lockstep-maintenance hotspot without changing the public API shape.
- It creates the first `zremote-core::services` pattern that future local/server route refactors can copy.
- It proves that route duplication can be reduced incrementally while preserving mode-specific behavior through injected validators/adapters.

Recommended next step:

Continue priority 1 with `projects` routes, but do it in a narrow slice rather than moving all project behavior at once. The next best target is shared read/update/delete behavior:

- `list_projects`
- `get_project`
- `list_project_sessions`
- `update_project`
- the DB deletion/error portion of `delete_project`

Avoid moving `add_project` first. Local mode performs filesystem existence checks, project detection, metadata updates, and worktree parent registration, while server mode sends `ProjectRegister` to the agent. That makes `add_project` a higher-risk behavior split. Extracting the read/update/delete slice first should reduce duplication while keeping the transport and filesystem differences obvious.

## Completion Audit

Requested deliverables:

- Project quality and health review: covered in Workspace And Tooling, Rust Best Practices Assessment, Health Verdict.
- Needed refactoring: covered in Refactoring Priorities.
- Frequency of `mut`: covered in Initial Measurements and `mut` Assessment.
- Rust best practices compliance: covered in Workspace And Tooling and Rust Best Practices Assessment.
- Write everything to a file progressively: this report was created first and updated after each evidence-gathering step.

Evidence used:

- File structure via `rg --files`, `find`, and `wc`.
- Workspace/tooling files: `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, crate manifests, CI workflows, `flake.nix`.
- Metrics via `rg` and a production-oriented count that excludes `tests.rs` and content after `#[cfg(test)]`.
- Code inspection of large dispatch, route, state, GUI, and protocol files.
- Verification commands run through `nix develop`.

Known limitations:

- `cargo audit` registry yanked checks timed out, so yanked dependency status is not fully verified.
- The working tree was dirty before the audit. Findings involving modified terminal files may include in-progress local changes.

## Follow-Up Implementation - Projects CRUD Slice - 2026-05-13

Second priority-1 refactor completed:

- Target selected from priority 1: duplicated local/server route behavior in `projects`.
- Implemented the next narrow low-risk slice:
  - `list_projects`
  - `get_project`
  - `list_project_sessions`
  - `update_project`
  - DB/error portion of `delete_project`
- Branch: `worktree-audit-projects-core-service`.
- Based on prior refactor branch: `worktree-audit-route-refactor` / PR #62.

What changed:

- Added `zremote_core::services::projects`.
- Moved shared project response aliases, `UpdateProjectRequest`, UUID boundary validation, list/get/session-list/update/delete DB flow, and delete-not-found mapping into `zremote-core`.
- Kept `add_project` intentionally out of this slice because local mode still owns filesystem checks, project detection, metadata update, and worktree parent registration, while server mode sends `ProjectRegister` to the agent.
- Kept mode-specific behavior at the route edge:
  - server mode still sends `ServerMessage::ProjectRemove` before DB deletion when a connected agent is available;
  - local mode deletes without remote notification;
  - both modes still broadcast `ProjectsUpdated` after successful `update_project`.
- Reduced `crates/zremote-server/src/routes/projects/crud.rs` and `crates/zremote-agent/src/local/routes/projects/crud.rs` to HTTP/state glue for the extracted endpoints.

Coverage added:

- Core service tests for host/project ID validation, list/get/session-list behavior, update pinned, empty update patch behavior, delete success, delete not found, session FK nulling on delete, and server delete-notification target lookup.
- Server route tests for `PATCH /api/projects/:project_id`, including pinned update, invalid ID, missing project, and `ProjectsUpdated` broadcast.
- Local route tests for the same `PATCH /api/projects/:project_id` behavior and broadcast.

Validation evidence:

- `nix develop -c cargo fmt --all -- --check`: passed.
- `nix develop -c cargo test -p zremote-core services::projects --quiet`: passed.
- `nix develop -c cargo test -p zremote-server routes::projects::tests --quiet`: passed.
- `nix develop -c cargo test -p zremote-agent local::routes::projects::tests --quiet`: passed.
- `nix develop -c cargo test -p zremote-server --lib tests::list_project_sessions_returns_linked_sessions --quiet`: passed.
- `nix develop -c cargo test -p zremote-server --lib tests::delete_project_sets_session_project_id_null --quiet`: passed.
- `nix develop -c cargo clippy --workspace --all-targets --all-features --exclude zremote-gui --exclude zremote -- -D warnings`: passed.
- `nix develop -c cargo test --workspace --quiet`: passed.

Impact:

- Removes another lockstep-maintenance hotspot between local and server modes while preserving public API behavior and status codes.
- Keeps the complex `add_project` mode split explicit for a later, separate refactor.
- Extends the `zremote-core::services` pattern established by `agent_profiles` to a second route family.
