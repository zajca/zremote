//! Persistent session token storage for the GUI.
//!
//! Sessions are stored in `~/.config/zremote/session.json` (mode 0600),
//! keyed by server URL so multiple servers are supported simultaneously.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

fn session_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("zremote").join("session.json"))
}

fn load_all() -> HashMap<String, SessionEntry> {
    let Some(path) = session_path() else {
        return HashMap::new();
    };
    let Ok(bytes) = fs::read(&path) else {
        return HashMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_all(sessions: &HashMap<String, SessionEntry>) {
    let Some(path) = session_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_vec_pretty(sessions) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, &json).is_ok() {
        let _ = set_file_permissions(&tmp);
        let _ = fs::rename(&tmp, &path);
    }
}

#[cfg(unix)]
fn set_file_permissions(path: &PathBuf) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &PathBuf) -> std::io::Result<()> {
    Ok(())
}

/// Returns the stored session for `server_url` if it exists and is not expired.
pub fn load(server_url: &str) -> Option<SessionEntry> {
    let sessions = load_all();
    sessions
        .get(server_url)
        .cloned()
        .filter(|e| !e.is_expired())
}

/// Persists `entry` for `server_url`.
pub fn save(server_url: &str, entry: &SessionEntry) {
    let mut sessions = load_all();
    sessions.insert(server_url.to_string(), entry.clone());
    save_all(&sessions);
}

/// Removes the stored session for `server_url`.
pub fn clear(server_url: &str) {
    let mut sessions = load_all();
    sessions.remove(server_url);
    save_all(&sessions);
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
}
