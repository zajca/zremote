//! Local-mode helpers for the GUI (RFC auth-overhaul Phase 6).
//!
//! In local mode the GUI and the agent run on the same machine. The agent
//! generates a per-install bearer token at `~/.zremote/local.token` on first
//! run; the GUI reads it and attaches it as `Authorization: Bearer` on every
//! REST request (plus `?token=` on WebSocket upgrades).
//!
//! Returns `None` if the file is missing or unreadable. Callers must handle
//! that as a fatal error — without the token the agent will reject every
//! request with 401.

use std::fs;
use std::path::PathBuf;

/// Path to the local-mode token file. Mirrors
/// `zremote_agent::local::token::token_path`.
pub(crate) fn token_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".zremote").join("local.token"))
}

/// Human-readable rendering of `token_path()`, used by the local-mode
/// bootstrap error panel. Falls back to the literal `~/.zremote/local.token`
/// when the home directory isn't resolvable.
#[must_use]
pub fn token_path_display() -> String {
    token_path().map_or_else(
        || "~/.zremote/local.token".to_string(),
        |p| p.display().to_string(),
    )
}

/// Read the agent's local-mode bearer token from `~/.zremote/local.token`.
///
/// Returns `None` when the file is missing or empty. Whitespace and trailing
/// newlines are trimmed.
#[must_use]
pub fn read_local_token() -> Option<String> {
    let path = token_path()?;
    match fs::read_to_string(&path) {
        Ok(s) => {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                tracing::warn!(
                    path = %path.display(),
                    "local.token exists but is empty — agent may not have initialised yet"
                );
                None
            } else {
                Some(trimmed)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                path = %path.display(),
                "local.token missing — run `zremote agent local` once to generate it"
            );
            None
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                path = %path.display(),
                "failed to read local.token"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_path_resolves_under_home() {
        if let Some(path) = token_path() {
            assert!(path.ends_with(".zremote/local.token"));
        }
    }
}
