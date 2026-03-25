# Rust Build Fix

Invoke the **rust-build-resolver** agent to incrementally fix Rust build errors with minimal changes.

## What This Command Does

1. **Diagnose**: Run `cargo check --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`
2. **Parse errors**: Identify error codes and affected files
3. **Fix incrementally**: One error at a time, verify after each fix
4. **Test**: Run `cargo test --workspace` to verify no regressions
5. **Report**: Summary of what was fixed and what remains

## When to Use

- `cargo build` or `cargo check` fails with errors
- `cargo clippy` reports warnings/errors
- Borrow checker or lifetime errors block compilation
- After pulling changes that break the build
- After merging branches with conflicts resolved

## Fix Priority

1. **Compilation errors** -- code must build
2. **Clippy errors** -- `deny` level violations
3. **Clippy warnings** -- `warn` level (pedantic)
4. **Formatting** -- `cargo fmt`

## Common Errors

| Error | Typical Fix |
|-------|-------------|
| `cannot borrow as mutable` | Restructure to end immutable borrow first |
| `does not live long enough` | Use owned type or restructure lifetime |
| `mismatched types` | Add `.into()`, `as`, or explicit conversion |
| `trait X not implemented` | Add `#[derive(Trait)]` or implement manually |
| `unresolved import` | Add to Cargo.toml or fix `use` path |
| GPUI linking failure | Must run inside `nix develop` |

## Stop Conditions

- Same error persists after 3 attempts
- Fix introduces more errors than it resolves
- Requires architectural changes (escalate to user)

## Related

- `/rust-review` -- Review code quality after build succeeds
- `/rust-test` -- TDD workflow
- `/verify` -- Full verification pipeline
