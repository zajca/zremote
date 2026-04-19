//! Shell path resolution for PTY sessions.
//!
//! Per-project settings can specify a `shell` field, but that value may be a
//! bare command name (`zsh`) or an absolute path that is valid on the author's
//! machine but missing on another (e.g. `/bin/zsh` on NixOS where the real path
//! is `/run/current-system/sw/bin/zsh`). Spawning a PTY daemon with a
//! non-existent shell makes the daemon exec-fail before it can write its state
//! file, which surfaces to the caller as a 500 "timeout waiting for daemon
//! state file". [`resolve_shell`] normalizes these inputs against the real
//! filesystem and `$PATH`, falling back to the user's login shell when the
//! requested value is missing — so user settings stay portable.

use std::path::Path;
use std::sync::OnceLock;

/// Read the current user's login shell from the passwd database.
fn login_shell_from_passwd() -> Option<String> {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    let output = std::process::Command::new("getent")
        .args(["passwd", uid.trim()])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    // passwd format: name:password:uid:gid:gecos:home:shell
    let shell = output.trim().rsplit(':').next()?;
    if shell.is_empty() {
        return None;
    }
    Some(shell.to_string())
}

/// Resolve the default shell from the passwd database, falling back to `$SHELL`
/// and then `/bin/sh`. Cached after first call.
///
/// Reads from passwd rather than `$SHELL` because `$SHELL` can be overridden by
/// `nix develop` to a non-interactive bash (without readline), which breaks PS1
/// escape processing in PTY sessions. The passwd entry is the user's actual
/// login shell.
pub fn default_shell() -> &'static str {
    static SHELL: OnceLock<String> = OnceLock::new();
    SHELL.get_or_init(|| {
        login_shell_from_passwd()
            .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()))
    })
}

/// Find `name` in `$PATH`. Returns the first hit that is a regular file.
fn which(name: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// Resolve a shell specification into an absolute path that exists.
///
/// Inputs:
/// - `None` or empty/whitespace → the user's login shell from passwd.
/// - An absolute path that exists → used as-is.
/// - An absolute path that does not exist → look up its basename in `$PATH`;
///   if found, use the PATH resolution. Otherwise fall back to the login shell.
/// - A bare name (no `/`) → look up in `$PATH`; fall back to the login shell.
///
/// A warning is logged whenever the requested shell is rewritten or replaced,
/// so users can fix stale `.zremote/settings.json` entries.
pub fn resolve_shell(requested: Option<&str>) -> String {
    let Some(req) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
        return default_shell().to_string();
    };

    if req.starts_with('/') {
        if Path::new(req).is_file() {
            return req.to_string();
        }
        if let Some(basename) = Path::new(req).file_name().and_then(|s| s.to_str())
            && let Some(found) = which(basename)
        {
            tracing::warn!(
                requested = req,
                resolved = %found,
                "requested shell path does not exist; resolved via PATH basename"
            );
            return found;
        }
        tracing::warn!(
            requested = req,
            fallback = default_shell(),
            "requested shell path does not exist and basename not in PATH; using login shell"
        );
        return default_shell().to_string();
    }

    if let Some(found) = which(req) {
        return found;
    }
    tracing::warn!(
        requested = req,
        fallback = default_shell(),
        "requested shell not found in PATH; using login shell"
    );
    default_shell().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_returns_non_empty() {
        assert!(!default_shell().is_empty());
    }

    #[test]
    fn resolve_none_returns_default() {
        assert_eq!(resolve_shell(None), default_shell());
    }

    #[test]
    fn resolve_empty_treated_as_none() {
        assert_eq!(resolve_shell(Some("")), default_shell());
        assert_eq!(resolve_shell(Some("   ")), default_shell());
    }

    #[test]
    fn resolve_existing_absolute_returned_as_is() {
        // /bin/sh exists on every POSIX system including NixOS (via /bin/sh symlink)
        let r = resolve_shell(Some("/bin/sh"));
        assert_eq!(r, "/bin/sh");
    }

    #[test]
    fn resolve_missing_absolute_falls_back_via_basename() {
        // Classic NixOS scenario: settings.json from another host says `/bin/zsh`
        // which does not exist, but `sh` is in PATH.
        let r = resolve_shell(Some("/bogus/path/does/not/exist/sh"));
        // Either resolved via PATH or fell back to default_shell — both are acceptable
        // and both are valid shells on this system.
        assert!(Path::new(&r).is_file() || r == default_shell());
    }

    #[test]
    fn resolve_bare_name_resolves_via_path() {
        // `sh` is in PATH on every POSIX system
        let r = resolve_shell(Some("sh"));
        assert!(Path::new(&r).is_file(), "resolved path {r} should exist");
        assert!(r.ends_with("/sh"));
    }

    #[test]
    fn resolve_unknown_bare_name_falls_back_to_default() {
        let r = resolve_shell(Some("definitely-not-a-real-shell-xyz-123"));
        assert_eq!(r, default_shell());
    }

    #[test]
    fn resolve_missing_absolute_with_no_basename_match_falls_back() {
        let r = resolve_shell(Some("/bogus/xyz-no-such-shell-123"));
        assert_eq!(r, default_shell());
    }
}
