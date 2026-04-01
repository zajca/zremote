# RFC: Shell Integration — PTY Shell Awareness and Environment Control

**Status:** Draft
**Date:** 2026-03-31
**Author:** zajca
**Parent:** [RFC v0.10.0 Agent Intelligence](README.md)
**Depends on:** -

---

## 1. Problem Statement

ZRemote spawns PTY sessions via `portable_pty` (`crates/zremote-agent/src/pty.rs`) with minimal shell awareness. The current spawn path sets `TERM=xterm-256color`, `COLORTERM=truecolor`, optionally sets a working directory and user-supplied env vars, then hands off to `CommandBuilder::new(shell)`. This creates several problems:

### 1.1 Autosuggestion interference

Shell plugins like `zsh-autosuggestions`, `zsh-autocomplete`, and `ble.sh` (bash) produce ghost text in the terminal. When an AI agent (Claude Code, Aider, Codex) runs inside a ZRemote PTY:

- Ghost text from autosuggestions mixes with actual output, confusing the Output Analyzer (Phase 1) pattern matching
- Autosuggestion accept keybindings can interfere with agent-typed commands
- ANSI escape sequences for dimmed ghost text add noise to the PTY output stream

### 1.2 No shell type awareness

The agent receives a shell path string (e.g., `/bin/zsh`, `/usr/bin/bash`) and passes it through unchanged. There is no structured `ShellType` enum, which means:

- No per-shell initialization customization
- No way to apply shell-specific workarounds (e.g., zsh SIGWINCH race, bash rcfile injection)
- Shell name extraction is done post-hoc via `Path::file_name()` in `session.rs:58` for display only

### 1.3 No session identity in the shell environment

Running processes inside a ZRemote PTY have no way to detect they are inside a managed session. Tools, scripts, and agents cannot:

- Check `ZREMOTE_TERMINAL=1` to adapt behavior
- Look up `ZREMOTE_SESSION_ID` to correlate with the ZRemote API
- This prevents future integrations where in-shell tools could communicate with the ZRemote agent

### 1.4 Resize race on zsh

When GPUI opens a terminal and immediately resizes it, zsh may not process the `SIGWINCH` in time, leaving the terminal with incorrect dimensions until the next manual resize. The Output Analyzer depends on correct terminal dimensions for prompt detection accuracy.

---

## 2. Goals

- **Detect shell type** from the spawn command or `$SHELL`, producing a structured `ShellType` enum
- **Disable autosuggestion plugins** per shell (zsh-autosuggestions, zsh-autocomplete, ble.sh, fish native) to produce clean PTY output for the Output Analyzer
- **Inject environment variables** (`ZREMOTE_TERMINAL=1`, `ZREMOTE_SESSION_ID=<uuid>`) for session identity
- **Force SIGWINCH** on zsh startup to fix the resize race with GPUI
- **Preserve user shell configuration** -- source user configs first, apply overrides additively
- **Make integration opt-in** per session, with sensible defaults (enabled for AI sessions, disabled for manual terminals)
- **Clean up temp files** (custom ZDOTDIR, rcfiles) on session close
- **Zero breaking changes** -- existing sessions without shell integration work exactly as before

---

## 3. Design

### 3.1 Shell Type Detection

```rust
/// Detected shell type, derived from the spawn command path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellType {
    Zsh,
    Bash,
    Fish,
    Unknown(String),
}

impl ShellType {
    /// Detect shell type from a command path (e.g., "/bin/zsh", "bash", "/usr/local/bin/fish").
    pub fn detect(shell_cmd: &str) -> Self {
        let name = std::path::Path::new(shell_cmd)
            .file_name()
            .map_or(shell_cmd, |n| n.to_str().unwrap_or(shell_cmd));
        match name {
            "zsh" => Self::Zsh,
            "bash" => Self::Bash,
            "fish" => Self::Fish,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// Return the short name for display.
    pub fn name(&self) -> &str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
            Self::Unknown(n) => n,
        }
    }
}
```

### 3.2 Shell Integration Configuration

```rust
/// Configuration for shell integration features.
/// Controls which modifications are applied to the shell environment at spawn time.
#[derive(Debug, Clone)]
pub struct ShellIntegrationConfig {
    /// Disable autosuggestion plugins (zsh-autosuggestions, ble.sh, fish native).
    /// Default: true for AI sessions, false for manual terminals.
    pub disable_autosuggestions: bool,

    /// Export ZREMOTE_TERMINAL=1 and ZREMOTE_SESSION_ID=<uuid>.
    /// Default: true.
    pub export_env_vars: bool,

    /// Force SIGWINCH on zsh startup to fix resize race with GPUI.
    /// Default: true.
    pub force_sigwinch: bool,
}

impl ShellIntegrationConfig {
    /// Configuration for AI agent sessions (aggressive cleanup).
    pub fn for_ai_session() -> Self {
        Self {
            disable_autosuggestions: true,
            export_env_vars: true,
            force_sigwinch: true,
        }
    }

    /// Configuration for manual terminal sessions (minimal interference).
    pub fn for_manual_session() -> Self {
        Self {
            disable_autosuggestions: false,
            export_env_vars: true,
            force_sigwinch: true,
        }
    }

    /// Disabled -- no shell integration at all (backward-compatible behavior).
    pub fn disabled() -> Self {
        Self {
            disable_autosuggestions: false,
            export_env_vars: false,
            force_sigwinch: false,
        }
    }
}
```

### 3.3 Shell Integration State

Tracks temp files created during integration so they can be cleaned up on session close.

```rust
/// Tracks resources created by shell integration for a single session.
/// Stored alongside the session state and cleaned up on close.
pub struct ShellIntegrationState {
    /// Shell type detected at spawn time.
    pub shell_type: ShellType,
    /// Temp directory for custom ZDOTDIR (zsh) or rcfile (bash).
    /// None if no temp files were needed.
    pub temp_dir: Option<tempfile::TempDir>,
    /// PID of the shell process, used for graceful cleanup.
    pub shell_pid: Option<u32>,
}

impl ShellIntegrationState {
    /// Clean up temp resources. Called on session close.
    ///
    /// Cleanup strategy to avoid race conditions:
    /// 1. Send SIGHUP to the shell process (graceful termination signal).
    /// 2. Wait up to 500ms for the process to exit.
    /// 3. Clean up temp files regardless — stale temp files are harmless,
    ///    and the shell no longer needs them after receiving SIGHUP.
    ///
    /// If the process already exited, steps 1-2 are no-ops.
    pub fn cleanup(self) {
        if let Some(pid) = self.shell_pid {
            // Send SIGHUP for graceful shutdown
            #[cfg(unix)]
            {
                unsafe { libc::kill(pid as i32, libc::SIGHUP); }

                // Wait up to 500ms for process to exit
                let start = std::time::Instant::now();
                let timeout = std::time::Duration::from_millis(500);
                while start.elapsed() < timeout {
                    // Check if process still exists (kill with signal 0)
                    let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
                    if !alive {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                // After timeout, proceed with cleanup anyway.
                // Stale temp files are harmless and will be cleaned on next restart.
            }
        }
        drop(self.temp_dir);
    }
}
```

### 3.4 Integration per Shell

#### 3.4.1 Zsh Integration

Zsh loads config from `$ZDOTDIR/.zshrc` (or `~/.zshrc` if `ZDOTDIR` is unset). To inject overrides without breaking user config:

1. Create a temp directory as the new `ZDOTDIR`
2. Write a `.zshrc` that sources the user's original config, then applies overrides
3. Set `ZDOTDIR` env var on the PTY command

**Generated `.zshrc` content:**

```bash
# ZRemote shell integration for zsh
# Source user's original zshrc
if [[ -f "${ZREMOTE_ORIGINAL_ZDOTDIR:-$HOME}/.zshrc" ]]; then
    ZDOTDIR="${ZREMOTE_ORIGINAL_ZDOTDIR:-$HOME}" source "${ZREMOTE_ORIGINAL_ZDOTDIR:-$HOME}/.zshrc"
fi

# --- ZRemote overrides below ---

# Disable zsh-autosuggestions (nuclear: replace suggest function with noop)
if (( $+functions[_zsh_autosuggest_suggest] )); then
    _zsh_autosuggest_suggest() { :; }
    _zsh_autosuggest_clear() { :; }
fi

# Disable zsh-autocomplete if loaded
if (( $+functions[.autocomplete:async:start] )); then
    zstyle ':autocomplete:*' disabled yes
fi

# Preserve HIST_IGNORE_SPACE (commands prefixed with space are hidden from history)
setopt HIST_IGNORE_SPACE

# Force SIGWINCH to fix resize race with GPUI terminal.
# Strategy: wait for first prompt output (detected via first newline after spawn),
# then send SIGWINCH. Fallback: if no prompt detected within 500ms, send anyway.
{
    # Wait for prompt: read one line from terminal (blocks until prompt renders)
    # Use a background subshell with timeout to avoid blocking forever
    (
        # Try to detect prompt output via file descriptor readability
        if read -t 0.5 -n 1 < /dev/tty 2>/dev/null; then
            kill -WINCH $$ 2>/dev/null
        else
            # Fallback: 100ms delay if no prompt detected within 500ms
            sleep 0.1
            kill -WINCH $$ 2>/dev/null
        fi
    ) &
    disown
}
```

**Implementation:**

```rust
fn prepare_zsh_integration(
    session_id: SessionId,
    config: &ShellIntegrationConfig,
    cmd: &mut CommandBuilder,
) -> Result<Option<tempfile::TempDir>, std::io::Error> {
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("zremote-zsh-{session_id}-"))
        .tempdir()?;

    let mut zshrc = String::new();
    zshrc.push_str("# ZRemote shell integration for zsh\n");

    // Source user's original config
    zshrc.push_str(
        "if [[ -f \"${ZREMOTE_ORIGINAL_ZDOTDIR:-$HOME}/.zshrc\" ]]; then\n\
         \    ZDOTDIR=\"${ZREMOTE_ORIGINAL_ZDOTDIR:-$HOME}\" source \"${ZREMOTE_ORIGINAL_ZDOTDIR:-$HOME}/.zshrc\"\n\
         fi\n\n"
    );

    if config.disable_autosuggestions {
        zshrc.push_str(
            "# Disable zsh-autosuggestions\n\
             if (( $+functions[_zsh_autosuggest_suggest] )); then\n\
             \    _zsh_autosuggest_suggest() { :; }\n\
             \    _zsh_autosuggest_clear() { :; }\n\
             fi\n\
             # Disable zsh-autocomplete\n\
             if (( $+functions[.autocomplete:async:start] )); then\n\
             \    zstyle ':autocomplete:*' disabled yes\n\
             fi\n\n"
        );
    }

    zshrc.push_str("setopt HIST_IGNORE_SPACE\n");

    if config.force_sigwinch {
        // Strategy: wait for first prompt output, then send SIGWINCH.
        // Fallback: if no prompt detected within 500ms, send after 100ms delay.
        zshrc.push_str(
            "# Force SIGWINCH after prompt renders (or fallback after 100ms)\n\
             {\n\
             \    (\n\
             \        if read -t 0.5 -n 1 < /dev/tty 2>/dev/null; then\n\
             \            kill -WINCH $$ 2>/dev/null\n\
             \        else\n\
             \            sleep 0.1\n\
             \            kill -WINCH $$ 2>/dev/null\n\
             \        fi\n\
             \    ) &\n\
             \    disown\n\
             }\n"
        );
    }

    std::fs::write(temp_dir.path().join(".zshrc"), &zshrc)?;

    // Preserve original ZDOTDIR so user config can be sourced
    if let Ok(original) = std::env::var("ZDOTDIR") {
        cmd.env("ZREMOTE_ORIGINAL_ZDOTDIR", &original);
    }
    cmd.env("ZDOTDIR", temp_dir.path().to_string_lossy().as_ref());

    Ok(Some(temp_dir))
}
```

#### 3.4.2 Bash Integration

Bash supports `--rcfile` to specify an alternative init file. Similar pattern:

1. Create a temp rcfile that sources `~/.bashrc` then applies overrides
2. Modify the command to use `bash --rcfile <temp_path>`

**Generated rcfile content:**

```bash
# ZRemote shell integration for bash
# Source user's bashrc
if [[ -f "$HOME/.bashrc" ]]; then
    source "$HOME/.bashrc"
fi

# --- ZRemote overrides below ---

# Disable ble.sh autosuggestions if loaded.
# Detection: check the $_ble_bash variable (set by ble.sh on load) and also
# check at runtime if the ble-0 function exists (covers cases where ble.sh is
# sourced from .bash_profile, .profile, or other non-.bashrc init files).
if [[ -n "${_ble_bash}" ]] || type ble-0 &>/dev/null; then
    ble-detach 2>/dev/null || true
fi
```

**Implementation:**

```rust
fn prepare_bash_integration(
    session_id: SessionId,
    config: &ShellIntegrationConfig,
    cmd: &mut CommandBuilder,
) -> Result<Option<tempfile::TempDir>, std::io::Error> {
    let temp_dir = tempfile::Builder::new()
        .prefix(&format!("zremote-bash-{session_id}-"))
        .tempdir()?;

    let rcfile_path = temp_dir.path().join(".bashrc");
    let mut rcfile = String::new();

    rcfile.push_str("# ZRemote shell integration for bash\n");
    rcfile.push_str(
        "if [[ -f \"$HOME/.bashrc\" ]]; then\n\
         \    source \"$HOME/.bashrc\"\n\
         fi\n\n"
    );

    if config.disable_autosuggestions {
        // Detect ble.sh via $_ble_bash variable (set on load) and also via
        // runtime check for ble-0 function (covers sourcing from .bash_profile,
        // .profile, or other non-.bashrc init files).
        rcfile.push_str(
            "# Disable ble.sh\n\
             if [[ -n \"${_ble_bash}\" ]] || type ble-0 &>/dev/null; then\n\
             \    ble-detach 2>/dev/null || true\n\
             fi\n\n"
        );
    }

    std::fs::write(&rcfile_path, &rcfile)?;

    // Modify command to use --rcfile
    cmd.arg("--rcfile");
    cmd.arg(rcfile_path.to_string_lossy().as_ref());

    Ok(Some(temp_dir))
}
```

**Note on `CommandBuilder` change for bash:** Currently `PtySession::spawn` creates `CommandBuilder::new(shell)` with no arguments. For bash integration, the shell command becomes `bash --rcfile /tmp/zremote-bash-<id>/.bashrc`. This requires the spawn path to accept additional arguments from the integration layer. See section 3.6 for the modified spawn signature.

#### 3.4.3 Fish Integration

Fish supports `-C` (init command) to run commands at startup:

```rust
fn prepare_fish_integration(
    config: &ShellIntegrationConfig,
    cmd: &mut CommandBuilder,
) -> Result<Option<tempfile::TempDir>, std::io::Error> {
    if config.disable_autosuggestions {
        // Fish native autosuggestions: disable via global variable override.
        // Use `set -g` (global scope) instead of plain `set` to prevent
        // universal variables from overriding the session-scoped setting.
        cmd.arg("-C");
        cmd.arg("set -g fish_autosuggestion_enabled 0; function fish_suggest; end");
    }
    // Fish does not need temp files
    Ok(None)
}
```

#### 3.4.4 Environment Variables (All Shells)

Applied unconditionally when `config.export_env_vars` is true:

```rust
fn apply_env_vars(session_id: SessionId, cmd: &mut CommandBuilder) {
    cmd.env("ZREMOTE_TERMINAL", "1");
    cmd.env("ZREMOTE_SESSION_ID", &session_id.to_string());
}
```

### 3.5 Top-Level Prepare Function

Single entry point that dispatches to per-shell integration:

```rust
/// Prepare shell integration for a PTY session.
/// Modifies the CommandBuilder in-place and returns state for cleanup.
///
/// Returns `None` if integration is fully disabled (backward-compatible path).
pub fn prepare(
    session_id: SessionId,
    shell_cmd: &str,
    config: &ShellIntegrationConfig,
    cmd: &mut CommandBuilder,
) -> Result<Option<ShellIntegrationState>, std::io::Error> {
    // If everything is disabled, skip entirely
    if !config.disable_autosuggestions && !config.export_env_vars && !config.force_sigwinch {
        return Ok(None);
    }

    let shell_type = ShellType::detect(shell_cmd);

    if config.export_env_vars {
        apply_env_vars(session_id, cmd);
    }

    let temp_dir = match &shell_type {
        ShellType::Zsh => prepare_zsh_integration(session_id, config, cmd)?,
        ShellType::Bash => prepare_bash_integration(session_id, config, cmd)?,
        ShellType::Fish => prepare_fish_integration(config, cmd)?,
        ShellType::Unknown(_) => {
            // Unknown shells: skip shell-specific config (ZDOTDIR, --rcfile, etc.)
            // but environment variables (ZREMOTE_TERMINAL, ZREMOTE_SESSION_ID) are
            // already applied above via apply_env_vars(), so the session is still
            // identifiable from within the shell.
            None
        }
    };

    Ok(Some(ShellIntegrationState { shell_type, temp_dir }))
}
```

### 3.6 Modified Spawn Path

The `PtySession::spawn` and `DaemonSession::spawn` methods need to accept an optional `ShellIntegrationConfig`. The integration modifies the `CommandBuilder` before `spawn_command` is called.

**Current flow** (`pty.rs:24-56`):

```
PtySession::spawn(session_id, shell, cols, rows, working_dir, env, output_tx)
  → CommandBuilder::new(shell)
  → cmd.env("TERM", ...)
  → cmd.cwd(working_dir)
  → cmd.env(user_env_vars)
  → pair.slave.spawn_command(cmd)
```

**Proposed flow:**

```
PtySession::spawn(session_id, shell, cols, rows, working_dir, env, output_tx, shell_integration)
  → CommandBuilder::new(shell)
  → cmd.env("TERM", ...)
  → cmd.cwd(working_dir)
  → cmd.env(user_env_vars)
  → shell_integration::prepare(session_id, shell, &config, &mut cmd)  // NEW
  → pair.slave.spawn_command(cmd)
  → return (session, pid, integration_state)  // NEW: return state for cleanup
```

**Modified `PtySession::spawn` signature:**

```rust
pub fn spawn(
    session_id: SessionId,
    shell: &str,
    cols: u16,
    rows: u16,
    working_dir: Option<&str>,
    env: Option<&std::collections::HashMap<String, String>>,
    output_tx: mpsc::Sender<PtyOutput>,
    shell_config: Option<&ShellIntegrationConfig>,  // NEW
) -> Result<(Self, u32, Option<ShellIntegrationState>), Box<dyn std::error::Error + Send + Sync>>
```

**Modified `SessionManager::create` signature:**

```rust
pub async fn create(
    &mut self,
    session_id: SessionId,
    shell: &str,
    cols: u16,
    rows: u16,
    working_dir: Option<&str>,
    env: Option<&std::collections::HashMap<String, String>>,
    shell_config: Option<&ShellIntegrationConfig>,  // NEW
) -> Result<u32, Box<dyn std::error::Error + Send + Sync>>
```

`SessionManager` stores the `ShellIntegrationState` in a new `HashMap<SessionId, ShellIntegrationState>` alongside the existing `shell_names` map. On `close()`, the state is removed and `cleanup()` is called.

### 3.7 Daemon Mode Integration

`DaemonSession::spawn` builds args for the `pty-daemon` subprocess. Shell integration config is passed as additional CLI flags:

```
zremote pty-daemon \
    --session-id <uuid> \
    --shell /bin/zsh \
    --disable-autosuggestions \
    --export-env-vars \
    --force-sigwinch \
    ...
```

The `pty-daemon` subprocess applies `shell_integration::prepare()` before spawning the shell. Temp file cleanup happens when the daemon exits (the `TempDir` is held in the daemon's memory and dropped on process exit).

### 3.8 Relationship to Output Analyzer

The Output Analyzer (Phase 1) benefits directly from shell integration:

| Integration feature | Analyzer benefit |
|---|---|
| Disabled autosuggestions | No ghost text in PTY output -- prompt regex matches are more reliable |
| Force SIGWINCH | Correct terminal dimensions from the start -- line wrap calculations are accurate |
| ZREMOTE_SESSION_ID env var | Future: agent scripts can query ZRemote API for session context |

The analyzer does not depend on shell integration being enabled -- it must handle raw PTY output regardless. Shell integration improves accuracy but is not required.

---

## 4. Files

### CREATE

| File | Description |
|------|-------------|
| `crates/zremote-agent/src/pty/shell_integration.rs` | `ShellType`, `ShellIntegrationConfig`, `ShellIntegrationState`, `prepare()`, per-shell init generators |
| `crates/zremote-agent/src/pty/mod.rs` | Module root (current `pty.rs` moves here), re-exports `PtySession` + `shell_integration` |

### MODIFY

| File | Change |
|------|--------|
| `crates/zremote-agent/src/pty.rs` | Rename to `crates/zremote-agent/src/pty/mod.rs`, add `pub mod shell_integration;`, modify `PtySession::spawn` to accept `ShellIntegrationConfig` and call `shell_integration::prepare()`, return `ShellIntegrationState` |
| `crates/zremote-agent/src/session.rs` | Add `shell_integrations: HashMap<SessionId, ShellIntegrationState>` to `SessionManager`, call `cleanup()` in `close()`, update `create()` signature to accept `ShellIntegrationConfig` |
| `crates/zremote-agent/src/connection/dispatch.rs` | Pass `ShellIntegrationConfig` to `session_manager.create()` in `handle_session_create` and Claude task spawn. Use `ShellIntegrationConfig::for_ai_session()` when spawning for agentic sessions, `for_manual_session()` for regular sessions |
| `crates/zremote-agent/src/daemon/session.rs` | Add shell integration CLI flags to daemon spawn args, pass config through |
| `crates/zremote-agent/src/local/routes/sessions.rs` | Pass `ShellIntegrationConfig` when creating sessions via local REST API |

---

## 5. Implementation Phases

### Phase 3a: Shell Detection and Configuration Types

- Implement `ShellType` enum with `detect()` and `name()` methods
- Implement `ShellIntegrationConfig` with factory methods
- Implement `ShellIntegrationState` with `cleanup()`
- Unit tests for shell detection from various paths
- **Estimate:** Small, isolated types with no external dependencies

### Phase 3b: Per-Shell Init Generators

- Implement `prepare_zsh_integration()` with temp ZDOTDIR and `.zshrc` generation
- Implement `prepare_bash_integration()` with temp rcfile generation
- Implement `prepare_fish_integration()` with `-C` flag
- Implement `apply_env_vars()` for all shells
- Implement top-level `prepare()` dispatcher
- Unit tests for generated script content (verify disable commands present/absent based on config)
- Integration tests: spawn shell, verify env vars are set, verify autosuggestion functions are overridden

### Phase 3c: PTY Spawn Integration

- Convert `pty.rs` to `pty/mod.rs` + `pty/shell_integration.rs` module structure
- Modify `PtySession::spawn` signature to accept `ShellIntegrationConfig`
- Call `shell_integration::prepare()` before `spawn_command`
- Return `ShellIntegrationState` alongside `(session, pid)`
- Modify `SessionManager::create` to accept and store `ShellIntegrationState`
- Cleanup in `SessionManager::close()`
- Update `DaemonSession::spawn` to pass integration flags
- Integration tests: spawn PTY with integration, verify `ZREMOTE_TERMINAL=1` is set in child environment

### Phase 3d: Wiring and Dispatch

- Update `handle_session_create` in `dispatch.rs` to pass config
- Determine AI vs manual session from context (agentic detector, Claude task creation) and use appropriate config
- Update local mode session routes
- End-to-end test: create session via dispatch, verify shell integration applied

---

## 6. Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| Custom ZDOTDIR breaks user's zsh plugins | Medium -- user loses plugin functionality | Source user config first, overrides are additive only. Only disable specific autosuggestion functions, not entire plugin loading. |
| Temp file accumulation on crash | Low -- leaked temp dirs in `/tmp` | Use `tempfile::TempDir` (auto-cleanup on drop). Agent startup could sweep stale `zremote-zsh-*` dirs. |
| `--rcfile` in bash skips `/etc/profile` | Medium -- system-wide settings lost | The generated rcfile sources `~/.bashrc`, which typically sources `/etc/profile`. Document this behavior. If issues arise, add explicit `/etc/profile` sourcing. |
| `CommandBuilder` argument injection for bash | Medium -- if shell path contains spaces or special chars | Shell path is validated by `portable_pty` before use. `--rcfile` path is from `TempDir` (system-controlled). |
| Shell integration delays PTY startup | Low -- file writes are sub-ms | Temp file creation is a single `fs::write` call. No network I/O. |
| Fish `fish_suggest` override insufficient | Low -- fish may use different function names across versions | Fish autosuggestions are built-in. The `-C` override is the documented way to disable them. Verify against fish 3.x and 4.x. |
| ZDOTDIR race if user zshrc modifies ZDOTDIR | Low -- unusual configuration | The generated `.zshrc` restores ZDOTDIR to the original value before sourcing user config, so nested ZDOTDIR references resolve correctly. |

---

## 7. Protocol Compatibility

Shell integration is entirely agent-local. No protocol changes are required:

| Aspect | Impact |
|--------|--------|
| `SessionManager::create` signature | Internal API change, not protocol |
| `PtySession::spawn` signature | Internal API change, not protocol |
| Environment variables | Set inside PTY child process, not visible in protocol messages |
| Temp files | Agent-local filesystem, no protocol involvement |

The `SessionCreate` server message already has an `env` field (`Option<HashMap<String, String>>`). Shell integration does NOT use this field -- it operates on the `CommandBuilder` directly. This keeps the protocol unchanged and avoids leaking integration details to the server.

If future phases want to expose shell type or integration status to the GUI, a new optional field on `SessionCreated` agent message can be added with `#[serde(default)]` for backward compatibility:

```rust
AgentMessage::SessionCreated {
    session_id: SessionId,
    shell: String,
    pid: u32,
    #[serde(default)]
    shell_type: Option<String>,  // "zsh", "bash", "fish", or null
}
```

This is NOT part of this RFC -- listed for future reference only.

---

## 8. Testing

### Unit Tests

| Test | Location | Description |
|------|----------|-------------|
| `shell_type_detect_zsh` | `shell_integration.rs` | `ShellType::detect("/bin/zsh")` returns `Zsh` |
| `shell_type_detect_usr_local_bash` | `shell_integration.rs` | `ShellType::detect("/usr/local/bin/bash")` returns `Bash` |
| `shell_type_detect_fish` | `shell_integration.rs` | `ShellType::detect("fish")` returns `Fish` (no path prefix) |
| `shell_type_detect_unknown` | `shell_integration.rs` | `ShellType::detect("/bin/nu")` returns `Unknown("nu")` |
| `shell_type_name` | `shell_integration.rs` | `.name()` returns correct short names |
| `config_for_ai_session` | `shell_integration.rs` | `for_ai_session()` has all features enabled |
| `config_for_manual_session` | `shell_integration.rs` | `for_manual_session()` has autosuggestions disabled=false |
| `config_disabled` | `shell_integration.rs` | `disabled()` has all features off |
| `zsh_integration_generates_zshrc` | `shell_integration.rs` | Verify generated `.zshrc` contains autosuggestion noop, SIGWINCH, user config sourcing |
| `zsh_integration_no_autosuggest_when_disabled` | `shell_integration.rs` | Config with `disable_autosuggestions=false` does not include the noop overrides |
| `bash_integration_generates_rcfile` | `shell_integration.rs` | Verify generated rcfile contains bashrc sourcing and ble.sh disable |
| `prepare_returns_none_when_all_disabled` | `shell_integration.rs` | `prepare()` with `disabled()` config returns `Ok(None)` |
| `prepare_sets_env_vars` | `shell_integration.rs` | Verify `ZREMOTE_TERMINAL` and `ZREMOTE_SESSION_ID` are set on CommandBuilder |
| `cleanup_removes_temp_dir` | `shell_integration.rs` | `ShellIntegrationState::cleanup()` removes the temp directory |

### Integration Tests

| Test | Location | Description |
|------|----------|-------------|
| `spawn_with_shell_integration_sets_env` | `pty/mod.rs` | Spawn `/bin/sh` with integration, run `echo $ZREMOTE_TERMINAL`, verify output contains `1` |
| `spawn_zsh_with_integration` | `pty/mod.rs` | Spawn `zsh` with AI config, verify `ZREMOTE_SESSION_ID` is set and matches expected UUID |
| `session_manager_cleanup_on_close` | `session.rs` | Create session with integration, close it, verify temp dir is removed |
| `session_manager_create_without_integration` | `session.rs` | Create session with `None` config (backward-compatible), verify normal operation |

### Negative Tests

| Test | Location | Description |
|------|----------|-------------|
| `malformed_zdotdir_path` | `shell_integration.rs` | Verify graceful failure when temp ZDOTDIR path contains invalid characters or is too long (>PATH_MAX) |
| `shell_fails_with_custom_config` | `shell_integration.rs` | Verify that if a shell fails to start with the custom rcfile/ZDOTDIR, the error is propagated cleanly and temp files are still cleaned up |
| `cleanup_after_crash` | `session.rs` | Simulate abrupt session termination (drop without close), verify `TempDir`'s `Drop` impl still removes temp files |
| `cleanup_with_running_process` | `session.rs` | Verify cleanup completes even when the shell process does not exit within the 500ms timeout after SIGHUP |
| `unknown_shell_no_crash` | `shell_integration.rs` | `prepare()` with `Unknown("nu")` shell does not panic, env vars are still set, no temp files created |
| `stale_temp_dir_sweep` | `shell_integration.rs` | Verify that leftover `zremote-zsh-*` / `zremote-bash-*` dirs in `/tmp` from prior crashed sessions do not interfere with new session creation |

### End-to-End Verification

1. Start local mode, create manual terminal session -- verify `echo $ZREMOTE_TERMINAL` returns `1`, autosuggestions NOT disabled
2. Start local mode, create AI session (via agentic flow) -- verify autosuggestions disabled, env vars set
3. Close AI session -- verify temp ZDOTDIR directory is cleaned up
4. Create session with daemon backend -- verify shell integration flags passed to daemon subprocess
5. Create session with unknown shell (e.g., `nu`) -- verify no crash, env vars (`ZREMOTE_TERMINAL`, `ZREMOTE_SESSION_ID`) are set, no shell-specific config applied (no ZDOTDIR, no --rcfile), no temp files created

---

## 9. Open Questions

| Question | Current answer | Revisit when |
|----------|---------------|-------------|
| Should integration config be exposed in the REST API? | No -- agent-local concern for now | If users request per-session override from GUI |
| Should we detect autosuggestion plugins before disabling? | No -- always write the override, noop if plugin not loaded | If override causes issues in clean shells |
| Should fish `set -U` (universal variable) be used instead of `set -g`? | No -- `set -g` (global scope) is session-scoped and prevents universal variables from overriding. `-C` init command applies the override at startup. | If fish integration proves unreliable |
| Should we support `nushell`, `elvish`, `xonsh`? | No -- only zsh, bash, fish for now | When user demand arises |
