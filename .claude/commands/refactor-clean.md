# Refactor Clean

Invoke the **refactor-cleaner** agent to safely identify and remove dead code from the ZRemote workspace.

## What This Command Does

1. **Detect**: Run cargo warnings, clippy unused checks, and grep-based analysis
2. **Categorize**: Sort findings into SAFE / CAUTION / DANGER tiers
3. **Remove safely**: One category at a time, verify after each batch
4. **Consolidate**: Merge duplicate logic, remove unnecessary indirection
5. **Report**: Summary of changes and verification status

## Detection Tools

| Tool | What It Finds |
|------|---------------|
| `cargo check --workspace` warnings | Dead code, unused variables, unused imports |
| `cargo clippy -W unused-imports` | Unused imports specifically |
| `cargo +nightly udeps` | Unused crate dependencies (if installed) |
| grep analysis | Unused `pub` items, duplicate function signatures |

## Safety Tiers

| Tier | Examples | Action |
|------|----------|--------|
| SAFE | Unused imports, private functions with no callers | Remove confidently |
| CAUTION | Unused `pub` items, components | Verify no cross-crate usage |
| DANGER | Protocol types, core query functions | Never remove without full workspace grep |

## Rules

- **Never remove without testing**: `cargo check --workspace` + `cargo test --workspace` after each batch
- **One category at a time**: Imports, then functions, then types, then deps
- **Never remove protocol types**: May be used in deserialization
- **Check both server AND local mode routes**: Query functions may be used in both
- **Skip if uncertain**: Dead code is annoying but harmless

## When to Use

- After completing a large feature (cleanup leftovers)
- During dedicated refactoring sprints
- When build warnings pile up
- NOT during active feature development
- NOT right before deployment

## Related

- `/verify` -- Run full verification after cleanup
- `/rust-review` -- Review the cleanup changes
