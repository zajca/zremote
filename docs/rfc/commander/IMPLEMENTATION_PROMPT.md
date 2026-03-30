# Commander Implementation Prompt

Use this prompt to start a Claude Code agent for implementing the Commander feature.

## Usage

```bash
claude --model opus -p "$(cat docs/rfc/commander/IMPLEMENTATION_PROMPT.md)"
```

Or copy the prompt section below into a new CC session.

---

## Prompt

You are implementing the ZRemote Commander feature. Read the RFC documents in `docs/rfc/commander/` for full design details. Follow the project's CLAUDE.md for coding conventions, testing requirements, and review workflow.

### What to implement

Commander v1 consists of 4 phases plus a prerequisite. Phases 1 and 2 are independent and can be done in parallel. Phase 3 depends on both. Phase 4 depends on Phase 3.

#### Prerequisite: ServerEvent Unknown variant

Add `#[serde(other)] Unknown` variant to the `ServerEvent` enum in `crates/zremote-protocol/src/events.rs`. This is required for protocol forward-compatibility before adding any new event types. Verify that existing event deserialization still works (all tests pass). This is a one-line change plus tests.

#### Phase 1: LLM Output Format (`--output llm`)

See `docs/rfc/commander/phase-1-llm-output.md` for full spec.

Files to create/modify:
- CREATE `crates/zremote-cli/src/format/llm.rs` -- new `LlmFormatter` implementing the `Formatter` trait
- MODIFY `crates/zremote-cli/src/format/mod.rs` -- add `Llm` to `OutputFormat` enum, add `mod llm;`, add match arm in `create_formatter`

Key requirements:
- JSON Lines format (one compact JSON object per line)
- Short keys: `_t` (type), `st` (status), `n` (name), `v` (version), etc.
- See the key mapping table in the RFC for the complete mapping
- Every object includes `_t` field with entity type
- Structured error output: `{"_t":"error","code":"...","msg":"..."}`
- Full IDs (never truncated)
- Flat structure (no nested objects)
- Implement all 17 methods of the `Formatter` trait
- Write unit tests for each formatter method

#### Phase 2: Knowledge Extract CLI

See `docs/rfc/commander/phase-2-knowledge-extract.md` for full spec.

Files to modify:
- MODIFY `crates/zremote-cli/src/commands/knowledge.rs` -- add `Extract` subcommand

Key requirements:
- Command: `zremote cli knowledge extract <project_id> --loop-id <loop_id>`
- Calls existing `client.extract_memories()` API endpoint
- Flags: `--loop-id`, `--session-id` (one required), `--save`
- Output uses existing `memories` formatter method
- Write tests using in-memory SQLite + real API handlers (no mocks)

#### Phase 3: Commander Generate

See `docs/rfc/commander/phase-3-commander-generate.md` for full spec.

Files to create/modify:
- CREATE `crates/zremote-cli/src/commands/commander.rs` -- `generate` subcommand
- CREATE `crates/zremote-cli/commander-reference.md` -- static CLI reference (checked in, included by generator)
- MODIFY `crates/zremote-cli/src/commands/mod.rs` -- add `pub mod commander;`
- MODIFY `crates/zremote-cli/src/lib.rs` -- add `Commander` variant to `Commands` enum + match arm

Key requirements:
- `zremote cli commander generate` outputs CLAUDE.md to stdout
- `--write` writes to project directory (verify CC file loading behavior first)
- `--no-dynamic` skips API calls
- Dynamic section caching with 5-minute TTL at `~/.zremote/commander-cache.json`
- Token budget: under 6000 tokens total
- Generated content: identity, CLI reference, context protocol, dynamic infrastructure, error handling, workflow recipes, limitations
- Workflow recipes include: task dispatch, memory sync, Linear task processing, error recovery
- Write tests with in-memory SQLite + real API handlers

IMPORTANT: Before implementing `--write`, verify how Claude Code loads project instructions from `.claude/` directory. Does CC load all `*.md` files, or only `CLAUDE.md`? This determines the output filename and method.

#### Phase 4: Commander Start

See `docs/rfc/commander/phase-4-commander-start.md` for full spec.

Files to modify:
- MODIFY `crates/zremote-cli/src/commands/commander.rs` -- add `start` and `status` subcommands

Key requirements:
- `zremote cli commander start` generates CLAUDE.md + launches CC
- Claude binary discovery: `--claude-path` flag, `CLAUDE_CODE_PATH` env, PATH, common locations
- Environment setup: `ZREMOTE_OUTPUT=llm`, `ZREMOTE_SERVER_URL`, `ZREMOTE_HOST_ID`
- `--no-regenerate` skips generation if file exists and is < 5 min old
- `commander status` reports current state
- Exit code propagation from spawned `claude` process
- Shell quoting for prompt argument (see `crates/zremote-agent/src/claude/mod.rs` for pattern)

### Implementation rules

1. Read the project's `CLAUDE.md` first -- it has detailed coding conventions, testing requirements, and review workflow.
2. Run `cargo check --workspace` frequently during development.
3. Write tests for all new code. Use in-memory SQLite for API handler tests, never mock.
4. After implementing each phase, run `cargo test --workspace` and `cargo clippy --workspace`.
5. After all implementation is done, spawn `rust-reviewer` and `code-reviewer` agents for review.
6. Fix ALL review findings before committing.
7. Commit inside `nix develop`: `nix develop --command bash -c 'git commit -m "message"'`
8. Do NOT modify code outside the scope of this RFC. Do NOT refactor surrounding code.
9. Phase 5 (TigerFS) is NOT in scope -- do not implement it.
