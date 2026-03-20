//! Workspace persistence: save/restore GUI state across restarts.
//!
//! State file: `~/.config/zremote/gui-state.json`
//!
//! Safety layers (inspired by Okena pattern):
//! 1. Fallback guard: parse failure returns Default, never crash
//! 2. Empty check: don't save fully default state
//! 3. Rolling backup: rename existing to .bak before write
//! 4. Atomic write: write .tmp -> fsync -> rename

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Current format version.
const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GuiState {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub server_url: Option<String>,
    #[serde(default)]
    pub active_session_id: Option<String>,
    #[serde(default)]
    pub window_width: Option<f32>,
    #[serde(default)]
    pub window_height: Option<f32>,
}

impl GuiState {
    fn is_default(&self) -> bool {
        self.server_url.is_none()
            && self.active_session_id.is_none()
            && self.window_width.is_none()
            && self.window_height.is_none()
    }
}

pub struct Persistence {
    path: PathBuf,
    state: GuiState,
    data_version: u64,
    last_saved_version: u64,
}

impl Persistence {
    /// Load state from disk. Returns default state on any error.
    pub fn load() -> Self {
        let path = state_file_path();

        let state = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str::<GuiState>(&contents) {
                    Ok(state) => state,
                    Err(e) => {
                        tracing::warn!(error = %e, path = %path.display(), "failed to parse GUI state, using defaults");
                        GuiState::default()
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "failed to read GUI state file");
                    GuiState::default()
                }
            }
        } else {
            GuiState::default()
        };

        Self {
            path,
            state,
            data_version: 0,
            last_saved_version: 0,
        }
    }

    pub fn state(&self) -> &GuiState {
        &self.state
    }

    /// Mutate state and bump the data version.
    pub fn update(&mut self, f: impl FnOnce(&mut GuiState)) {
        f(&mut self.state);
        self.data_version += 1;
    }

    /// Save to disk if state has changed since last save.
    /// Returns Ok(true) if saved, Ok(false) if skipped.
    pub fn save_if_changed(&mut self) -> std::io::Result<bool> {
        if self.data_version == self.last_saved_version {
            return Ok(false);
        }

        // Don't save fully default state (nothing meaningful to persist).
        if self.state.is_default() {
            return Ok(false);
        }

        self.state.version = FORMAT_VERSION;
        self.atomic_write()?;
        self.last_saved_version = self.data_version;
        Ok(true)
    }

    /// Atomic write with rolling backup.
    fn atomic_write(&self) -> std::io::Result<()> {
        // Ensure parent directory exists.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Rolling backup: rename existing to .bak
        if self.path.exists() {
            let bak = self.path.with_extension("json.bak");
            let _ = std::fs::rename(&self.path, &bak);
        }

        // Write to .tmp first.
        let tmp = self.path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(&self.state)
            .map_err(std::io::Error::other)?;

        {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }

        // Atomic rename.
        std::fs::rename(&tmp, &self.path)?;

        tracing::debug!(path = %self.path.display(), "saved GUI state");
        Ok(())
    }
}

fn state_file_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("zremote")
        .join("gui-state.json")
}
