# RFC: Knowledge Integration for Claude Code

## Context

ZRemote has a partially built knowledge system powered by OpenViking (semantic indexing, memory extraction, CLAUDE.md generation). The system gathers knowledge but never delivers it back to Claude Code. Generated instructions are displayed in a web UI for manual clipboard copy. There is no automatic integration with Claude Code's context system, no on-demand querying during sessions, and no bootstrapping for existing projects.

**Problem**: Knowledge gathered by zremote is invisible to Claude Code sessions.

**Goal**: Claude Code sessions on remote hosts should automatically benefit from accumulated project knowledge through three progressive layers:
1. **CLAUDE.md** -- baseline context loaded at every session start
2. **MCP Server** -- on-demand semantic search and memory queries during sessions
3. **Automatic lifecycle** -- knowledge extraction, regeneration, and freshness without manual intervention

**Decision**: CLAUDE.md uses **section mode** -- single file, user content above `<!-- ZRemote Knowledge -->` marker, auto-generated content below.

---

## Architecture

```
Claude Code session (on remote host)
  |
  +-- reads {project}/.claude/CLAUDE.md        [Layer 1: always loaded, ~50 lines]
  |     (user section + auto-generated section)
  |
  +-- MCP stdio --> zremote-agent mcp-serve   [Layer 2: on-demand]
  |                   |
  |                   +-- HTTP --> OpenViking (localhost, semantic search)
  |                   +-- reads --> local memory cache (~/.zremote/memories/{project}.json)
  |
  +-- PTY output --> agentic detection -----> server --> auto-extract --> memories DB
                                                    |
                                                    +--> auto-regen CLAUDE.md (threshold)
```

Key principle: **MCP server talks directly to OpenViking on localhost**, never through the central server WebSocket. This eliminates latency and the server as bottleneck for real-time queries.

For memories (stored in server's SQLite), the agent periodically syncs a local cache file that the MCP server reads.

---

## Phase 1: Write-to-Disk Pipeline

**Goal**: Agent can write generated knowledge directly to `.claude/CLAUDE.md` on the remote host.

### 1.1 Protocol Changes

**File**: `crates/zremote-protocol/src/knowledge.rs`

Add new enum variant to `KnowledgeServerMessage`:
```rust
WriteClaudeMd {
    project_path: String,
    content: String,
    mode: WriteMdMode,
}
```

Add new enum variant to `KnowledgeAgentMessage`:
```rust
ClaudeMdWritten {
    project_path: String,
    bytes_written: u64,
    error: Option<String>,
}
```

Add new enum:
```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteMdMode {
    Replace,
    Section,
}
```

### 1.2 Agent Write Logic

**File**: `crates/zremote-agent/src/knowledge/mod.rs`

Add `write_claude_md()` method to `KnowledgeManager`:

- `Section` mode (default):
  1. Read existing `{project_path}/.claude/CLAUDE.md` if it exists
  2. Find marker line `<!-- ZRemote Knowledge (auto-generated, do not edit below) -->`
  3. If marker found: keep everything above marker, replace everything below with new content
  4. If file exists but no marker: append `\n\n` + marker + content at end
  5. If no file: create `.claude/` directory, write marker + content
- `Replace` mode: overwrite entire file (for projects where user opts out of section mode)
- Send `ClaudeMdWritten` back to server with bytes_written or error

Handle `WriteClaudeMd` in `handle_message()` match arm.

### 1.3 Server Endpoint

**File**: `crates/zremote-server/src/routes/knowledge.rs`

Add `POST /api/projects/{project_id}/knowledge/write-claude-md`:
1. Fetch project info (host_id, path)
2. Call `GenerateInstructions` and wait for response (reuse existing 60s timeout pattern)
3. On success, send `WriteClaudeMd { project_path, content, mode: Section }` to agent
4. Wait for `ClaudeMdWritten` confirmation (10s timeout via oneshot channel)
5. Return `{ written: true, bytes: N }` or error

**File**: `crates/zremote-server/src/routes/agents.rs`

Handle `ClaudeMdWritten` in `handle_knowledge_message()` -- route to oneshot channel.

### 1.4 CLAUDE.md Content Template

The synthesized content from OpenViking will be wrapped in a compact template:

```markdown
<!-- ZRemote Knowledge (auto-generated, do not edit below) -->

## Architecture
{2-3 line summary from Architecture memories, confidence >= 0.7}

## Key Patterns
{bullet list from Pattern memories, max 8 items, confidence >= 0.7}

## Pitfalls
{bullet list from Pitfall memories, max 5 items, confidence >= 0.7}

## Conventions
{bullet list from Convention memories, max 5 items, confidence >= 0.7}

## Knowledge Tools
This project has a ZRemote knowledge base. Use MCP tools for detailed queries:
- `knowledge_search`: semantic code search
- `knowledge_memories`: query project learnings
```

This replaces the current raw OpenViking synthesis with a structured, size-controlled template. Implement as a Rust function `format_claude_md_section()` in the agent.

### 1.5 Web UI Changes

**File**: `web/src/components/knowledge/InstructionGenerator.tsx`

Add "Write to CLAUDE.md" button next to existing "Copy to clipboard":
- Calls `api.knowledge.writeClaudeMd(projectId)`
- Shows success/error toast
- Disabled if host is offline

**File**: `web/src/lib/api.ts`

Add `knowledge.writeClaudeMd(projectId): Promise<{ written: boolean, bytes: number }>`.

### 1.6 Tests

- Protocol: roundtrip tests for `WriteClaudeMd`, `ClaudeMdWritten`, `WriteMdMode`
- Agent: unit test `write_claude_md()` -- section mode with existing file, without marker, without file
- Server: integration test for the new endpoint (mock agent response)

---

## Phase 2: MCP Server

**Goal**: Claude Code can query knowledge on-demand during sessions via MCP tools.

### 2.1 Architecture Decision

**Agent subcommand** (`zremote-agent mcp-serve --project /path/to/project`):
- Shares `OvClient` code for OpenViking HTTP calls
- Shares `OvConf` for configuration (port, data_dir)
- Single binary to deploy on remote hosts
- Direct localhost HTTP to OpenViking (no server round-trip)
- Reads local memory cache for memories (synced separately)

### 2.2 New Dependencies

**File**: `Cargo.toml` (workspace)
```toml
rmcp = { version = "1.2", features = ["server", "macros", "transport-io"] }
schemars = "0.8"
clap = { version = "4", features = ["derive"] }
```

**File**: `crates/zremote-agent/Cargo.toml`
```toml
rmcp = { workspace = true, optional = true }
schemars = { workspace = true, optional = true }
clap.workspace = true

[features]
default = ["mcp"]
mcp = ["dep:rmcp", "dep:schemars"]
```

### 2.3 CLI Subcommand

**File**: `crates/zremote-agent/src/main.rs`

Replace direct main logic with `clap` subcommands:
```rust
#[derive(clap::Parser)]
enum Cli {
    /// Run as agent connecting to zremote server (default)
    Run,
    /// Run as MCP server for Claude Code
    McpServe {
        /// Project path to serve knowledge for
        #[arg(long)]
        project: PathBuf,
        /// OpenViking port (default: from config or 8741)
        #[arg(long, default_value = "8741")]
        ov_port: u16,
    },
}
```

When no subcommand given, default to `Run` (backwards compatible).

### 2.4 MCP Server Module

**New files**:
- `crates/zremote-agent/src/mcp/mod.rs` -- Server struct, initialization
- `crates/zremote-agent/src/mcp/tools.rs` -- Tool definitions
- `crates/zremote-agent/src/mcp/resources.rs` -- Resource definitions

**Server struct**:
```rust
#[derive(Clone)]
pub struct KnowledgeMcpServer {
    client: Arc<OvClient>,
    project_path: PathBuf,
    namespace: String,
    memory_cache: Arc<RwLock<Vec<CachedMemory>>>,
}
```

### 2.5 MCP Tools

| Tool | Description | Params | Implementation |
|------|-------------|--------|----------------|
| `knowledge_search` | Semantic code search across project files | `query: String`, `tier?: "l0"\|"l1"\|"l2"`, `max_results?: u32` (default 10) | `OvClient::search()` |
| `knowledge_memories` | Query extracted project memories/learnings | `category?: String`, `query?: String` | Read from local memory cache, filter by category, FTS on query |
| `knowledge_context` | Get high-level project understanding | none | Read `{project}/.claude/CLAUDE.md` auto-generated section, or synthesize from memories |

Each tool returns `ToolUseResultBlock::text()` with formatted results. Keep responses compact:
- `knowledge_search`: max 10 results, each with path, line range, snippet (truncated to 200 chars)
- `knowledge_memories`: max 20 memories, each with key, content, category, confidence
- `knowledge_context`: the auto-generated CLAUDE.md section content

### 2.6 MCP Resources

| Resource URI | Content |
|-------------|---------|
| `zremote://context` | Auto-generated section of CLAUDE.md |
| `zremote://memories/pattern` | Pattern memories |
| `zremote://memories/decision` | Decision memories |
| `zremote://memories/pitfall` | Pitfall memories |
| `zremote://memories/architecture` | Architecture memories |
| `zremote://memories/convention` | Convention memories |

### 2.7 Memory Cache Sync

The MCP server runs independently of the server WebSocket connection. For memories (stored in server DB), it needs a local cache.

**Approach**: File-based cache at `~/.zremote/memories/{project_name}.json`

- The agent's main process (running in WebSocket mode) writes this file whenever `MemoryExtracted` is received
- The MCP server process reads this file on startup and watches for changes (inotify)
- Format: `Vec<CachedMemory>` serialized as JSON
- `CachedMemory`: `{ key, content, category, confidence, updated_at }`

Add to `KnowledgeManager`:
```rust
async fn sync_memories_to_cache(&self, project_path: &str, memories: &[ExtractedMemory]) {
    // Write to ~/.zremote/memories/{project_name}.json
}
```

### 2.8 Registration / Setup

When knowledge is first enabled for a project, the agent writes:

**File**: `{project_path}/.mcp.json` (if not exists)
```json
{
  "mcpServers": {
    "zremote-knowledge": {
      "command": "zremote-agent",
      "args": ["mcp-serve", "--project", "{project_path}"],
      "env": {}
    }
  }
}
```

Or user manually runs: `claude mcp add zremote-knowledge -- zremote-agent mcp-serve --project /path`

Add `write_mcp_json()` to `KnowledgeManager`, called after first successful indexing.

### 2.9 Tests

- MCP tools: unit tests with mock OvClient (test tool argument parsing, response formatting)
- MCP server: integration test -- start server, send JSON-RPC `tools/list`, verify tools returned
- Memory cache: test read/write/watch cycle

---

## Phase 3: Automatic Knowledge Lifecycle

**Goal**: Knowledge accumulates and stays fresh without manual intervention.

### 3.1 Fix Auto-Extract on Loop End

**File**: `crates/zremote-server/src/routes/agents.rs` (lines 883-940)

Current issues:
1. `openviking.auto_extract` defaults to disabled -- **change default to enabled**
2. `project_path` in `agentic_loops` table is often empty because `LoopDetected` doesn't reliably send it

Fix for project_path detection:
- **File**: `crates/zremote-agent/src/agentic/manager.rs`
- When `LoopDetected` is emitted, resolve project_path from the session's working directory
- The session's working directory comes from PTY's cwd: read `/proc/{pid}/cwd` symlink
- Fall back to matching against known project paths from the project scanner

### 3.2 Auto-Regenerate CLAUDE.md After Extraction

**File**: `crates/zremote-server/src/routes/agents.rs` -- in `handle_knowledge_message()`, after `MemoryExtracted` is processed:

1. Count memories extracted
2. Increment `memories_since_regen` counter in `knowledge_bases` table
3. If `memories_since_regen >= threshold` (default 5, configurable via `openviking.regenerate_threshold`):
   - Send `GenerateInstructions` to agent
   - On response, send `WriteClaudeMd { mode: Section }` to agent
   - Reset counter

### 3.3 DB Migration

**File**: `crates/zremote-server/migrations/007_knowledge_lifecycle.sql`

```sql
ALTER TABLE knowledge_bases ADD COLUMN memories_since_regen INTEGER NOT NULL DEFAULT 0;
ALTER TABLE knowledge_bases ADD COLUMN last_regenerated_at TEXT;
ALTER TABLE knowledge_bases ADD COLUMN last_claude_md_hash TEXT;
```

### 3.4 Memory Cache Sync Trigger

After `MemoryExtracted` is handled in the server, also send a new message to the agent to update the local cache:

Add to `KnowledgeServerMessage`:
```rust
SyncMemories {
    project_path: String,
    memories: Vec<CachedMemory>,
}
```

Or simpler: have the agent's knowledge manager always write cache after receiving `MemoryExtracted` (it already has the data).

**Decision**: Agent writes cache in `extract_memory()` after sending `MemoryExtracted` to server. No new protocol message needed -- agent has the data locally. Add `sync_memories_to_cache()` call in `KnowledgeManager::extract_memory()`.

### 3.5 Auto-Extract Default

**File**: `crates/zremote-server/src/routes/agents.rs` (line 892-893)

Change:
```rust
let should_extract = auto_extract
    .is_some_and(|(v,)| v == "true" || v == "1");
```
To:
```rust
let should_extract = auto_extract
    .map_or(true, |(v,)| v != "false" && v != "0");
```

This makes auto-extract enabled by default, opt-out via `openviking.auto_extract = false`.

### 3.6 Tests

- Auto-extract default: test that missing config key triggers extraction
- Regeneration threshold: test counter increment and trigger
- Memory cache sync: test file written after extraction

---

## Phase 4: Bootstrapping Existing Projects

**Goal**: Projects discovered by the scanner with zero knowledge get meaningful initial context.

### 4.1 Bootstrap Protocol

**File**: `crates/zremote-protocol/src/knowledge.rs`

Add to `KnowledgeServerMessage`:
```rust
BootstrapProject {
    project_path: String,
    existing_claude_md: Option<String>,
}
```

Add to `KnowledgeAgentMessage`:
```rust
BootstrapComplete {
    project_path: String,
    files_indexed: u64,
    memories_seeded: u32,
    error: Option<String>,
}
```

### 4.2 Bootstrap Flow

**File**: `crates/zremote-server/src/routes/agents.rs`

In `ProjectDiscovered` / `ProjectList` handler, after inserting project into DB:
1. Check if OpenViking is running for this host (query `knowledge_bases` where status = 'ready')
2. Check if project has any memories already (query `knowledge_memories` count)
3. If OV running AND zero memories: send `BootstrapProject` to agent

**File**: `crates/zremote-agent/src/knowledge/mod.rs`

Add `bootstrap_project()` method:
1. Index project files via `OvClient::index_project()` (if not already indexed)
2. If `existing_claude_md` is provided, send it to OpenViking for memory extraction:
   - Create synthetic transcript: `[{role: "system", content: "Project instructions: {claude_md_content}"}]`
   - Call `OvClient::extract_memories()`
3. Read project metadata files for additional seed memories:
   - `README.md` first 500 lines -- extract Architecture memories
   - `Cargo.toml` / `package.json` -- extract dependency info as Architecture
4. Send `BootstrapComplete` back to server
5. Write initial CLAUDE.md section and memory cache

### 4.3 Server-Side Bootstrap Trigger

**File**: `crates/zremote-server/src/routes/agents.rs`

In existing `ProjectDiscovered` handling (around line 481-545), after DB insert:
```rust
// Check if we should bootstrap knowledge
if knowledge_enabled_for_host(&state.db, &host_id).await {
    let memory_count = count_project_memories(&state.db, &project_id).await;
    if memory_count == 0 {
        // Read existing CLAUDE.md if present
        // Send BootstrapProject to agent
    }
}
```

Also add "Bootstrap" button in web UI:

**File**: `web/src/components/knowledge/KnowledgeStatus.tsx`

Show "Bootstrap Knowledge" button when:
- OV service is running
- Project has zero memories
- No bootstrap is currently in progress

### 4.4 Web UI: Bootstrap Progress

**File**: `web/src/stores/knowledge-store.ts`

Add `bootstrapStatus: Record<string, "idle" | "running" | "complete" | "error">`.

### 4.5 Existing CLAUDE.md Detection

The agent already tracks `has_claude_config` in `ProjectInfo`. But we need the actual file content for seeding.

When `BootstrapProject` is received, agent reads `{project_path}/.claude/CLAUDE.md` directly from filesystem. The `existing_claude_md` field in the protocol message is for cases where the server already has the content (e.g., from a previous scan). If `None`, agent reads from disk.

### 4.6 Tests

- Protocol: roundtrip tests for `BootstrapProject`, `BootstrapComplete`
- Agent: test bootstrap flow with mock OvClient (verify indexing called, memories extracted)
- Server: test auto-bootstrap trigger on ProjectDiscovered when OV is ready
- Bootstrap with existing CLAUDE.md: verify seed memories are extracted and section is preserved

---

## Phase 5: Skills Generation & Hooks

**Goal**: Complement CLAUDE.md with on-demand skills loaded when relevant.

### 5.1 Skills Generation

**File**: `crates/zremote-agent/src/knowledge/mod.rs`

Add `generate_skills()` method, called after CLAUDE.md regeneration:

Generate `.claude/skills/` files from memory categories with sufficient memories (>= 3 memories in category):

| File | Content | Load Trigger |
|------|---------|-------------|
| `.claude/skills/zremote-architecture/SKILL.md` | Architecture + Decision memories | Structural changes, new modules |
| `.claude/skills/zremote-patterns/SKILL.md` | Pattern memories with context | Feature implementation |
| `.claude/skills/zremote-pitfalls/SKILL.md` | Pitfall memories | Touching relevant code areas |

Skill file format:
```markdown
---
name: zremote-architecture
description: Project architecture decisions and component relationships
---

{memory content, organized by key, one section per memory}
```

### 5.2 Protocol Addition

Add to `KnowledgeServerMessage`:
```rust
GenerateSkills {
    project_path: String,
}
```

Add to `KnowledgeAgentMessage`:
```rust
SkillsGenerated {
    project_path: String,
    skills_written: u32,
}
```

### 5.3 Write Skills Logic

**File**: `crates/zremote-agent/src/knowledge/mod.rs`

1. Group memories by category
2. For each category with >= 3 memories (confidence >= 0.6):
   - Create `.claude/skills/zremote-{category}/` directory
   - Write `SKILL.md` with frontmatter + formatted memories
3. Clean up skills for categories that no longer have enough memories

### 5.4 Hooks (Lower Priority)

Generate `.claude/settings.json` hook entry for post-session notification. This is optional and can be implemented later -- the auto-extract on LoopEnded already handles the main use case.

### 5.5 Tests

- Skills generation: test with various memory distributions
- Skill cleanup: test category falls below threshold

---

## Task List

### Phase 1: Write-to-Disk Pipeline
- [ ] **P1-T01**: Add `WriteMdMode`, `WriteClaudeMd`, `ClaudeMdWritten` to `knowledge.rs` protocol + roundtrip tests
- [ ] **P1-T02**: Implement `write_claude_md()` in `KnowledgeManager` with section mode logic
- [ ] **P1-T03**: Implement `format_claude_md_section()` -- structured template from memories
- [ ] **P1-T04**: Handle `WriteClaudeMd` in `KnowledgeManager::handle_message()`
- [ ] **P1-T05**: Add oneshot channel routing for `ClaudeMdWritten` in `handle_knowledge_message()` (server)
- [ ] **P1-T06**: Add `POST /api/projects/{id}/knowledge/write-claude-md` endpoint
- [ ] **P1-T07**: Add `api.knowledge.writeClaudeMd()` to web API client
- [ ] **P1-T08**: Add "Write to CLAUDE.md" button in `InstructionGenerator.tsx`
- [ ] **P1-T09**: Unit tests for section mode: existing file with marker, without marker, no file
- [ ] **P1-T10**: Integration test for write-claude-md endpoint

### Phase 2: MCP Server
- [ ] **P2-T01**: Add `rmcp`, `schemars`, `clap` to workspace dependencies
- [ ] **P2-T02**: Add `clap` subcommand structure to agent `main.rs` (Run/McpServe)
- [ ] **P2-T03**: Create `mcp/mod.rs` -- `KnowledgeMcpServer` struct, stdio transport setup
- [ ] **P2-T04**: Implement `knowledge_search` tool (proxy to `OvClient::search()`)
- [ ] **P2-T05**: Implement `knowledge_memories` tool (read from local memory cache)
- [ ] **P2-T06**: Implement `knowledge_context` tool (read CLAUDE.md section)
- [ ] **P2-T07**: Create `mcp/resources.rs` -- implement resource listing and reading
- [ ] **P2-T08**: Implement memory cache file I/O (`~/.zremote/memories/{project}.json`)
- [ ] **P2-T09**: Add `write_mcp_json()` to agent -- generate `.mcp.json` for project
- [ ] **P2-T10**: Test MCP server startup and tool listing via JSON-RPC
- [ ] **P2-T11**: Test `knowledge_search` tool with mock OvClient
- [ ] **P2-T12**: Test memory cache read/write cycle
- [ ] **P2-T13**: End-to-end test: register MCP with `claude mcp add`, verify tools available

### Phase 3: Automatic Lifecycle
- [ ] **P3-T01**: Change auto-extract default to enabled (invert condition in `agents.rs`)
- [ ] **P3-T02**: Fix `project_path` in `LoopDetected` -- resolve from session cwd in agent
- [ ] **P3-T03**: Create migration `007_knowledge_lifecycle.sql` (add columns to `knowledge_bases`)
- [ ] **P3-T04**: Implement `memories_since_regen` counter -- increment on `MemoryExtracted`
- [ ] **P3-T05**: Implement auto-regeneration trigger -- when counter >= threshold, fire GenerateInstructions + WriteClaudeMd
- [ ] **P3-T06**: Add memory cache sync in agent's `extract_memory()` (write to `~/.zremote/memories/`)
- [ ] **P3-T07**: Test auto-extract default behavior
- [ ] **P3-T08**: Test regeneration threshold trigger
- [ ] **P3-T09**: Test memory cache written after extraction

### Phase 4: Bootstrapping Existing Projects
- [ ] **P4-T01**: Add `BootstrapProject`, `BootstrapComplete` to protocol + roundtrip tests
- [ ] **P4-T02**: Implement `bootstrap_project()` in `KnowledgeManager`
- [ ] **P4-T03**: Add seed memory extraction from README.md / config files
- [ ] **P4-T04**: Add bootstrap trigger in `ProjectDiscovered` handler (server)
- [ ] **P4-T05**: Add `bootstrapStatus` to knowledge-store.ts
- [ ] **P4-T06**: Add "Bootstrap Knowledge" button in `KnowledgeStatus.tsx`
- [ ] **P4-T07**: Test bootstrap with existing CLAUDE.md (preserve user content)
- [ ] **P4-T08**: Test bootstrap without CLAUDE.md (create from scratch)
- [ ] **P4-T09**: Test auto-bootstrap on project discovery

### Phase 5: Skills Generation
- [ ] **P5-T01**: Add `GenerateSkills`, `SkillsGenerated` to protocol
- [ ] **P5-T02**: Implement `generate_skills()` in `KnowledgeManager`
- [ ] **P5-T03**: Add skill generation trigger after CLAUDE.md regeneration
- [ ] **P5-T04**: Test skill file writing and cleanup
- [ ] **P5-T05**: Test skill content formatting with frontmatter

---

## Critical Files

| File | Phase | Changes |
|------|-------|---------|
| `crates/zremote-protocol/src/knowledge.rs` | 1,4,5 | WriteMdMode, WriteClaudeMd, ClaudeMdWritten, BootstrapProject, BootstrapComplete, GenerateSkills, SkillsGenerated |
| `crates/zremote-agent/src/knowledge/mod.rs` | 1,2,3,4,5 | write_claude_md(), format_claude_md_section(), bootstrap_project(), generate_skills(), sync_memories_to_cache() |
| `crates/zremote-agent/src/main.rs` | 2 | clap subcommands (Run/McpServe) |
| `crates/zremote-agent/src/mcp/mod.rs` | 2 | **NEW** -- KnowledgeMcpServer, stdio setup |
| `crates/zremote-agent/src/mcp/tools.rs` | 2 | **NEW** -- knowledge_search, knowledge_memories, knowledge_context |
| `crates/zremote-agent/src/mcp/resources.rs` | 2 | **NEW** -- resource listing and reading |
| `crates/zremote-agent/src/agentic/manager.rs` | 3 | Fix project_path in LoopDetected |
| `crates/zremote-agent/Cargo.toml` | 2 | Add rmcp, schemars, clap deps |
| `crates/zremote-server/src/routes/agents.rs` | 1,3,4 | Handle ClaudeMdWritten, fix auto-extract default, auto-regen, bootstrap trigger |
| `crates/zremote-server/src/routes/knowledge.rs` | 1 | write-claude-md endpoint |
| `crates/zremote-server/migrations/007_knowledge_lifecycle.sql` | 3 | **NEW** -- lifecycle tracking columns |
| `web/src/components/knowledge/InstructionGenerator.tsx` | 1 | "Write to CLAUDE.md" button |
| `web/src/components/knowledge/KnowledgeStatus.tsx` | 4 | "Bootstrap" button |
| `web/src/lib/api.ts` | 1 | writeClaudeMd endpoint |
| `web/src/stores/knowledge-store.ts` | 4 | bootstrapStatus |
| `Cargo.toml` (workspace) | 2 | rmcp, schemars, clap workspace deps |

## Existing Code to Reuse

| Component | Location | Reuse |
|-----------|----------|-------|
| `OvClient` | `agent/src/knowledge/client.rs` | MCP tools proxy directly through this client |
| `OvClient::search()` | `client.rs:68` | `knowledge_search` MCP tool |
| `OvClient::extract_memories()` | `client.rs:120` | Bootstrap memory seeding |
| `OvClient::synthesize_knowledge()` | `client.rs:160` | CLAUDE.md content generation |
| `project_name_from_path()` | `knowledge/mod.rs:281` | Namespace resolution in MCP server |
| `OvConf` | `knowledge/config.rs` | MCP server reads same config for port/data_dir |
| `handle_knowledge_message()` | `routes/agents.rs:948` | Extend with new message types |
| `generate_instructions()` endpoint | `routes/knowledge.rs:329` | Reuse pattern for write-claude-md flow |
| `knowledge_requests` DashMap | `state.rs` | Oneshot channel routing for new messages |

---

## Verification Plan

### Phase 1
1. Start server + agent, enable OpenViking, index a test project
2. Extract memories from a loop (or manually via web UI)
3. Click "Write to CLAUDE.md" in web UI
4. Verify `{project}/.claude/CLAUDE.md` contains marker + generated section
5. Edit the file above the marker, click "Write to CLAUDE.md" again -- verify user content preserved
6. `cargo test --workspace` -- all tests pass
7. `cargo clippy --workspace` -- no warnings

### Phase 2
1. `zremote-agent mcp-serve --project /path/to/project` starts without error
2. `claude mcp add zremote-knowledge -- zremote-agent mcp-serve --project /path` succeeds
3. Start Claude Code session, verify MCP tools appear in `/mcp`
4. Ask Claude Code to use `knowledge_search` -- verify semantic results returned
5. Ask Claude Code to use `knowledge_memories` -- verify memories returned
6. `cargo test --workspace` passes

### Phase 3
1. Run a Claude Code session in a tracked project, complete it
2. Verify memories auto-extracted (check server logs + DB)
3. After threshold (5 memories), verify CLAUDE.md auto-regenerated
4. Verify `~/.zremote/memories/{project}.json` cache file updated
5. `cargo test --workspace` passes

### Phase 4
1. Discover a new project that has no knowledge
2. Verify auto-indexing starts
3. For project with existing CLAUDE.md: verify seed memories extracted, original content preserved above marker
4. For project without CLAUDE.md: verify file created with initial content
5. Click "Bootstrap" button in web UI for manual trigger
6. `cargo test --workspace` passes

### Phase 5
1. Verify `.claude/skills/zremote-*.md` files generated after regeneration
2. In Claude Code, verify skills appear when typing `/`
3. Verify skills contain correctly formatted memories
4. `cargo test --workspace` passes

---

## Open Questions / Future Considerations

1. **OpenViking availability**: If OpenViking binary is not installed on a host, the MCP server should fail gracefully. Consider a fallback mode that only serves cached memories without semantic search.
2. **Multi-project on same host**: OpenViking uses namespace-based isolation, but the MCP server instance is per-project. This is fine -- each Claude Code session runs in one project context.
3. **Memory deduplication**: OpenViking may extract duplicate memories across sessions. Current DB uses key-based upsert (update if same key, higher confidence). Consider adding explicit dedup logic if duplicates become noisy.
4. **File watcher for re-indexing**: Not included in initial phases. Can be added later using `notify` crate -- watch project dirs, debounce 60s, trigger incremental re-index.
