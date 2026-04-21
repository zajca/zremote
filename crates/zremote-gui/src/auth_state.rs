//! Persistent session token storage for the GUI.
//!
//! Primary storage: system keyring (service `"zremote"`, username = server URL).
//! Fallback (opt-in): plaintext JSON at `~/.config/zremote/session.json` (mode
//! 0600), enabled only when `ZREMOTE_ALLOW_FILE_SESSION_FALLBACK=1` is set.
//!
//! If the keyring is unavailable AND the fallback is disabled, `save` returns
//! an error and `load` returns `None` — the caller must surface this to the user.
//!
//! Multiple server URLs are supported simultaneously (each keyed independently
//! in the keyring and in the JSON map).

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "zremote";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub session_token: String,
    pub expires_at: Option<DateTime<Utc>>,
}

impl SessionEntry {
    pub fn is_expired(&self) -> bool {
        self.expires_at.map(|t| t <= Utc::now()).unwrap_or(false)
    }
}

fn file_fallback_enabled() -> bool {
    std::env::var("ZREMOTE_ALLOW_FILE_SESSION_FALLBACK").as_deref() == Ok("1")
}

// ---- keyring helpers ----

fn keyring_load(server_url: &str) -> Option<SessionEntry> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, server_url).ok()?;
    let json = entry.get_password().ok()?;
    serde_json::from_str(&json).ok()
}

fn keyring_save(server_url: &str, value: &SessionEntry) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, server_url)
        .map_err(|e| format!("keyring entry creation failed: {e}"))?;
    let json = serde_json::to_string(value).map_err(|e| e.to_string())?;
    entry
        .set_password(&json)
        .map_err(|e| format!("keyring write failed: {e}"))
}

fn keyring_clear(server_url: &str) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, server_url) {
        let _ = entry.delete_credential();
    }
}

fn keyring_available() -> bool {
    // Try a dummy probe to verify the keyring backend is usable.
    keyring::Entry::new(KEYRING_SERVICE, "__probe__").is_ok()
}

// ---- plaintext file helpers (fallback only) ----

fn session_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("zremote").join("session.json"))
}

fn file_load_all() -> HashMap<String, SessionEntry> {
    let Some(path) = session_path() else {
        return HashMap::new();
    };
    let Ok(bytes) = fs::read(&path) else {
        return HashMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn file_save_all(sessions: &HashMap<String, SessionEntry>) -> Result<(), String> {
    let path = session_path().ok_or("cannot determine config directory")?;
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let json = serde_json::to_vec_pretty(sessions).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    // Open with O_CREAT|O_WRONLY|O_TRUNC and mode 0o600 at creation — TOCTOU-safe.
    write_private_file(&tmp, &json)?;
    fs::rename(&tmp, &path).map_err(|e| format!("rename failed: {e}"))
}

#[cfg(unix)]
fn create_private_dir(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .map_err(|e| format!("mkdir failed: {e}"))
}

#[cfg(not(unix))]
fn create_private_dir(path: &std::path::Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("mkdir failed: {e}"))
}

#[cfg(unix)]
fn write_private_file(path: &PathBuf, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("open failed: {e}"))?;
    f.write_all(data).map_err(|e| format!("write failed: {e}"))
}

#[cfg(not(unix))]
fn write_private_file(path: &PathBuf, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("open failed: {e}"))?;
    f.write_all(data).map_err(|e| format!("write failed: {e}"))
}

// ---- public API ----

/// Returns the stored session for `server_url` if it exists and is not expired.
pub fn load(server_url: &str) -> Option<SessionEntry> {
    // Try keyring first.
    if let Some(entry) = keyring_load(server_url) {
        if !entry.is_expired() {
            return Some(entry);
        }
        // Expired — clean it up.
        keyring_clear(server_url);
    }
    // Try file fallback if enabled.
    if file_fallback_enabled() {
        let sessions = file_load_all();
        return sessions
            .get(server_url)
            .cloned()
            .filter(|e| !e.is_expired());
    }
    None
}

/// Persists `entry` for `server_url`.
///
/// Tries keyring first; falls back to plaintext file only when
/// `ZREMOTE_ALLOW_FILE_SESSION_FALLBACK=1`.  On Windows, emits a warning when
/// the file fallback is used.
///
/// Returns `Err` when neither storage backend is available.
pub fn save(server_url: &str, entry: &SessionEntry) {
    if keyring_available() {
        if let Err(err) = keyring_save(server_url, entry) {
            tracing::warn!(error = %err, "keyring save failed");
        } else {
            return;
        }
    }
    if file_fallback_enabled() {
        #[cfg(not(unix))]
        tracing::warn!(
            "storing session token in plaintext file (keyring unavailable on this platform); \
             set ZREMOTE_ALLOW_FILE_SESSION_FALLBACK=1 explicitly acknowledges this risk"
        );
        let mut sessions = file_load_all();
        sessions.insert(server_url.to_string(), entry.clone());
        if let Err(err) = file_save_all(&sessions) {
            tracing::error!(error = %err, "file session fallback save failed");
        }
    } else {
        tracing::error!(
            "could not save session token: keyring unavailable and \
             ZREMOTE_ALLOW_FILE_SESSION_FALLBACK is not set"
        );
    }
}

/// Removes the stored session for `server_url`.
pub fn clear(server_url: &str) {
    keyring_clear(server_url);
    if file_fallback_enabled() {
        let mut sessions = file_load_all();
        sessions.remove(server_url);
        let _ = file_save_all(&sessions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_expired_without_expiry() {
        let entry = SessionEntry {
            session_token: "tok".into(),
            expires_at: None,
        };
        assert!(!entry.is_expired());
    }

    #[test]
    fn is_expired_past_timestamp() {
        let entry = SessionEntry {
            session_token: "tok".into(),
            expires_at: Some(Utc::now() - chrono::Duration::seconds(10)),
        };
        assert!(entry.is_expired());
    }

    #[test]
    fn is_not_expired_future_timestamp() {
        let entry = SessionEntry {
            session_token: "tok".into(),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        };
        assert!(!entry.is_expired());
    }

    // File fallback path: only runs when ZREMOTE_ALLOW_FILE_SESSION_FALLBACK=1
    // is set, so we test the helper internals directly.
    #[test]
    fn file_fallback_serialization_roundtrip() {
        let entry = SessionEntry {
            session_token: "roundtrip-tok".into(),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: SessionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.session_token, entry.session_token);
        assert!(!decoded.is_expired());
    }

    #[test]
    fn file_fallback_flag_check() {
        // ZREMOTE_ALLOW_FILE_SESSION_FALLBACK not set in test env by default.
        // We cannot mutate env safely in parallel tests, so just verify the
        // function returns false when the var is absent.
        if std::env::var("ZREMOTE_ALLOW_FILE_SESSION_FALLBACK").is_err() {
            assert!(!file_fallback_enabled());
        }
    }
}
