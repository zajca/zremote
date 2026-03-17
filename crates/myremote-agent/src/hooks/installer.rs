use std::path::Path;

/// Install the myremote hook scripts and update Claude Code settings.
///
/// Creates:
/// - `~/.myremote/hooks/myremote-hook.sh` - the hook script that curls the sidecar
/// - Updates `~/.claude/settings.json` with hook configuration
pub async fn install_hooks() -> Result<(), InstallError> {
    let home = std::env::var("HOME").map_err(|_| InstallError::HomeNotSet)?;
    install_hooks_at(Path::new(&home)).await
}

/// Install hooks at a specific home directory path (testable).
async fn install_hooks_at(home: &Path) -> Result<(), InstallError> {
    // Create hook script
    let hooks_dir = home.join(".myremote").join("hooks");
    tokio::fs::create_dir_all(&hooks_dir)
        .await
        .map_err(InstallError::Io)?;

    let script_path = hooks_dir.join("myremote-hook.sh");
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
# MyRemote hook script - forwards Claude Code hook events to the agent sidecar.
# Managed by myremote-agent. Do not edit manually.
PORT=$(cat ~/.myremote/hooks-port 2>/dev/null) || exit 0
INPUT=$(cat -)
RESPONSE=$(curl -s --max-time 60 -X POST "http://127.0.0.1:$PORT/hooks" \
  -H "Content-Type: application/json" \
  -d "$INPUT" 2>/dev/null)
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

    // Build hook configuration
    let hook_entry = serde_json::json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": script
        }]
    });

    // Merge into existing hooks (preserve user's own hooks)
    let hooks = settings
        .as_object_mut()
        .ok_or(InstallError::InvalidSettings)?
        .entry("hooks")
        .or_insert(serde_json::json!({}));

    let hooks_obj = hooks.as_object_mut().ok_or(InstallError::InvalidSettings)?;

    for event in &[
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "PermissionRequest",
        "Notification",
    ] {
        let event_hooks = hooks_obj.entry(*event).or_insert(serde_json::json!([]));

        if let Some(arr) = event_hooks.as_array_mut() {
            // Check if myremote hook is already present
            let already_installed = arr.iter().any(|entry| {
                entry
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

            if !already_installed {
                arr.push(hook_entry.clone());
            }
        }
    }

    // Write back
    let formatted =
        serde_json::to_string_pretty(&settings).map_err(|_| InstallError::InvalidSettings)?;
    tokio::fs::write(&settings_path, formatted)
        .await
        .map_err(InstallError::Io)?;

    tracing::info!(
        path = %settings_path.display(),
        "Claude Code settings updated with myremote hooks"
    );

    Ok(())
}

/// Remove myremote hooks from Claude Code settings.
#[allow(dead_code)]
pub async fn uninstall_hooks() -> Result<(), InstallError> {
    let home = std::env::var("HOME").map_err(|_| InstallError::HomeNotSet)?;
    uninstall_hooks_at(Path::new(&home)).await
}

/// Remove myremote hooks at a specific home directory path (testable).
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
                                    .is_some_and(|c| c.contains("myremote-hook"))
                            })
                        })
                });
            }
        }
    }

    let formatted =
        serde_json::to_string_pretty(&settings).map_err(|_| InstallError::InvalidSettings)?;
    tokio::fs::write(&settings_path, formatted)
        .await
        .map_err(InstallError::Io)?;

    // Remove hook script
    let script_path = home
        .join(".myremote")
        .join("hooks")
        .join("myremote-hook.sh");
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
    fn hook_script_content() {
        let script = generate_hook_script();
        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains("hooks-port"));
        assert!(script.contains("curl"));
        assert!(script.contains("/hooks"));
        assert!(script.contains("exit 0"));
    }

    #[tokio::test]
    async fn install_creates_script_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let result = install_hooks_at(home).await;
        assert!(result.is_ok());

        // Verify script exists
        let script = home.join(".myremote/hooks/myremote-hook.sh");
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
            "PermissionRequest",
            "Notification",
        ] {
            assert!(hooks.contains_key(*event), "missing hook event: {event}");
        }
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

        // Install myremote hooks
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
    async fn uninstall_hooks_removes_myremote_entries() {
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
            "PermissionRequest",
            "Notification",
        ] {
            let arr = settings["hooks"][event].as_array().unwrap();
            assert!(
                arr.is_empty(),
                "hook {event} should be empty after uninstall"
            );
        }

        // Verify script was removed
        let script = home.join(".myremote/hooks/myremote-hook.sh");
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

        // Create settings with both user and myremote hooks
        let claude_dir = home.join(".claude");
        tokio::fs::create_dir_all(&claude_dir).await.unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/my-hook.sh"}]},
                    {"matcher": "", "hooks": [{"type": "command", "command": "/home/user/.myremote/hooks/myremote-hook.sh"}]}
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

        let script = home.join(".myremote/hooks/myremote-hook.sh");
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
        assert!(script.contains("RESPONSE="));
        assert!(script.contains("exit 0"));
        // Verify it reads from hooks-port file
        assert!(script.contains("hooks-port"));
        // Verify it POSTs to the sidecar
        assert!(script.contains("POST"));
        assert!(script.contains("127.0.0.1"));
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
}
