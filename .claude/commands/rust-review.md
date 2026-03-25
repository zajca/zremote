# Rust Code Review

Invoke the **rust-reviewer** agent for comprehensive Rust-specific code review of the ZRemote workspace.

## What This Command Does

1. **Pre-gate checks**: `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`, `cargo test --workspace` -- stop if any fail
2. **Identify changes**: `git diff HEAD~1 -- '*.rs'` (or `git diff main...HEAD` for PRs)
3. **Security audit**: Check for unsafe usage, injection vectors, hardcoded secrets
4. **Ownership review**: Unnecessary clones, lifetime issues, borrowing patterns
5. **GPUI patterns**: theme::* usage, cx.notify(), icon() helper, thread safety
6. **Protocol compat**: serde tags, default fields, backward compatibility
7. **Generate report**: Issues categorized by severity

## When to Use

- After writing or modifying Rust code
- Before committing changes
- Reviewing pull requests
- After adding new endpoints or protocol messages

## Severity Levels

| Level | Examples | Action |
|-------|----------|--------|
| CRITICAL | SQL injection, command injection, auth bypass, unchecked unwrap in prod | Block merge |
| HIGH | Unnecessary clone, blocking in async, protocol breaking change, missing cx.notify() | Block merge |
| MEDIUM | Missing with_capacity, allocation in hot path, missing docs | Merge with caution |

## Approval Criteria

- **Approve**: No CRITICAL or HIGH issues
- **Warning**: Only MEDIUM issues
- **Block**: CRITICAL or HIGH issues found

## Related

- `/rust-build` -- Fix build errors first
- `/rust-test` -- TDD workflow
- `/verify` -- Full verification pipeline
