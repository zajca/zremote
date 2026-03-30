# Phase 5: TigerFS Integration (Separate RFC -- v2)

**Status: Deferred.** This phase is the most complex, has the most external dependencies, and the most failure modes. Commander v1 (Phases 1-4) is fully functional without TigerFS using CLI-based memory sync. This phase should be revisited after Commander v1 is in use and CLI-based memory sync proves insufficient.

## Problem

In a multi-host ZRemote setup, Claude Code instances on different hosts work on the same projects but don't share context. Memories, learnings, and project knowledge are local to each host's `~/.claude/` directory. The Commander can sync memories via CLI commands, but this is explicit, per-task, and eventual -- not real-time.

## Goal

Optional per-project shared filesystem via TigerFS. When enabled for a project, all hosts working on that project see the same memories, context, and artifacts in real-time with ACID guarantees. No custom sync protocol -- the filesystem IS the sync layer.

## What is TigerFS

TigerFS (github.com/timescale/tigerfs) mounts a PostgreSQL database as a local directory. File writes become INSERTs, reads become SELECTs. Multiple hosts mounting the same database see changes immediately. ACID transactions prevent conflicts. Optional versioning tracks history.

- Go binary, MIT license, v0.5.0 (pre-1.0, API may change)
- Linux: FUSE (`fuse3`), macOS: NFS
- Backend: PostgreSQL only

## Prerequisites

### Protocol prerequisite

`ServerEvent` enum in `zremote-protocol/src/events.rs` must have `#[serde(other)] Unknown` variant before adding `TigerFsStatus` event. Without this, older clients fail to deserialize the new event. This should be done as a standalone change before TigerFS work.

### System prerequisites

- TigerFS binary installed on the host (`tigerfs` in PATH)
- Agent should detect minimum TigerFS version and warn if too old
- PostgreSQL instance accessible from the host
- Linux: `fuse3` package installed, `/dev/fuse` device available
- macOS: no additional dependencies (TigerFS uses NFS)
- Docker: requires `--device /dev/fuse --cap-add SYS_ADMIN` (significant security surface)

The agent must check for `/dev/fuse` existence and `fuse3` availability before attempting to mount, not just whether the binary is in PATH.

## Design

### Per-Project Opt-In

TigerFS is NOT a global ZRemote feature. It's enabled per project via project settings:

```json
{
  "tigerfs": {
    "enabled": true,
    "database_url_env": "TIGERFS_DB_URL"
  }
}
```

### Credential Handling

The `database_url` contains PostgreSQL credentials. Instead of storing the literal URL in settings (which would appear in API responses, CLI output, and potentially logs), use an environment variable reference:

```json
{
  "tigerfs": {
    "enabled": true,
    "database_url_env": "TIGERFS_DB_URL"
  }
}
```

The agent reads the actual URL from the named environment variable at mount time. This follows the same pattern as other secret-bearing configs and ensures credentials never appear in:
- API responses from `settings get`
- CLI `settings` output
- Debug/trace logs
- WebSocket event payloads

### Configuration via CLI

```
zremote cli settings set <project_id> tigerfs.enabled true
zremote cli settings set <project_id> tigerfs.database_url_env "TIGERFS_DB_URL"
```

### Mount Path Convention

Each project gets a deterministic mount path on the agent's host:

```
~/.zremote/tigerfs/<project_name>/
```

Mount directory permissions: `0700` (readable/writable only by agent user).

### Filesystem Layout

Once mounted, the shared directory contains:

```
~/.zremote/tigerfs/myapp/
  memories/
    <key>.md                    # One file per memory
  context/
    commander.md                # Commander instructions
    project-knowledge.md        # Extracted knowledge summaries
  artifacts/
    <artifact-name>.md          # Task outputs, review results
```

This is a convention, not enforced schema. TigerFS maps directories to PostgreSQL tables and files to rows.

### Agent Lifecycle

**On agent startup / project discovery:**
1. Scan project settings
2. For each project with `tigerfs.enabled == true`:
   a. Check if `tigerfs` binary is in PATH and meets minimum version
   b. Check if `/dev/fuse` exists (Linux) or NFS is available (macOS)
   c. If prerequisites missing: log warning, skip (CLI fallback)
   d. Read database URL from the configured environment variable
   e. If env var not set: log error, skip
   f. Create mount directory with `0700` permissions if needed
   g. Run `tigerfs mount <database_url> <mount_path>` as a child process
   h. Verify mount is healthy (check if mount path is accessible)
   i. Track the child process for cleanup

**On agent shutdown:**
1. For each active TigerFS mount:
   a. Run `tigerfs unmount <mount_path>`
   b. Wait for clean unmount (timeout 5s)
   c. If unmount fails: SIGTERM the tigerfs process, then SIGKILL after 3s
   d. Log stale mount warning if unmount ultimately fails (user may need manual cleanup)

**On agent crash (stale mounts):**
- On next startup, detect stale FUSE mounts at expected paths
- Attempt `tigerfs unmount` or `fusermount -u` before re-mounting
- Log warning about stale mount cleanup

**On project settings change (TigerFS toggled):**
1. If enabled: mount (same as startup flow)
2. If disabled: unmount

**On PostgreSQL network partition:**
- TigerFS process may hang or error on file operations
- Agent health check detects unresponsive mount (timeout on stat operation)
- Log error, emit TigerFS health event
- Do NOT auto-remount during partition (may cause data issues)

### Health Monitoring

Agent periodically checks TigerFS mount health:
- Is the mount directory accessible? (stat with timeout)
- Is the tigerfs process still running?
- If unhealthy: log error, emit event, do NOT auto-remount (operator should investigate)

Health status is reported via `ServerEvent::TigerFsStatus`.

## Memory Dual Path

The Commander CLAUDE.md (Phase 3) adapts instructions based on TigerFS availability:

### With TigerFS

CC reads and writes shared context as regular files. Changes are immediately visible on all hosts.

Note: file writes through TigerFS are atomic at the row level (last-write-wins). For memory files written by different CC instances, this is fine (each memory is a distinct file). For shared config files edited concurrently, conflicts are possible.

### Without TigerFS (CLI fallback)

CC uses ZRemote CLI commands. This works but is explicit and higher latency.

### How Commander Knows Which Path to Use

The Commander CLAUDE.md generator queries project settings at generation time. For each project, it emits either filesystem instructions or CLI instructions.

## Cross-Project Knowledge Sharing

Multiple projects can share the same PostgreSQL database (same `database_url_env` pointing to the same URL). TigerFS creates separate tables per mount directory, but shared DB enables cross-project queries.

## Protocol Changes

### Project Settings Extension

Add optional `TigerFsSettings` to `ProjectSettings` in `zremote-protocol`:

```rust
#[derive(Default, Serialize, Deserialize)]
pub struct TigerFsSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Name of the environment variable containing the PostgreSQL connection URL.
    /// The actual URL is never stored in settings to avoid credential exposure.
    #[serde(default)]
    pub database_url_env: Option<String>,
}
```

Use `#[serde(default)]` so existing settings without TigerFS config deserialize correctly.

### Server Events

Requires `#[serde(other)] Unknown` variant on `ServerEvent` first (see Prerequisites).

New event variant:

```rust
#[serde(rename = "tigerfs_status")]
TigerFsStatus {
    project_id: String,
    mounted: bool,
    error: Option<String>,
}
```

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| TigerFS v0.5.0 is pre-1.0, API may change | Medium | Pin minimum version, detect and warn |
| Go binary dependency, no Rust bindings | Low | Sidecar process, graceful degradation |
| FUSE unavailable (cloud VMs, containers, WSL2) | Medium | Check `/dev/fuse` before mount, clear error |
| PostgreSQL operational burden | Medium | Document as optional, only for multi-host users |
| Stale mounts after agent crash | Medium | Detect and clean stale mounts on startup |
| Network partition makes mount unresponsive | Medium | Health check with timeout, don't auto-remount |
| Two agents mount same path simultaneously | Low | PID file or flock before mounting |

## Alternative Considered: `memory push/pull` Commands

Instead of TigerFS, add `memory push` and `memory pull` commands that sync memories between local `~/.claude/` and ZRemote server's SQLite. Simpler, no external dependencies, but eventual consistency. This alternative should be considered if TigerFS proves impractical.

## Testing

- Unit tests for TigerFS lifecycle management
- Test project settings serialization with and without TigerFS config
- Test that missing `tigerfs` binary results in graceful degradation
- Test that missing `/dev/fuse` results in clear error message
- Test stale mount detection and cleanup
- Test mount path derivation from project name
- Test credential handling (env var reference, never raw URL in settings)
- Test cleanup on agent shutdown (including SIGTERM/SIGKILL escalation)
- Integration test (requires TigerFS + PostgreSQL): mount, write file, verify on second mount
