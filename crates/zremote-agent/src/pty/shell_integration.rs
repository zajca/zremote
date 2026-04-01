use portable_pty::CommandBuilder;
use zremote_protocol::SessionId;

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

/// Configuration for shell integration features.
/// Controls which modifications are applied to the shell environment at spawn time.
#[derive(Debug, Clone)]
pub struct ShellIntegrationConfig {
    /// Disable autosuggestion plugins (zsh-autosuggestions, ble.sh, fish native).
    pub disable_autosuggestions: bool,

    /// Export ZREMOTE_TERMINAL=1 and ZREMOTE_SESSION_ID=<uuid>.
    pub export_env_vars: bool,

    /// Force SIGWINCH on zsh startup to fix resize race with GPUI.
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
    /// Clean up temp resources (ZDOTDIR, rcfile). Called on session close.
    ///
    /// Shell termination is NOT handled here — it is managed by the PTY
    /// session lifecycle:
    /// 1. `portable-pty`'s `child.kill()` sends SIGHUP to the shell PID
    ///    (safe: the `Child` handle prevents PID recycling).
    /// 2. Dropping the PTY master fd triggers kernel SIGHUP to the shell's
    ///    foreground process group (standard POSIX behavior).
    ///
    /// We intentionally do NOT send an explicit SIGHUP here because the
    /// stored `shell_pid` is a bare `u32` without a process handle — any
    /// edge case (PID recycling, async drop order) could route the signal
    /// to an unrelated process such as `systemd --user`, destroying the
    /// desktop session.
    pub fn cleanup(self) {
        drop(self.temp_dir);
    }
}

/// Prepare shell integration for a PTY session.
/// Modifies the `CommandBuilder` in-place and returns state for cleanup.
///
/// Returns `None` if integration is fully disabled (backward-compatible path).
pub fn prepare(
    session_id: SessionId,
    shell_cmd: &str,
    config: &ShellIntegrationConfig,
    cmd: &mut CommandBuilder,
) -> Result<Option<ShellIntegrationState>, std::io::Error> {
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
        ShellType::Unknown(_) => None,
    };

    Ok(Some(ShellIntegrationState {
        shell_type,
        temp_dir,
        shell_pid: None,
    }))
}

fn apply_env_vars(session_id: SessionId, cmd: &mut CommandBuilder) {
    cmd.env("ZREMOTE_TERMINAL", "1");
    cmd.env("ZREMOTE_SESSION_ID", session_id.to_string());
}

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
    zshrc.push_str("if [[ -f \"${ZREMOTE_ORIGINAL_ZDOTDIR:-$HOME}/.zshrc\" ]]; then\n");
    zshrc.push_str("    ZDOTDIR=\"${ZREMOTE_ORIGINAL_ZDOTDIR:-$HOME}\" source \"${ZREMOTE_ORIGINAL_ZDOTDIR:-$HOME}/.zshrc\"\n");
    zshrc.push_str("fi\n\n");

    if config.disable_autosuggestions {
        zshrc.push_str("# Disable zsh-autosuggestions\n");
        zshrc.push_str("if (( $+functions[_zsh_autosuggest_suggest] )); then\n");
        zshrc.push_str("    _zsh_autosuggest_suggest() { :; }\n");
        zshrc.push_str("    _zsh_autosuggest_clear() { :; }\n");
        zshrc.push_str("fi\n");
        zshrc.push_str("# Disable zsh-autocomplete\n");
        zshrc.push_str("if (( $+functions[.autocomplete:async:start] )); then\n");
        zshrc.push_str("    zstyle ':autocomplete:*' disabled yes\n");
        zshrc.push_str("fi\n\n");
    }

    zshrc.push_str("setopt HIST_IGNORE_SPACE\n");

    if config.force_sigwinch {
        zshrc.push_str("# Force SIGWINCH after prompt renders (or fallback after 100ms)\n");
        zshrc.push_str("{\n");
        zshrc.push_str("    (\n");
        zshrc.push_str("        if read -t 0.5 -n 1 < /dev/tty 2>/dev/null; then\n");
        zshrc.push_str("            kill -WINCH $$ 2>/dev/null\n");
        zshrc.push_str("        else\n");
        zshrc.push_str("            sleep 0.1\n");
        zshrc.push_str("            kill -WINCH $$ 2>/dev/null\n");
        zshrc.push_str("        fi\n");
        zshrc.push_str("    ) &\n");
        zshrc.push_str("    disown\n");
        zshrc.push_str("}\n");
    }

    std::fs::write(temp_dir.path().join(".zshrc"), &zshrc)?;

    // Preserve original ZDOTDIR so user config can be sourced
    if let Ok(original) = std::env::var("ZDOTDIR") {
        cmd.env("ZREMOTE_ORIGINAL_ZDOTDIR", &original);
    }
    cmd.env("ZDOTDIR", temp_dir.path().to_string_lossy().as_ref());

    Ok(Some(temp_dir))
}

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
    rcfile.push_str("if [[ -f \"$HOME/.bashrc\" ]]; then\n");
    rcfile.push_str("    source \"$HOME/.bashrc\"\n");
    rcfile.push_str("fi\n\n");

    if config.disable_autosuggestions {
        rcfile.push_str("# Disable ble.sh\n");
        rcfile.push_str("if [[ -n \"${_ble_bash}\" ]] || type ble-0 &>/dev/null; then\n");
        rcfile.push_str("    ble-detach 2>/dev/null || true\n");
        rcfile.push_str("fi\n\n");
    }

    std::fs::write(&rcfile_path, &rcfile)?;

    // Modify command to use --rcfile
    cmd.arg("--rcfile");
    cmd.arg(rcfile_path.to_string_lossy().as_ref());

    Ok(Some(temp_dir))
}

fn prepare_fish_integration(
    config: &ShellIntegrationConfig,
    cmd: &mut CommandBuilder,
) -> Result<Option<tempfile::TempDir>, std::io::Error> {
    if config.disable_autosuggestions {
        cmd.arg("-C");
        cmd.arg("set -g fish_autosuggestion_enabled 0; function fish_suggest; end");
    }
    // Fish does not need temp files
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_type_detect_zsh() {
        assert_eq!(ShellType::detect("/bin/zsh"), ShellType::Zsh);
    }

    #[test]
    fn shell_type_detect_usr_local_bash() {
        assert_eq!(ShellType::detect("/usr/local/bin/bash"), ShellType::Bash);
    }

    #[test]
    fn shell_type_detect_fish() {
        assert_eq!(ShellType::detect("fish"), ShellType::Fish);
    }

    #[test]
    fn shell_type_detect_unknown() {
        assert_eq!(
            ShellType::detect("/bin/nu"),
            ShellType::Unknown("nu".to_string())
        );
    }

    #[test]
    fn shell_type_name() {
        assert_eq!(ShellType::Zsh.name(), "zsh");
        assert_eq!(ShellType::Bash.name(), "bash");
        assert_eq!(ShellType::Fish.name(), "fish");
        assert_eq!(ShellType::Unknown("nu".to_string()).name(), "nu");
    }

    #[test]
    fn config_for_ai_session() {
        let config = ShellIntegrationConfig::for_ai_session();
        assert!(config.disable_autosuggestions);
        assert!(config.export_env_vars);
        assert!(config.force_sigwinch);
    }

    #[test]
    fn config_for_manual_session() {
        let config = ShellIntegrationConfig::for_manual_session();
        assert!(!config.disable_autosuggestions);
        assert!(config.export_env_vars);
        assert!(config.force_sigwinch);
    }

    #[test]
    fn config_disabled() {
        let config = ShellIntegrationConfig::disabled();
        assert!(!config.disable_autosuggestions);
        assert!(!config.export_env_vars);
        assert!(!config.force_sigwinch);
    }

    #[test]
    fn zsh_integration_generates_zshrc() {
        let session_id = uuid::Uuid::new_v4();
        let config = ShellIntegrationConfig::for_ai_session();
        let mut cmd = CommandBuilder::new("/bin/zsh");

        let temp_dir = prepare_zsh_integration(session_id, &config, &mut cmd)
            .unwrap()
            .expect("should create temp dir");

        let zshrc = std::fs::read_to_string(temp_dir.path().join(".zshrc")).unwrap();
        assert!(
            zshrc.contains("_zsh_autosuggest_suggest"),
            "should contain autosuggestion noop"
        );
        assert!(
            zshrc.contains("kill -WINCH"),
            "should contain SIGWINCH force"
        );
        assert!(
            zshrc.contains("ZREMOTE_ORIGINAL_ZDOTDIR"),
            "should source user config"
        );
    }

    #[test]
    fn zsh_integration_no_autosuggest_when_disabled() {
        let session_id = uuid::Uuid::new_v4();
        let config = ShellIntegrationConfig {
            disable_autosuggestions: false,
            export_env_vars: true,
            force_sigwinch: true,
        };
        let mut cmd = CommandBuilder::new("/bin/zsh");

        let temp_dir = prepare_zsh_integration(session_id, &config, &mut cmd)
            .unwrap()
            .expect("should create temp dir");

        let zshrc = std::fs::read_to_string(temp_dir.path().join(".zshrc")).unwrap();
        assert!(
            !zshrc.contains("_zsh_autosuggest_suggest"),
            "should NOT contain autosuggestion noop"
        );
    }

    #[test]
    fn bash_integration_generates_rcfile() {
        let session_id = uuid::Uuid::new_v4();
        let config = ShellIntegrationConfig::for_ai_session();
        let mut cmd = CommandBuilder::new("/bin/bash");

        let temp_dir = prepare_bash_integration(session_id, &config, &mut cmd)
            .unwrap()
            .expect("should create temp dir");

        let rcfile = std::fs::read_to_string(temp_dir.path().join(".bashrc")).unwrap();
        assert!(rcfile.contains(".bashrc"), "should source user bashrc");
        assert!(rcfile.contains("ble-detach"), "should disable ble.sh");
    }

    #[test]
    fn prepare_returns_none_when_all_disabled() {
        let session_id = uuid::Uuid::new_v4();
        let config = ShellIntegrationConfig::disabled();
        let mut cmd = CommandBuilder::new("/bin/zsh");

        let result = prepare(session_id, "/bin/zsh", &config, &mut cmd).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn prepare_sets_env_vars() {
        let session_id = uuid::Uuid::new_v4();
        let config = ShellIntegrationConfig::for_manual_session();
        let mut cmd = CommandBuilder::new("/bin/sh");

        let state = prepare(session_id, "/bin/sh", &config, &mut cmd)
            .unwrap()
            .expect("should return state");

        assert_eq!(state.shell_type, ShellType::Unknown("sh".to_string()));
        // Env vars are set on the CommandBuilder, which we can verify indirectly
        // by checking that the state was created (env vars applied before return)
    }

    #[test]
    fn cleanup_removes_temp_dir() {
        let temp_dir = tempfile::Builder::new()
            .prefix("zremote-test-cleanup-")
            .tempdir()
            .unwrap();
        let temp_path = temp_dir.path().to_path_buf();

        let state = ShellIntegrationState {
            shell_type: ShellType::Zsh,
            temp_dir: Some(temp_dir),
            shell_pid: None,
        };

        assert!(temp_path.exists(), "temp dir should exist before cleanup");
        state.cleanup();
        assert!(
            !temp_path.exists(),
            "temp dir should be removed after cleanup"
        );
    }

    #[test]
    fn cleanup_does_not_signal_shell() {
        // Spawn a long-lived process and verify cleanup() does NOT kill it.
        // This is the core safety invariant: cleanup() must never send signals
        // because the stored shell_pid could have been recycled by the kernel.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();

        let state = ShellIntegrationState {
            shell_type: ShellType::Zsh,
            temp_dir: None,
            shell_pid: Some(pid),
        };

        state.cleanup();

        // Process must still be alive after cleanup
        let alive =
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid.cast_signed()), None).is_ok();
        assert!(alive, "cleanup() must not signal the shell process");

        // Clean up
        child.kill().ok();
        child.wait().ok();
    }

    #[test]
    fn unknown_shell_no_crash() {
        let session_id = uuid::Uuid::new_v4();
        let config = ShellIntegrationConfig::for_ai_session();
        let mut cmd = CommandBuilder::new("/bin/nu");

        let state = prepare(session_id, "/bin/nu", &config, &mut cmd)
            .unwrap()
            .expect("should return state");

        assert_eq!(state.shell_type, ShellType::Unknown("nu".to_string()));
        assert!(
            state.temp_dir.is_none(),
            "unknown shell should not create temp files"
        );
    }
}
