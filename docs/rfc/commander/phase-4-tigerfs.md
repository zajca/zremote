# Phase 4: TigerFS Integration

## Problem

In a multi-host ZRemote setup, Claude Code instances on different hosts work on the same projects but don't share context. Memories, learnings, and project knowledge are local to each host's `~/.claude/` directory. The Commander can sync memories via CLI commands (Phase 2 fallback), but this is explicit, per-task, and eventual -- not real-time.

## Goal

Optional per-project shared filesystem via TigerFS. When enabled for a project, all hosts working on that project see the same memories, context, and artifacts in real-time with ACID guarantees. No custom sync protocol -- the filesystem IS the sync layer.

## What is TigerFS

TigerFS (github.com/timescale/tigerfs) mounts a PostgreSQL database as a local directory. File writes become INSERTs, reads become SELECTs. Multiple hosts mounting the same database see changes immediately. ACID transactions prevent conflicts. Optional versioning tracks history.

- Go binary, MIT license, v0.5.0
- Linux: FUSE (`fuse3`), macOS: NFS
- Backend: PostgreSQL only

## Design

### Per-Project Opt-In

TigerFS is NOT a global ZRemote feature. It's enabled per project via project settings:

```json
{
  "tigerfs": {
    "enabled": true,
    "database_url": "postgres://user:pass@shared-db:5432/zremote_shared"
  }
}
```

Projects without TigerFS work exactly as before. No PostgreSQL dependency for the default case. ZRemote core stays on SQLite.

### Configuration via CLI

```
zremote cli settings set <project_id> tigerfs.enabled true
zremote cli settings set <project_id> tigerfs.database_url "postgres://..."
```

### Mount Path Convention

Each project gets a deterministic mount path on the agent's host:

```
~/.zremote/tigerfs/<project_name>/
```

The agent derives this from the project name. The mount path is not configurable per-project to keep things simple -- the agent decides where to mount based on a consistent naming scheme.

### Filesystem Layout

Once mounted, the shared directory contains:

```
~/.zremote/tigerfs/myapp/
  memories/
    <key>.md                    # One file per memory. Content is the memory text.
                                # Filename is the memory key (slugified).
  context/
    commander.md                # Commander instructions (if generated)
    project-knowledge.md        # Extracted knowledge summaries
  artifacts/
    <artifact-name>.md          # Task outputs, review results, etc.
```

This is a convention, not enforced schema. CC and Commander can create any directory structure they need. TigerFS maps directories to PostgreSQL tables and files to rows.

### Agent Lifecycle

**On agent startup / project discovery:**
1. Scan project settings
2. For each project with `tigerfs.enabled == true`:
   a. Check if `tigerfs` binary is in PATH
   b. If not found: log warning, skip (project works without TigerFS, CLI fallback)
   c. Create mount directory if needed
   d. Run `tigerfs mount <database_url> <mount_path>` as a child process
   e. Verify mount is healthy (check if mount path is accessible)
   f. Track the child process for cleanup

**On agent shutdown:**
1. For each active TigerFS mount:
   a. Run `tigerfs unmount <mount_path>`
   b. Wait for clean unmount (timeout 5s)
   c. If unmount fails: SIGTERM the tigerfs process

**On project settings change (TigerFS toggled):**
1. If enabled: mount (same as startup flow)
2. If disabled: unmount

### Health Monitoring

Agent periodically checks TigerFS mount health:
- Is the mount directory accessible?
- Is the tigerfs process still running?
- If unhealthy: attempt remount, log error, emit event

Health status is reported via the existing server event system so the GUI and Commander can see it.

## Memory Dual Path

The Commander CLAUDE.md (Phase 2) adapts instructions based on TigerFS availability:

### With TigerFS

CC reads and writes shared context as regular files:

```bash
# Read all shared memories
cat ~/.zremote/tigerfs/myapp/memories/*.md

# Write a new memory
cat > ~/.zremote/tigerfs/myapp/memories/api-pattern.md << 'EOF'
Use repository pattern for data access layer.
All DB queries go through repository structs, not called directly from handlers.
EOF

# Read project context
cat ~/.zremote/tigerfs/myapp/context/project-knowledge.md
```

Changes are immediately visible on all other hosts mounting the same database. No API calls, no sync delays.

### Without TigerFS (CLI fallback)

CC uses ZRemote CLI commands:

```bash
zremote cli memory list <project_id> --output llm
zremote cli memory update <project_id> <memory_id> --content "..."
```

This works but is explicit (must be called), per-operation (no automatic sync), and higher latency.

### How Commander Knows Which Path to Use

The Commander CLAUDE.md generator (Phase 2) queries project settings at generation time. For each project, it emits either filesystem instructions or CLI instructions. The Commander doesn't need to detect TigerFS at runtime -- it follows the generated instructions.

## Cross-Project Knowledge Sharing

Multiple projects can share the same PostgreSQL database:

```
Project A: tigerfs.database_url = "postgres://host/shared_knowledge"
Project B: tigerfs.database_url = "postgres://host/shared_knowledge"
```

TigerFS creates separate tables for each mount's directory structure, but since they're in the same database, cross-project queries are possible through TigerFS's query paths or direct SQL.

A more common pattern: a dedicated "shared knowledge" pseudo-project that multiple real projects reference. The Commander reads from the shared knowledge mount when dispatching tasks to any project.

## Protocol Changes

### Project Settings Extension

Add optional `TigerFsSettings` to `ProjectSettings` in `zremote-protocol`:

```rust
#[derive(Default, Serialize, Deserialize)]
pub struct TigerFsSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub database_url: Option<String>,
}
```

Use `#[serde(default)]` so existing settings without TigerFS config deserialize correctly (backward compatible).

### Server Events

New event variant for TigerFS status changes:

```rust
TigerFsStatus {
    project_id: String,
    mounted: bool,
    error: Option<String>,
}
```

This lets the GUI show TigerFS mount status per project.

## Security Considerations

- **database_url contains credentials**: Stored in project settings (in SQLite). Same trust model as existing config values. Never log the full URL.
- **TigerFS mount permissions**: Mount directory should be readable/writable only by the agent user. Use `0700` permissions.
- **FUSE privileges**: Linux FUSE requires `fuse3` package. Docker requires `--device /dev/fuse --cap-add SYS_ADMIN`. This is a deployment requirement, not something the agent can fix at runtime.

## Prerequisites

- TigerFS binary installed on the host (`tigerfs` in PATH)
- PostgreSQL instance accessible from the host
- Linux: `fuse3` package installed
- macOS: no additional dependencies (TigerFS uses NFS)

These are documented requirements. The agent gracefully degrades when TigerFS is not available (logs warning, uses CLI fallback).

## Testing

- Unit tests for TigerFS lifecycle management (mock process spawning)
- Test project settings serialization with and without TigerFS config
- Test that missing `tigerfs` binary results in graceful degradation, not crash
- Test mount path derivation from project name
- Test cleanup on agent shutdown
- Integration test (requires TigerFS + PostgreSQL): mount, write file, verify on second mount
