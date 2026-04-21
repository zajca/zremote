use std::path::Path;

struct HookConfig {
    event: &'static str,
    matcher: &'static str,
    command_arg: &'static str,
    async_hook: bool,
}

/// Install the zremote hook scripts and update Claude Code settings.
///
/// Creates:
/// - `~/.zremote/hooks/zremote-hook.sh` - the hook script that curls the sidecar
/// - Updates `~/.claude/settings.json` with hook configuration
pub async fn install_hooks() -> Result<(), InstallError> {
    let home = std::env::var("HOME").map_err(|_| InstallError::HomeNotSet)?;
    install_hooks_at(Path::new(&home)).await
}

/// Check if hooks and statusLine are already correctly installed, avoiding
/// unnecessary settings.json rewrites that can race with Claude Code reads.
async fn is_already_installed(home: &Path, script_path: &Path) -> bool {
    let settings_path = home.join(".claude").join("settings.json");
    let Ok(content) = tokio::fs::read_to_string(&settings_path).await else {
        return false;
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };

    // Consider statusLine already installed if it points at ANY working zremote
    // ccline binary. This keeps a user's manually-chosen stable path (e.g.
    // `/etc/profiles/.../zremote agent ccline`) intact across rebuilds and
    // prevents re-writing the file on every start.
    //
    // Only fall back to matching against the current binary if no valid
    // statusLine is present, so broken `(deleted)` paths or non-zremote
    // configurations still get fixed.
    let existing_command = settings
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(|c| c.as_str());

    let status_ok = match existing_command {
        Some(cmd) if existing_status_line_is_valid(cmd) => true,
        Some(cmd) => build_ccline_command().as_deref() == Some(cmd),
        None => false,
    };

    if !status_ok {
        return false;
    }

    // Check hook script exists
    if !script_path.exists() {
        return false;
    }

    // Check all required hook events are present with zremote entries
    let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };

    let required_events = [
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "Notification",
        "Elicitation",
        "UserPromptSubmit",
        "SessionStart",
        "SubagentStart",
        "SubagentStop",
        "StopFailure",
        "FileChanged",
        "CwdChanged",
    ];

    for event in &required_events {
        let has_zremote_hook = hooks
            .get(*event)
            .and_then(|e| e.as_array())
            .is_some_and(|arr| {
                arr.iter().any(|entry| {
                    entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .is_some_and(|hooks| {
                            hooks.iter().any(|h| {
                                h.get("command")
                                    .and_then(|c| c.as_str())
                                    .is_some_and(|c| c.contains("zremote-hook"))
                            })
                        })
                })
            });
        if !has_zremote_hook {
            return false;
        }
    }

    true
}

/// Install hooks at a specific home directory path (testable).
async fn install_hooks_at(home: &Path) -> Result<(), InstallError> {
    let script_path = home.join(".zremote").join("hooks").join("zremote-hook.sh");

    // Check if hooks are already correctly installed (skip redundant writes to
    // avoid race conditions with Claude Code reading settings.json).
    if is_already_installed(home, &script_path).await {
        tracing::debug!("hooks already installed, skipping");
        return Ok(());
    }

    // Create hook script
    let hooks_dir = home.join(".zremote").join("hooks");
    tokio::fs::create_dir_all(&hooks_dir)
        .await
        .map_err(InstallError::Io)?;

    let script_content = generate_hook_script();
    tokio::fs::write(&script_path, &script_content)
        .await
        .map_err(InstallError::Io)?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(&script_path, perms)
            .await
            .map_err(InstallError::Io)?;
    }

    tracing::info!(path = %script_path.display(), "hook script installed");

    // Update Claude Code settings
    update_claude_settings(home, &script_path).await?;

    Ok(())
}

fn generate_hook_script() -> String {
    r#"#!/bin/sh
# ZRemote hook script - forwards Claude Code hook events to the agent sidecar.
# Managed by zremote-agent. Do not edit manually.
PORT=$(cat ~/.zremote/hooks-port 2>/dev/null) || exit 0
INPUT=$(cat -)
# Whitelist valid endpoints to prevent command injection via $1
case "${1:-hooks}" in
  hooks|hooks/notification/idle|hooks/notification/permission) ENDPOINT="${1:-hooks}" ;;
  *) exit 1 ;;
esac
# Forward CLAUDE_ENV_FILE path (set by CC for SessionStart/CwdChanged/FileChanged)
if [ -n "$CLAUDE_ENV_FILE" ]; then
  RESPONSE=$(curl -s --max-time 60 -X POST "http://127.0.0.1:$PORT/$ENDPOINT" \
    -H "Content-Type: application/json" \
    -H "X-Claude-Env-File: $CLAUDE_ENV_FILE" \
    -d "$INPUT" 2>/dev/null)
else
  RESPONSE=$(curl -s --max-time 60 -X POST "http://127.0.0.1:$PORT/$ENDPOINT" \
    -H "Content-Type: application/json" \
    -d "$INPUT" 2>/dev/null)
fi
if [ -n "$RESPONSE" ]; then
  echo "$RESPONSE"
fi
exit 0
"#
    .to_string()
}

async fn update_claude_settings(home: &Path, script_path: &Path) -> Result<(), InstallError> {
    let settings_path = home.join(".claude").join("settings.json");

    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = tokio::fs::read_to_string(&settings_path)
            .await
            .map_err(InstallError::Io)?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        // Create .claude directory if needed
        if let Some(parent) = settings_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(InstallError::Io)?;
        }
        serde_json::json!({})
    };

    let script = script_path.to_string_lossy().to_string();

    // Per-event hook configuration.
    // NOTE: No catch-all Notification entry -- CC fires ALL matching hooks,
    // so a catch-all would duplicate typed notification handlers.
    let hook_configs = [
        HookConfig {
            event: "PreToolUse",
            matcher: "",
            command_arg: "hooks",
            async_hook: false,
        },
        HookConfig {
            event: "PostToolUse",
            matcher: "",
            command_arg: "hooks",
            async_hook: false,
        },
        HookConfig {
            event: "Stop",
            matcher: "",
            command_arg: "hooks",
            async_hook: false,
        },
        HookConfig {
            event: "Notification",
            matcher: "idle_prompt",
            command_arg: "hooks/notification/idle",
            async_hook: false,
        },
        HookConfig {
            event: "Notification",
            matcher: "permission_prompt",
            command_arg: "hooks/notification/permission",
            async_hook: false,
        },
        HookConfig {
            event: "Elicitation",
            matcher: "",
            command_arg: "hooks",
            async_hook: false,
        },
        HookConfig {
            event: "UserPromptSubmit",
            matcher: "",
            command_arg: "hooks",
            async_hook: true,
        },
        HookConfig {
            event: "SessionStart",
            matcher: "",
            command_arg: "hooks",
            async_hook: false,
        },
        HookConfig {
            event: "SubagentStart",
            matcher: "",
            command_arg: "hooks",
            async_hook: true,
        },
        HookConfig {
            event: "SubagentStop",
            matcher: "",
            command_arg: "hooks",
            async_hook: true,
        },
        HookConfig {
            event: "StopFailure",
            matcher: "",
            command_arg: "hooks",
            async_hook: true,
        },
        HookConfig {
            event: "FileChanged",
            matcher: "",
            command_arg: "hooks",
            async_hook: true,
        },
        HookConfig {
            event: "CwdChanged",
            matcher: "",
            command_arg: "hooks",
            async_hook: false,
        },
    ];

    // Merge into existing hooks (preserve user's own hooks)
    let hooks = settings
        .as_object_mut()
        .ok_or(InstallError::InvalidSettings)?
        .entry("hooks")
        .or_insert(serde_json::json!({}));

    let hooks_obj = hooks.as_object_mut().ok_or(InstallError::InvalidSettings)?;

    // Remove legacy myremote hooks (replaced by zremote hooks)
    for (_, event_hooks) in hooks_obj.iter_mut() {
        if let Some(arr) = event_hooks.as_array_mut() {
            let before = arr.len();
            arr.retain(|entry| {
                !entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c.contains("myremote-hook"))
                        })
                    })
            });
            if arr.len() < before {
                tracing::info!(
                    removed = before - arr.len(),
                    "removed legacy myremote hook entries"
                );
            }
        }
    }

    for config in &hook_configs {
        let event_hooks = hooks_obj
            .entry(config.event)
            .or_insert(serde_json::json!([]));

        if let Some(arr) = event_hooks.as_array_mut() {
            // Check if this specific zremote hook entry is already present
            // (match by both command containing "zremote-hook" and same matcher)
            let already_installed = arr.iter().any(|entry| {
                let matcher_matches = entry
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .is_some_and(|m| m == config.matcher);
                let command_matches =
                    entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .is_some_and(|hooks| {
                            hooks.iter().any(|h| {
                                h.get("command")
                                    .and_then(|c| c.as_str())
                                    .is_some_and(|c| c.contains("zremote-hook"))
                            })
                        });
                matcher_matches && command_matches
            });

            if !already_installed {
                let command = if config.command_arg == "hooks" {
                    script.clone()
                } else {
                    format!("{script} {}", config.command_arg)
                };

                let mut hook_entry = serde_json::json!({
                    "matcher": config.matcher,
                    "hooks": [{
                        "type": "command",
                        "command": command
                    }]
                });

                if config.async_hook {
                    hook_entry["hooks"][0]["async"] = serde_json::json!(true);
                }

                arr.push(hook_entry);
            }
        }
    }

    // Set statusLine to use the agent's own ccline subcommand
    install_status_line(&mut settings);

    // Write back atomically (write to tmp, then rename) to avoid Claude Code
    // reading a truncated file during the write.
    let formatted =
        serde_json::to_string_pretty(&settings).map_err(|_| InstallError::InvalidSettings)?;
    let tmp_path = settings_path.with_extension("json.tmp");
    tokio::fs::write(&tmp_path, &formatted)
        .await
        .map_err(InstallError::Io)?;
    if let Err(e) = tokio::fs::rename(&tmp_path, &settings_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(InstallError::Io(e));
    }

    tracing::info!(
        path = %settings_path.display(),
        "Claude Code settings updated with zremote hooks"
    );

    Ok(())
}

/// Resolve the path of the currently running binary to a stable, on-disk path.
///
/// On Linux, `std::env::current_exe()` reads `/proc/self/exe`, which returns a
/// path with a trailing `" (deleted)"` marker if the original file was replaced
/// or removed after the process started (common after `cargo build` or a
/// package upgrade). Writing such a path into `settings.json` permanently
/// breaks the status line. We strip the marker and verify the resulting path
/// still refers to an existing file; if not, we refuse to build a command.
fn resolved_current_exe() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let cleaned = strip_deleted_marker(&exe);
    if cleaned.is_file() {
        Some(cleaned)
    } else {
        None
    }
}

/// Strip the Linux `" (deleted)"` suffix from a path returned by
/// `current_exe()` / `/proc/self/exe` when the backing file no longer exists.
/// This is a no-op on other platforms; Windows and macOS do not emit the
/// marker, and a genuine filename ending in ` (deleted)` is extremely
/// unlikely (and would be rejected downstream when the file existence check
/// fails anyway).
fn strip_deleted_marker(exe: &Path) -> std::path::PathBuf {
    const MARKER: &str = " (deleted)";
    exe.to_str()
        .and_then(|s| s.strip_suffix(MARKER))
        .map_or_else(|| exe.to_path_buf(), std::path::PathBuf::from)
}

/// Build the statusLine command string for the current binary.
///
/// Uses runtime filename inspection to distinguish the unified `zremote`
/// binary (needs `agent ccline`) from the standalone `zremote-agent` (just
/// `ccline`). Returns `None` when the current binary path cannot be resolved
/// to a real file on disk — this keeps stale `(deleted)` paths out of
/// `settings.json`.
fn build_ccline_command() -> Option<String> {
    let exe = resolved_current_exe()?;
    let exe_str = exe.to_str()?;

    if is_standalone_agent_path(&exe) {
        Some(format!("{exe_str} ccline"))
    } else {
        Some(format!("{exe_str} agent ccline"))
    }
}

/// Returns `true` when running as the standalone `zremote-agent` binary.
/// Uses runtime filename check since `CARGO_BIN_NAME` is not available in
/// library crates.
fn is_standalone_agent() -> bool {
    std::env::current_exe()
        .ok()
        .map(|p| strip_deleted_marker(&p))
        .as_deref()
        .is_some_and(is_standalone_agent_path)
}

/// Classify a resolved binary path as standalone `zremote-agent` vs. unified
/// `zremote`. Split out so callers that already resolved `current_exe()` (e.g.
/// `build_ccline_command`) don't have to read `/proc/self/exe` twice.
fn is_standalone_agent_path(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .is_some_and(|name| {
            // Match "zremote-agent" but not "zremote-agent-<hash>" (test binary)
            // or just "zremote" (unified binary)
            name == "zremote-agent" || name.starts_with("zremote-agent.")
        })
}

/// Return `true` if the statusLine `command` string points at an existing,
/// executable zremote ccline binary. Accepts both `"<path> ccline"` (standalone
/// agent) and `"<path> agent ccline"` (unified binary); the longer suffix is
/// checked first so a unified-binary path is not misparsed as `-agent`.
///
/// Limitation: the parser does not strip surrounding shell quotes, so a
/// command like `"\"/opt/my path/zremote\" agent ccline"` would fail the
/// existence check and be rejected. Users with spaces in the install path
/// should not wrap the command in quotes — Claude Code passes the whole
/// string to `/bin/sh -c`, which handles an unquoted path with spaces
/// identically to the one zremote itself writes.
fn existing_status_line_is_valid(command: &str) -> bool {
    if !command.contains("ccline") {
        return false;
    }
    let binary_path = command
        .strip_suffix(" agent ccline")
        .or_else(|| command.strip_suffix(" ccline"));
    let Some(path) = binary_path else {
        return false;
    };
    // Reject Linux `(deleted)` marker or any path whose file no longer exists.
    if path.ends_with(" (deleted)") {
        return false;
    }
    let p = Path::new(path);
    if !p.is_file() {
        return false;
    }
    // Best-effort: ensure the file has execute bits on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(p)
            && meta.permissions().mode() & 0o111 == 0
        {
            return false;
        }
    }
    true
}

/// Install the `statusLine` config pointing to the current zremote binary.
///
/// Behavior:
/// - If an existing `statusLine.command` already points at a working zremote
///   ccline binary, leave it alone. This keeps manually-chosen stable paths
///   across rebuilds and avoids rewriting `settings.json` on every start.
/// - If the current binary path cannot be resolved (e.g. Linux `(deleted)`
///   marker with no backing file), skip the write entirely rather than
///   persist a broken command.
/// - Otherwise install the command built from the current binary.
fn install_status_line(settings: &mut serde_json::Value) {
    let Some(obj) = settings.as_object_mut() else {
        return;
    };

    if let Some(existing_cmd) = obj
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(|c| c.as_str())
        && existing_status_line_is_valid(existing_cmd)
    {
        tracing::debug!(
            command = existing_cmd,
            "existing statusLine is valid, preserving"
        );
        return;
    }

    let Some(command) = build_ccline_command() else {
        tracing::warn!("cannot determine a valid agent binary path, skipping statusLine install");
        return;
    };

    if let Some(existing) = obj.get("statusLine") {
        let existing_cmd = existing
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if !existing_cmd.contains("zremote") {
            tracing::warn!(
                existing = existing_cmd,
                "overwriting existing statusLine config"
            );
        }
    }

    obj.insert(
        "statusLine".to_string(),
        serde_json::json!({
            "type": "command",
            "command": command,
            "padding": 0
        }),
    );
    tracing::info!(command, "statusLine configured");
}

/// Remove zremote hooks from Claude Code settings.
#[allow(dead_code)]
pub async fn uninstall_hooks() -> Result<(), InstallError> {
    let home = std::env::var("HOME").map_err(|_| InstallError::HomeNotSet)?;
    uninstall_hooks_at(Path::new(&home)).await
}

/// Remove zremote hooks at a specific home directory path (testable).
async fn uninstall_hooks_at(home: &Path) -> Result<(), InstallError> {
    let settings_path = home.join(".claude").join("settings.json");
    if !settings_path.exists() {
        return Ok(());
    }

    let content = tokio::fs::read_to_string(&settings_path)
        .await
        .map_err(InstallError::Io)?;
    let mut settings: serde_json::Value =
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}));

    if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for (_event, event_hooks) in hooks.iter_mut() {
            if let Some(arr) = event_hooks.as_array_mut() {
                arr.retain(|entry| {
                    !entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .is_some_and(|hooks| {
                            hooks.iter().any(|h| {
                                h.get("command")
                                    .and_then(|c| c.as_str())
                                    .is_some_and(|c| c.contains("zremote-hook"))
                            })
                        })
                });
            }
        }
    }

    // Remove statusLine if it points to zremote (unified or standalone binary)
    if let Some(obj) = settings.as_object_mut() {
        let is_zremote = obj
            .get("statusLine")
            .and_then(|s| s.get("command"))
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains("zremote") && c.contains("ccline"));
        if is_zremote {
            obj.remove("statusLine");
        }
    }

    let formatted =
        serde_json::to_string_pretty(&settings).map_err(|_| InstallError::InvalidSettings)?;
    let tmp_path = settings_path.with_extension("json.tmp");
    tokio::fs::write(&tmp_path, &formatted)
        .await
        .map_err(InstallError::Io)?;
    if let Err(e) = tokio::fs::rename(&tmp_path, &settings_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(InstallError::Io(e));
    }

    // Remove hook script
    let script_path = home.join(".zremote").join("hooks").join("zremote-hook.sh");
    let _ = tokio::fs::remove_file(&script_path).await;

    Ok(())
}

#[derive(Debug)]
pub enum InstallError {
    HomeNotSet,
    Io(std::io::Error),
    InvalidSettings,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HomeNotSet => write!(f, "HOME environment variable not set"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::InvalidSettings => write!(f, "invalid Claude Code settings.json format"),
        }
    }
}

impl std::error::Error for InstallError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_ccline_command_returns_some() {
        // In test context CARGO_BIN_NAME is neither "zremote" nor "zremote-agent",
        // so it falls back to runtime check. The important thing is it returns Some.
        let cmd = build_ccline_command();
        assert!(cmd.is_some(), "build_ccline_command should return Some");
        let cmd = cmd.unwrap();
        assert!(cmd.contains("ccline"), "command must contain ccline");
        assert!(
            !cmd.contains("(deleted)"),
            "command must never embed Linux `(deleted)` marker"
        );
    }

    #[test]
    fn strip_deleted_marker_removes_suffix() {
        let p = Path::new("/usr/local/bin/zremote (deleted)");
        let stripped = strip_deleted_marker(p);
        assert_eq!(stripped, Path::new("/usr/local/bin/zremote"));
    }

    #[test]
    fn strip_deleted_marker_leaves_clean_path() {
        let p = Path::new("/usr/local/bin/zremote");
        let stripped = strip_deleted_marker(p);
        assert_eq!(stripped, p);
    }

    #[test]
    fn existing_status_line_rejects_deleted_marker() {
        assert!(!existing_status_line_is_valid(
            "/home/x/target/debug/zremote (deleted) agent ccline"
        ));
    }

    #[test]
    fn existing_status_line_rejects_nonexistent_path() {
        assert!(!existing_status_line_is_valid(
            "/nonexistent/path/zremote agent ccline"
        ));
        assert!(!existing_status_line_is_valid(
            "/nonexistent/path/zremote-agent ccline"
        ));
    }

    #[test]
    fn existing_status_line_rejects_non_ccline_command() {
        assert!(!existing_status_line_is_valid("/bin/sh echo hi"));
        assert!(!existing_status_line_is_valid(""));
    }

    #[test]
    fn existing_status_line_accepts_real_executable() {
        // The current test binary exists and is executable; treat it as the
        // "zremote" binary to verify the `<path> ccline` form is accepted.
        let exe = resolved_current_exe().expect("test binary should resolve");
        let cmd_standalone = format!("{} ccline", exe.display());
        assert!(existing_status_line_is_valid(&cmd_standalone));

        let cmd_unified = format!("{} agent ccline", exe.display());
        assert!(existing_status_line_is_valid(&cmd_unified));
    }

    #[tokio::test]
    async fn install_preserves_existing_valid_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Pre-seed settings.json with a statusLine pointing at a REAL executable
        // (the test binary). The file intentionally has NO `hooks` section, so
        // `is_already_installed` returns false and the full install path runs.
        // This verifies that even during a full rewrite, `install_status_line`
        // preserves a pre-existing valid command instead of overwriting it.
        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        let exe = resolved_current_exe().expect("test binary must resolve");
        let stable_cmd = format!("{} agent ccline", exe.display());
        let settings = serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": stable_cmd,
                "padding": 0
            }
        });
        tokio::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .await
        .unwrap();

        install_hooks_at(home).await.unwrap();

        let content = tokio::fs::read_to_string(claude_dir.join("settings.json"))
            .await
            .unwrap();
        let updated: serde_json::Value = serde_json::from_str(&content).unwrap();
        let cmd = updated["statusLine"]["command"].as_str().unwrap();
        assert_eq!(
            cmd, stable_cmd,
            "installer must not overwrite an already-valid statusLine"
        );
    }

    #[tokio::test]
    async fn install_replaces_deleted_marker_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        let settings = serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": "/home/u/target/debug/zremote (deleted) agent ccline",
                "padding": 0
            }
        });
        tokio::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .await
        .unwrap();

        install_hooks_at(home).await.unwrap();

        let content = tokio::fs::read_to_string(claude_dir.join("settings.json"))
            .await
            .unwrap();
        let updated: serde_json::Value = serde_json::from_str(&content).unwrap();
        let cmd = updated["statusLine"]["command"].as_str().unwrap();
        assert!(
            !cmd.contains("(deleted)"),
            "broken `(deleted)` statusLine must be replaced, got: {cmd}"
        );
        assert!(cmd.contains("ccline"));
    }

    #[test]
    fn is_standalone_agent_falls_back_for_test_binary() {
        // In test context, CARGO_BIN_NAME is the test harness name.
        // The function should still return a valid result via fallback.
        let _result = is_standalone_agent();
        // Just verify it doesn't panic
    }

    #[tokio::test]
    async fn install_detects_stale_status_line_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Install once
        install_hooks_at(home).await.unwrap();

        // Manually change statusLine to a stale path
        let settings_path = home.join(".claude/settings.json");
        let content = tokio::fs::read_to_string(&settings_path).await.unwrap();
        let mut settings: serde_json::Value = serde_json::from_str(&content).unwrap();
        settings["statusLine"]["command"] =
            serde_json::json!("/old/worktree/target/debug/zremote ccline");
        tokio::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .await
        .unwrap();

        let script_path = home.join(".zremote").join("hooks").join("zremote-hook.sh");

        // is_already_installed should return false due to mismatched statusLine
        assert!(
            !is_already_installed(home, &script_path).await,
            "stale statusLine path should trigger reinstall"
        );

        // Reinstalling should fix the path
        install_hooks_at(home).await.unwrap();
        let content = tokio::fs::read_to_string(&settings_path).await.unwrap();
        let settings: serde_json::Value = serde_json::from_str(&content).unwrap();
        let cmd = settings["statusLine"]["command"].as_str().unwrap();
        assert!(
            !cmd.contains("/old/worktree/"),
            "statusLine should be updated to current binary"
        );
    }

    #[tokio::test]
    async fn uninstall_removes_unified_binary_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Create settings with unified binary statusLine (zremote agent ccline)
        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        let settings = serde_json::json!({
            "hooks": {},
            "statusLine": {
                "type": "command",
                "command": "/usr/local/bin/zremote agent ccline",
                "padding": 0
            }
        });
        tokio::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .await
        .unwrap();

        uninstall_hooks_at(home).await.unwrap();

        let content = tokio::fs::read_to_string(claude_dir.join("settings.json"))
            .await
            .unwrap();
        let updated: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            updated.get("statusLine").is_none(),
            "unified binary statusLine should be removed on uninstall"
        );
    }

    #[test]
    fn hook_script_content() {
        let script = generate_hook_script();
        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains("hooks-port"));
        assert!(script.contains("curl"));
        assert!(script.contains("ENDPOINT"));
        assert!(script.contains("exit 0"));
        // Verify whitelist validation against command injection
        assert!(script.contains("case"));
        assert!(script.contains("exit 1"));
    }

    #[tokio::test]
    async fn install_creates_script_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let result = install_hooks_at(home).await;
        assert!(result.is_ok());

        // Verify script exists
        let script = home.join(".zremote/hooks/zremote-hook.sh");
        assert!(script.exists());

        // Verify settings exist
        let settings_path = home.join(".claude/settings.json");
        assert!(settings_path.exists());

        let content = tokio::fs::read_to_string(&settings_path).await.unwrap();
        let settings: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(settings.get("hooks").is_some());

        // Verify all hook events are configured
        let hooks = settings["hooks"].as_object().unwrap();
        for event in &[
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "Notification",
            "Elicitation",
            "UserPromptSubmit",
            "SessionStart",
            "SubagentStart",
            "SubagentStop",
            "StopFailure",
            "FileChanged",
            "CwdChanged",
        ] {
            assert!(hooks.contains_key(*event), "missing hook event: {event}");
        }

        // Verify PreToolUse is NOT async (needs sync for additionalContext)
        let pre_tool = hooks["PreToolUse"].as_array().unwrap();
        let pre_tool_hook = &pre_tool[0]["hooks"][0];
        assert!(
            pre_tool_hook.get("async").is_none(),
            "PreToolUse should not be async"
        );

        // Verify Stop is NOT async
        let stop = hooks["Stop"].as_array().unwrap();
        let stop_hook = &stop[0]["hooks"][0];
        assert!(stop_hook.get("async").is_none(), "Stop should not be async");

        // Verify SessionStart is NOT async (needs sync for CLAUDE_ENV_FILE)
        let session_start = hooks["SessionStart"].as_array().unwrap();
        let session_start_hook = &session_start[0]["hooks"][0];
        assert!(
            session_start_hook.get("async").is_none(),
            "SessionStart should not be async"
        );

        // Verify Notification has separate entries for idle_prompt and permission_prompt
        // (no catch-all -- CC fires ALL matching hooks, catch-all would duplicate)
        let notifications = hooks["Notification"].as_array().unwrap();
        assert_eq!(
            notifications.len(),
            2,
            "Notification should have exactly idle_prompt and permission_prompt entries"
        );
        let matchers: Vec<&str> = notifications
            .iter()
            .filter_map(|e| e.get("matcher").and_then(|m| m.as_str()))
            .collect();
        assert!(
            matchers.contains(&"idle_prompt"),
            "missing idle_prompt matcher"
        );
        assert!(
            matchers.contains(&"permission_prompt"),
            "missing permission_prompt matcher"
        );
    }

    #[tokio::test]
    async fn install_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Install twice
        install_hooks_at(home).await.unwrap();
        install_hooks_at(home).await.unwrap();

        // Should have only one hook per event
        let settings_path = home.join(".claude/settings.json");
        let content = tokio::fs::read_to_string(&settings_path).await.unwrap();
        let settings: serde_json::Value = serde_json::from_str(&content).unwrap();

        let pre_tool = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 1, "should not duplicate hooks");
    }

    #[tokio::test]
    async fn install_preserves_existing_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Create existing settings with a user hook
        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        let existing = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/my-hook.sh"}]}
                ]
            }
        });
        tokio::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .await
        .unwrap();

        // Install zremote hooks
        install_hooks_at(home).await.unwrap();

        let content = tokio::fs::read_to_string(claude_dir.join("settings.json"))
            .await
            .unwrap();
        let settings: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Should have both hooks
        let pre_tool = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 2);
    }

    #[test]
    fn install_error_display() {
        assert!(InstallError::HomeNotSet.to_string().contains("HOME"));
        assert!(
            InstallError::InvalidSettings
                .to_string()
                .contains("settings.json")
        );
    }

    #[test]
    fn install_error_display_io() {
        let err = InstallError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file not found",
        ));
        let msg = err.to_string();
        assert!(msg.contains("I/O error"));
        assert!(msg.contains("file not found"));
    }

    #[test]
    fn install_error_is_error_trait() {
        // Verify InstallError implements std::error::Error
        let err: Box<dyn std::error::Error> = Box::new(InstallError::HomeNotSet);
        assert!(err.to_string().contains("HOME"));
    }

    #[tokio::test]
    async fn install_with_existing_non_object_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Create existing settings where hooks value is an array (invalid)
        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        let settings = serde_json::json!({
            "hooks": "not an object"
        });
        tokio::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .await
        .unwrap();

        // Should fail because hooks is not an object
        let result = install_hooks_at(home).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn install_with_invalid_json_settings() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Create existing settings with invalid JSON
        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        tokio::fs::write(claude_dir.join("settings.json"), "not json {{{")
            .await
            .unwrap();

        // Should handle gracefully (falls back to empty object)
        let result = install_hooks_at(home).await;
        assert!(result.is_ok());

        // Verify settings were written correctly
        let content = tokio::fs::read_to_string(claude_dir.join("settings.json"))
            .await
            .unwrap();
        let settings: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(settings["hooks"].is_object());
    }

    #[tokio::test]
    async fn uninstall_hooks_removes_zremote_entries() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Install first
        install_hooks_at(home).await.unwrap();

        // Verify hooks exist
        let settings_path = home.join(".claude/settings.json");
        let content = tokio::fs::read_to_string(&settings_path).await.unwrap();
        let settings: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            !settings["hooks"]["PreToolUse"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        // Uninstall using the testable function
        let result = uninstall_hooks_at(home).await;
        assert!(result.is_ok());

        // Verify hooks were removed
        let content = tokio::fs::read_to_string(&settings_path).await.unwrap();
        let settings: serde_json::Value = serde_json::from_str(&content).unwrap();
        for event in &[
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "Notification",
            "Elicitation",
            "UserPromptSubmit",
            "SessionStart",
            "SubagentStart",
            "SubagentStop",
            "StopFailure",
            "FileChanged",
            "CwdChanged",
        ] {
            let arr = settings["hooks"][event].as_array().unwrap();
            assert!(
                arr.is_empty(),
                "hook {event} should be empty after uninstall"
            );
        }

        // Verify script was removed
        let script = home.join(".zremote/hooks/zremote-hook.sh");
        assert!(!script.exists());
    }

    #[tokio::test]
    async fn uninstall_no_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Should succeed even if there's no settings file
        let result = uninstall_hooks_at(home).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn uninstall_preserves_user_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Create settings with both user and zremote hooks
        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/my-hook.sh"}]},
                    {"matcher": "", "hooks": [{"type": "command", "command": "/home/user/.zremote/hooks/zremote-hook.sh"}]}
                ]
            }
        });
        tokio::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .await
        .unwrap();

        uninstall_hooks_at(home).await.unwrap();

        let content = tokio::fs::read_to_string(claude_dir.join("settings.json"))
            .await
            .unwrap();
        let updated: serde_json::Value = serde_json::from_str(&content).unwrap();
        let pre_tool = updated["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 1, "user hook should be preserved");
        assert!(
            pre_tool[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("my-hook.sh")
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn install_sets_executable_permission() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        install_hooks_at(home).await.unwrap();

        let script = home.join(".zremote/hooks/zremote-hook.sh");
        let metadata = std::fs::metadata(&script).unwrap();
        let mode = metadata.permissions().mode();
        // Check that the executable bit is set
        assert!(
            mode & 0o111 != 0,
            "script should be executable, mode: {mode:o}"
        );
    }

    #[test]
    fn hook_script_is_complete_shell_script() {
        let script = generate_hook_script();
        // Verify it's a complete shell script
        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains("PORT="));
        assert!(script.contains("INPUT="));
        assert!(script.contains("ENDPOINT="));
        assert!(script.contains("RESPONSE="));
        assert!(script.contains("exit 0"));
        // Verify it reads from hooks-port file
        assert!(script.contains("hooks-port"));
        // Verify it POSTs to the sidecar
        assert!(script.contains("POST"));
        assert!(script.contains("127.0.0.1"));
        // Verify it uses ENDPOINT variable with default fallback
        assert!(script.contains("${1:-hooks}"));
        assert!(script.contains("$ENDPOINT"));
        // Verify CLAUDE_ENV_FILE forwarding
        assert!(script.contains("CLAUDE_ENV_FILE"));
        assert!(script.contains("X-Claude-Env-File"));
    }

    #[tokio::test]
    async fn install_with_root_level_non_object_settings() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Create settings that is not a JSON object (e.g., an array)
        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        tokio::fs::write(claude_dir.join("settings.json"), "[]")
            .await
            .unwrap();

        // Should fail since settings root is not an object
        let result = install_hooks_at(home).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn install_removes_legacy_myremote_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Create existing settings with legacy myremote hooks
        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        let existing = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/my-hook.sh"}]},
                    {"matcher": "", "hooks": [{"type": "command", "command": "/home/user/.myremote/hooks/myremote-hook.sh"}]}
                ],
                "Stop": [
                    {"matcher": "", "hooks": [{"type": "command", "command": "/home/user/.myremote/hooks/myremote-hook.sh"}]}
                ]
            }
        });
        tokio::fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .await
        .unwrap();

        install_hooks_at(home).await.unwrap();

        let content = tokio::fs::read_to_string(claude_dir.join("settings.json"))
            .await
            .unwrap();
        let settings: serde_json::Value = serde_json::from_str(&content).unwrap();

        // myremote hooks should be removed
        let pre_tool = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(
            !pre_tool.iter().any(|e| {
                e["hooks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|h| h["command"].as_str().unwrap().contains("myremote-hook"))
            }),
            "myremote hooks should be removed from PreToolUse"
        );

        // User's own hook should be preserved
        assert!(
            pre_tool.iter().any(|e| e["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|h| h["command"].as_str().unwrap().contains("my-hook.sh"))),
            "user hook should be preserved"
        );

        // Stop should have no myremote hooks
        let stop = settings["hooks"]["Stop"].as_array().unwrap();
        assert!(
            !stop.iter().any(|e| {
                e["hooks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|h| h["command"].as_str().unwrap().contains("myremote-hook"))
            }),
            "myremote hooks should be removed from Stop"
        );
    }
}
