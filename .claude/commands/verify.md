# Verify

Run the full verification pipeline for the ZRemote workspace. Executes checks sequentially -- stops on first failure.

## Pipeline

Run these checks in order:

### 1. Build Check
```bash
cargo check --workspace
```

### 2. Clippy Lint
```bash
cargo clippy --workspace -- -D warnings
```

### 3. Format Check
```bash
cargo fmt --check
```

### 4. Test Suite
```bash
cargo test --workspace
```

### 5. Git Status
```bash
git status
git diff --stat
```

## Report Format

```
Verification Report
───────────────────
1. Build:    PASS/FAIL
2. Clippy:   PASS/FAIL (N warnings)
3. Format:   PASS/FAIL
4. Tests:    PASS/FAIL (N passed, M failed)
5. Git:      clean / N uncommitted changes

Overall: PASS / FAIL at step N
```

## Modes

- **Default**: Run all 5 checks
- **Quick**: Build + Clippy only (steps 1-2)
- **Pre-commit**: All checks (same as default -- matches pre-commit hook)

## Notes

- Must run inside `nix develop` for system lib dependencies
- Clippy uses workspace-level config: `all = deny`, `pedantic = warn`
- Tests use in-memory SQLite -- no external services needed
- Pre-commit hook runs fmt, clippy, and test automatically

## When to Use

- Before committing (or let pre-commit hook handle it)
- After large refactoring
- After resolving merge conflicts
- Before creating a PR
- After pulling changes from main

## Related

- `/rust-build` -- Fix build errors
- `/rust-review` -- Code review
- `/rust-test` -- TDD workflow
