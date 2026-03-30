# Phase 1: LLM Output Format (`--output llm`)

## Problem

Current CLI output formats (`table`, `json`, `plain`) are designed for human consumption or generic machine parsing. When an LLM (Commander CC) uses ZRemote CLI via Bash, these formats waste tokens:

- `table` -- decorative borders, alignment padding, truncated values
- `json` -- pretty-printed, all fields including irrelevant timestamps, nested objects
- `plain` -- key-value pairs with relative time strings, no structure for programmatic use

A Commander CC instance making dozens of CLI calls per orchestration session burns thousands of tokens on formatting overhead.

## Goal

Add a fourth output format `--output llm` that minimizes token consumption while remaining reliably parseable by an LLM.

## Design

### Format: JSON Lines (NDJL)

One compact JSON object per line. No pretty-printing. Lists emit one object per line. Single entities emit one object.

### Principles

1. **Short keys** -- `_t` (type), `st` (status), `v` (version), `n` (name), `id` stays `id`
2. **Type tag** -- every object includes `_t` so the LLM knows what it's reading without surrounding context
3. **Actionable fields only** -- omit `created_at`, `updated_at`, audit fields unless they are the primary data
4. **Flat structure** -- no nested objects. Inline related data (e.g., host name instead of host_id reference)
5. **Status as short string** -- `"online"`, `"active"`, `"closed"` (not enum debug format)
6. **IDs untruncated** -- LLM needs full IDs to reference entities in follow-up commands

### Examples

```
# zremote cli --output llm host list
{"_t":"host","id":"a1b2c3d4-...","n":"dev-box","st":"online","v":"0.9.0","hostname":"dev.internal"}
{"_t":"host","id":"e5f6g7h8-...","n":"staging","st":"offline","v":"0.8.5","hostname":"staging.internal"}

# zremote cli --output llm session list
{"_t":"session","id":"...","n":"main","st":"active","shell":"/bin/zsh","dir":"/home/user/project"}

# zremote cli --output llm task get <id>
{"_t":"task","id":"...","st":"active","model":"opus","project":"/home/user/repo","cost":1.23}

# zremote cli --output llm project list
{"_t":"project","id":"...","n":"myapp","path":"/home/user/myapp","type":"rust","branch":"main","dirty":false}

# zremote cli --output llm memory list <project_id>
{"_t":"memory","id":"...","key":"auth-pattern","cat":"pattern","content":"Use repository pattern for data access"}

# zremote cli --output llm status
{"_t":"status","mode":"server","v":"0.9.0","hosts":3,"online":2}

# zremote cli --output llm loop list
{"_t":"loop","id":"...","session":"...","st":"active","tool":"claude","task":"Fix auth bug"}
```

### Events

Events use compact single-line JSON, same as the existing `json` formatter's `event()` method. No additional transformation needed.

### Auto-Detection

The existing `ZREMOTE_OUTPUT` env var (already wired in `GlobalOpts`) works. Commander's CLAUDE.md instructs setting `export ZREMOTE_OUTPUT=llm`. No new auto-detection logic needed.

## Implementation Scope

### What to build

- New `LlmFormatter` struct implementing the `Formatter` trait (17 methods)
- Add `Llm` variant to `OutputFormat` enum
- Wire in `create_formatter` match

### What NOT to build

- No changes to API responses or protocol types
- No changes to other formatters
- No new data fetching logic -- LLM formatter receives the same data as other formatters

### Formatter trait methods to implement

Each method maps entity fields to short-key compact JSON:

| Method | Entity | Key fields |
|--------|--------|------------|
| `hosts` / `host` | Host | id, n, st, v, hostname |
| `sessions` / `session` | Session | id, n, st, shell, dir |
| `projects` / `project` | Project | id, n, path, type, branch, dirty |
| `loops` / `agentic_loop` | AgenticLoop | id, session, st, tool, task |
| `tasks` / `task` | ClaudeTask | id, st, model, project, cost |
| `memories` / `memory` | Memory | id, key, cat, content |
| `config_value` | ConfigValue | key, value |
| `settings` | ProjectSettings | compact JSON (pass-through) |
| `actions` | ProjectAction | name, command |
| `worktrees` | WorktreeInfo | path, branch, dirty |
| `knowledge_status` | KnowledgeBase | st, version, error |
| `search_results` | SearchResult | compact JSON (pass-through) |
| `status_info` | ModeInfo + hosts | mode, v, hosts, online |
| `event` | ServerEvent | compact single-line JSON |
| `directory_entries` | DirectoryEntry | name, type (file/dir) |

## Testing

- Unit tests for each formatter method: verify output is valid JSON, contains expected fields, uses short keys
- Test that list output produces one JSON per line (split by `\n`, each parses as valid JSON)
- Test that single entity output produces exactly one JSON line
- Integration test: compare LLM format token count vs JSON format for same data (should be 40-60% fewer tokens)
