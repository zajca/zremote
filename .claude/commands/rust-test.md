# Rust TDD

Enforce test-driven development for Rust code in the ZRemote workspace.

## TDD Cycle

```
RED     -> Write failing test first
GREEN   -> Implement minimal code to pass
REFACTOR -> Improve code, tests stay green
REPEAT  -> Next test case
```

## Workflow

1. **Define interface**: Scaffold function signatures with `todo!()`
2. **Write tests** (RED): Comprehensive test module
3. **Run tests**: `cargo test --workspace` -- verify tests fail for the right reason
4. **Implement** (GREEN): Write minimal code to pass
5. **Refactor**: Improve while keeping tests green
6. **Coverage**: `cargo llvm-cov --workspace` -- target 80%+

## Test Patterns

### Unit Tests
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_behavior() {
        let result = function_under_test(input);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_error_case() -> Result<(), Box<dyn std::error::Error>> {
        let result = fallible_function("bad input");
        assert!(result.is_err());
        Ok(())
    }
}
```

### Async Tests (tokio)
```rust
#[tokio::test]
async fn test_async_operation() {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    // ... test with real in-memory SQLite
}
```

### Parameterized Tests (rstest)
```rust
use rstest::rstest;

#[rstest]
#[case("valid_input", true)]
#[case("", false)]
#[case("invalid!", false)]
fn test_validation(#[case] input: &str, #[case] expected: bool) {
    assert_eq!(validate(input), expected);
}
```

### Property-Based Tests (proptest)
```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn encode_decode_roundtrip(input in ".*") {
        let encoded = encode(&input);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(input, decoded);
    }
}
```

## Coverage Targets

| Code Type | Target |
|-----------|--------|
| Core queries & processing | 90%+ |
| Protocol serialization | 100% roundtrip |
| Public API (routes) | 80%+ |
| GUI views | Skip (GPUI rendering) |

## Coverage Commands

```bash
cargo llvm-cov --workspace              # Text summary
cargo llvm-cov --workspace --html       # HTML report -> target/llvm-cov/html/
cargo llvm-cov --workspace --fail-under-lines 80
```

## Edge Cases to Test

1. Empty input (empty string, empty vec, None)
2. Boundary values (max session count, scrollback limit)
3. Error paths (DB connection failure, WebSocket disconnect)
4. Concurrent operations (multiple sessions, simultaneous writes)
5. Protocol roundtrip (serialize -> deserialize -> assert_eq)
6. Migration idempotency (run migration twice)

## ZRemote Testing Patterns

- **Database tests**: Use in-memory SQLite (`sqlite::memory:`) for isolation
- **Protocol tests**: Roundtrip serialize/deserialize for all message types
- **Query tests**: Test each query function with realistic data
- **No mocking DB**: Use real in-memory SQLite, not mocks

## Related

- `/rust-build` -- Fix build errors
- `/rust-review` -- Review code quality
- `/verify` -- Full verification pipeline
