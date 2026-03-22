use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;
use zremote_protocol::SessionId;

use crate::session::PtyOutput;

const TMUX_SOCKET: &str = "zremote";
const FIFO_DIR_PREFIX: &str = "/tmp/zremote-tmux";
const STALE_SESSION_HOURS: u64 = 24;

/// Session name prefix used for all zremote-managed tmux sessions.
const SESSION_PREFIX: &str = "zremote-";

/// Information about a single tmux pane.
#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub pane_id: String,
    pub pid: u32,
    pub index: u16,
    pub is_active: bool,
}

/// A change detected in the set of panes within a tmux session.
#[derive(Debug)]
pub enum PaneChange {
    Added(PaneInfo),
    Removed(String), // pane_id
}

struct ExtraPaneHandle {
    pane_id: String,
    fifo_path: PathBuf,
    reader_handle: JoinHandle<()>,
}

pub struct TmuxSession {
    session_id: SessionId,
    tmux_name: String,
    pane_id: String,
    fifo_path: PathBuf,
    reader_handle: JoinHandle<()>,
    pid: u32,
    output_tx: mpsc::Sender<PtyOutput>,
    extra_panes: Vec<ExtraPaneHandle>,
    known_pane_ids: HashSet<String>,
}

/// Create a `Command` pre-configured with `tmux -L zremote`.
fn tmux_cmd() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(["-L", TMUX_SOCKET]);
    cmd
}

/// Return the per-UID FIFO directory path: `/tmp/zremote-tmux-{uid}/`.
pub(crate) fn fifo_dir() -> PathBuf {
    let uid = current_uid();
    PathBuf::from(format!("{FIFO_DIR_PREFIX}-{uid}"))
}

/// Build the tmux session name for a given session ID.
fn tmux_session_name(session_id: SessionId) -> String {
    format!("{SESSION_PREFIX}{session_id}")
}

/// Get the current user's UID by running `id -u`.
fn current_uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "0".to_owned())
}

impl TmuxSession {
    /// Spawn a new tmux-backed terminal session. Returns `(session, pid)`.
    ///
    /// `output_tx` receives terminal output as `(SessionId, Vec<u8>)`.
    /// When the FIFO reader encounters EOF or an error, it sends a zero-length
    /// vec to signal that the session has ended.
    pub fn spawn(
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
        env: Option<&std::collections::HashMap<String, String>>,
        output_tx: mpsc::Sender<PtyOutput>,
    ) -> Result<(Self, u32), Box<dyn std::error::Error + Send + Sync>> {
        let tmux_name = tmux_session_name(session_id);
        let dir = fifo_dir();

        // Ensure FIFO directory exists
        fs::create_dir_all(&dir)?;

        // Set environment variables on the tmux server BEFORE creating the
        // session so the spawned shell inherits them.
        if let Some(env_vars) = env {
            for (key, value) in env_vars {
                let _ = tmux_cmd()
                    .args(["set-environment", "-g", key, value])
                    .status();
            }
        }

        // Create tmux session
        let mut cmd = tmux_cmd();
        cmd.args([
            "new-session",
            "-d",
            "-s",
            &tmux_name,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
        ]);
        if let Some(wd) = working_dir {
            cmd.args(["-c", wd]);
        }
        cmd.arg(shell);

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tmux new-session failed: {stderr}").into());
        }

        tracing::info!(session_id = %session_id, tmux_name = %tmux_name, "tmux session created");

        // Clean up global env vars after session creation to avoid leaking
        // into other sessions.
        if let Some(env_vars) = env {
            for key in env_vars.keys() {
                let _ = tmux_cmd().args(["set-environment", "-gu", key]).status();
            }
        }

        // Capture the stable pane ID (%N) before anything can split the window
        let pane_id = get_pane_id(&tmux_name)?;

        // Get shell PID using the stable pane_id
        let pid = get_pane_pid(&pane_id)?;

        // Create FIFO
        let fifo_path = dir.join(format!("{session_id}.fifo"));
        create_fifo(&fifo_path)?;

        // Set up pipe-pane targeting the stable pane_id
        setup_pipe_pane(&pane_id, &fifo_path)?;

        // Spawn async reader task on FIFO
        let reader_handle =
            spawn_fifo_reader(session_id, None, fifo_path.clone(), output_tx.clone());

        tracing::info!(session_id = %session_id, pane_id = %pane_id, "pane ID captured");

        let mut known_pane_ids = HashSet::new();
        known_pane_ids.insert(pane_id.clone());

        let session = Self {
            session_id,
            tmux_name,
            pane_id,
            fifo_path,
            reader_handle,
            pid,
            output_tx,
            extra_panes: Vec::new(),
            known_pane_ids,
        };

        Ok((session, pid))
    }

    /// Reattach to an existing tmux session. Used for session recovery after
    /// agent restart or reconnection.
    pub fn reattach(
        session_id: SessionId,
        output_tx: mpsc::Sender<PtyOutput>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let tmux_name = tmux_session_name(session_id);

        // Verify the tmux session exists
        if !tmux_session_exists(&tmux_name) {
            return Err(format!("tmux session {tmux_name} does not exist").into());
        }

        tracing::info!(session_id = %session_id, tmux_name = %tmux_name, "reattaching to tmux session");

        // Resolve the first pane's stable %id directly. We cannot hard-code
        // `:0.0` because tmux may renumber both windows and panes (e.g., the
        // original window/pane 0 was closed and only index 1+ remains).
        let reattach_target = {
            let out = tmux_cmd()
                .args(["list-panes", "-t", &tmux_name, "-F", "#{pane_id}"])
                .output()
                .map_err(|e| format!("failed to list panes for {tmux_name}: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "tmux list-panes failed for {tmux_name}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )
                .into());
            }
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .ok_or_else(|| format!("no panes found in {tmux_name}"))?
                .to_string()
        };
        let pane_id = reattach_target;

        // Get shell PID using the stable pane_id
        let pid = get_pane_pid(&pane_id)?;

        // Create/recreate FIFO
        let dir = fifo_dir();
        fs::create_dir_all(&dir)?;
        let fifo_path = dir.join(format!("{session_id}.fifo"));

        // Remove stale FIFO if it exists, then create fresh
        let _ = fs::remove_file(&fifo_path);
        create_fifo(&fifo_path)?;

        // Capture the current visible pane content BEFORE setting up pipe-pane.
        // This avoids a race where live tmux output (with cursor positioning
        // sequences) arrives through the FIFO before the capture data, causing
        // conflicting screen content and duplicate cursors in the browser.
        // Send through output_tx so it flows through the normal PTY output loop
        // (scrollback + browser senders).
        if let Ok(cap) = tmux_cmd()
            .args(["capture-pane", "-t", &pane_id, "-p", "-e"])
            .output()
            && cap.status.success()
            && !cap.stdout.is_empty()
        {
            let _ = output_tx.try_send(PtyOutput {
                session_id,
                pane_id: None,
                data: cap.stdout,
            });
        }

        // Stop any existing pipe-pane, then set up fresh targeting the stable pane_id
        let _ = tmux_cmd().args(["pipe-pane", "-t", &pane_id]).output();
        setup_pipe_pane(&pane_id, &fifo_path)?;

        // Spawn async reader task on FIFO (only new output goes through channel)
        let reader_handle =
            spawn_fifo_reader(session_id, None, fifo_path.clone(), output_tx.clone());

        tracing::info!(session_id = %session_id, pane_id = %pane_id, "pane ID captured on reattach");

        let mut known_pane_ids = HashSet::new();
        known_pane_ids.insert(pane_id.clone());

        Ok(Self {
            session_id,
            tmux_name,
            pane_id,
            fifo_path,
            reader_handle,
            pid,
            output_tx,
            extra_panes: Vec::new(),
            known_pane_ids,
        })
    }

    /// Return the PID of the shell process inside the tmux pane.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Return the session ID.
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Return the stable pane ID (`%N` format).
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    /// Return the tmux session name (e.g. "zremote-{uuid}").
    pub fn tmux_name(&self) -> &str {
        &self.tmux_name
    }

    /// Re-establish FIFO reader and pipe-pane after GUI releases direct connection.
    ///
    /// This is called when the GUI disconnects from a direct tmux session and the
    /// agent needs to resume capturing output. It follows the same pattern as
    /// `reattach()` but operates on an existing `TmuxSession` instance.
    pub fn reattach_reader(
        &mut self,
        output_tx: mpsc::Sender<PtyOutput>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Abort old reader if still running
        self.reader_handle.abort();

        // Recreate FIFO at original path
        let dir = fifo_dir();
        fs::create_dir_all(&dir)?;
        let fifo_path = dir.join(format!("{}.fifo", self.session_id));
        let _ = fs::remove_file(&fifo_path);
        create_fifo(&fifo_path)?;

        // Capture current screen content to bridge any gap
        if let Ok(cap) = tmux_cmd()
            .args(["capture-pane", "-t", &self.pane_id, "-p", "-e"])
            .output()
            && cap.status.success()
            && !cap.stdout.is_empty()
        {
            let _ = output_tx.try_send(PtyOutput {
                session_id: self.session_id,
                pane_id: None,
                data: cap.stdout,
            });
        }

        // Stop any existing pipe-pane, then set up fresh
        let _ = tmux_cmd().args(["pipe-pane", "-t", &self.pane_id]).output();
        setup_pipe_pane(&self.pane_id, &fifo_path)?;

        // Spawn new FIFO reader task
        let reader_handle =
            spawn_fifo_reader(self.session_id, None, fifo_path.clone(), output_tx.clone());

        // Update internal state
        self.fifo_path = fifo_path;
        self.reader_handle = reader_handle;
        self.output_tx = output_tx;

        tracing::info!(
            session_id = %self.session_id,
            pane_id = %self.pane_id,
            "re-attached reader after direct connection released"
        );

        Ok(())
    }

    /// Send raw bytes as input to the tmux pane via `send-keys -H` (hex mode).
    ///
    /// Writing directly to the PTY slave device injects data into the output
    /// stream (it appears on screen but the shell never receives it as input).
    /// `send-keys -H` feeds bytes through the PTY master, which is the correct
    /// input path: line discipline processes them, shell reads them.
    pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        let mut cmd = tmux_cmd();
        cmd.args(["send-keys", "-t", &self.pane_id, "-H"]);
        for byte in data {
            cmd.arg(format!("{byte:02x}"));
        }

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(std::io::Error::other(format!(
                "tmux send-keys failed: {stderr}"
            )));
        }
        Ok(())
    }

    /// Resize the tmux window (and all its panes).
    ///
    /// Uses `resize-window` instead of `resize-pane` because detached sessions
    /// (created with `new-session -d`) keep their initial window size, and
    /// `resize-pane` is silently capped by the window dimensions.
    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let output = tmux_cmd()
            .args([
                "resize-window",
                "-t",
                &self.tmux_name,
                "-x",
                &cols.to_string(),
                "-y",
                &rows.to_string(),
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tmux resize-window failed: {stderr}").into());
        }

        Ok(())
    }

    /// List all panes in this session.
    pub fn list_panes(&self) -> Vec<PaneInfo> {
        let output = tmux_cmd()
            .args([
                "list-panes",
                "-t",
                &self.tmux_name,
                "-F",
                "#{pane_id}:#{pane_pid}:#{pane_index}:#{pane_active}",
            ])
            .output();

        let Ok(output) = output else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.trim().splitn(4, ':').collect();
                if parts.len() < 4 {
                    return None;
                }
                Some(PaneInfo {
                    pane_id: parts[0].to_owned(),
                    pid: parts[1].parse().unwrap_or(0),
                    index: parts[2].parse().unwrap_or(0),
                    is_active: parts[3] == "1",
                })
            })
            .collect()
    }

    /// Detect pane changes, setup/teardown extra pane I/O. Returns changes.
    pub fn sync_panes(&mut self) -> Vec<PaneChange> {
        let current_panes = self.list_panes();
        let current_ids: HashSet<String> =
            current_panes.iter().map(|p| p.pane_id.clone()).collect();

        let mut changes = Vec::new();

        // Detect new panes (not the main pane)
        for pane in &current_panes {
            if pane.pane_id == self.pane_id {
                continue; // Skip the main pane
            }
            if !self.known_pane_ids.contains(&pane.pane_id) {
                // New pane detected -- set up FIFO + reader
                let stripped = pane.pane_id.trim_start_matches('%');
                let fifo_path = fifo_dir().join(format!("{}-{stripped}.fifo", self.session_id));

                if create_fifo(&fifo_path).is_err() {
                    tracing::warn!(pane_id = %pane.pane_id, "failed to create FIFO for extra pane");
                    continue;
                }

                if setup_pipe_pane(&pane.pane_id, &fifo_path).is_err() {
                    tracing::warn!(pane_id = %pane.pane_id, "failed to set up pipe-pane for extra pane");
                    let _ = fs::remove_file(&fifo_path);
                    continue;
                }

                let reader_handle = spawn_fifo_reader(
                    self.session_id,
                    Some(pane.pane_id.clone()),
                    fifo_path.clone(),
                    self.output_tx.clone(),
                );

                self.extra_panes.push(ExtraPaneHandle {
                    pane_id: pane.pane_id.clone(),
                    fifo_path,
                    reader_handle,
                });
                self.known_pane_ids.insert(pane.pane_id.clone());

                tracing::info!(
                    session_id = %self.session_id,
                    pane_id = %pane.pane_id,
                    index = pane.index,
                    "extra pane detected"
                );

                changes.push(PaneChange::Added(pane.clone()));
            }
        }

        // Detect removed panes
        let removed_ids: Vec<String> = self
            .known_pane_ids
            .iter()
            .filter(|id| *id != &self.pane_id && !current_ids.contains(*id))
            .cloned()
            .collect();

        for removed_id in removed_ids {
            // Clean up the extra pane handle
            if let Some(pos) = self
                .extra_panes
                .iter()
                .position(|h| h.pane_id == removed_id)
            {
                let handle = self.extra_panes.remove(pos);
                handle.reader_handle.abort();
                let _ = fs::remove_file(&handle.fifo_path);
            }
            self.known_pane_ids.remove(&removed_id);

            tracing::info!(
                session_id = %self.session_id,
                pane_id = %removed_id,
                "extra pane removed"
            );

            changes.push(PaneChange::Removed(removed_id));
        }

        changes
    }

    /// Write to a specific pane (main or extra).
    pub fn write_to_pane(&mut self, pane_id: &str, data: &[u8]) -> std::io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        let mut cmd = tmux_cmd();
        cmd.args(["send-keys", "-t", pane_id, "-H"]);
        for byte in data {
            cmd.arg(format!("{byte:02x}"));
        }

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(std::io::Error::other(format!(
                "tmux send-keys to pane {pane_id} failed: {stderr}"
            )));
        }
        Ok(())
    }

    /// Resize a specific pane.
    pub fn resize_pane(
        &self,
        pane_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let output = tmux_cmd()
            .args([
                "resize-pane",
                "-t",
                pane_id,
                "-x",
                &cols.to_string(),
                "-y",
                &rows.to_string(),
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tmux resize-pane for {pane_id} failed: {stderr}").into());
        }

        Ok(())
    }

    /// Kill the tmux session and clean up the FIFO.
    pub fn kill(&mut self) {
        // Clean up extra pane handles first
        for handle in self.extra_panes.drain(..) {
            handle.reader_handle.abort();
            let _ = fs::remove_file(&handle.fifo_path);
        }

        let output = tmux_cmd()
            .args(["kill-session", "-t", &self.tmux_name])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                tracing::info!(tmux_name = %self.tmux_name, "tmux session killed");
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!(tmux_name = %self.tmux_name, stderr = %stderr, "tmux kill-session failed");
            }
            Err(e) => {
                tracing::error!(tmux_name = %self.tmux_name, error = %e, "failed to run tmux kill-session");
            }
        }

        self.reader_handle.abort();
        let _ = fs::remove_file(&self.fifo_path);
    }

    /// Check if the tmux session still exists. Returns `Some(0)` if the session
    /// has ended, `None` if it is still running.
    pub fn try_wait(&mut self) -> Option<i32> {
        if tmux_session_exists(&self.tmux_name) {
            None
        } else {
            Some(0)
        }
    }

    /// Detach the reader and close the FIFO without killing the tmux session.
    /// Used during graceful agent shutdown so that tmux sessions persist for
    /// later reattachment.
    pub fn detach(&mut self) {
        // Clean up extra pane handles
        for handle in self.extra_panes.drain(..) {
            handle.reader_handle.abort();
            let _ = tmux_cmd()
                .args(["pipe-pane", "-t", &handle.pane_id])
                .output();
            let _ = fs::remove_file(&handle.fifo_path);
        }

        self.reader_handle.abort();

        // Stop pipe-pane targeting the stable pane_id
        let _ = tmux_cmd().args(["pipe-pane", "-t", &self.pane_id]).output();

        // Remove the FIFO file
        let _ = fs::remove_file(&self.fifo_path);

        tracing::info!(
            session_id = %self.session_id,
            tmux_name = %self.tmux_name,
            pane_id = %self.pane_id,
            "detached from tmux session (session remains alive)"
        );
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        // Clean up extra pane handles
        for handle in self.extra_panes.drain(..) {
            handle.reader_handle.abort();
            let _ = fs::remove_file(&handle.fifo_path);
            let _ = tmux_cmd()
                .args(["pipe-pane", "-t", &handle.pane_id])
                .output();
        }
        // Only detach, do NOT kill the tmux session
        self.reader_handle.abort();
        // Remove FIFO
        let _ = fs::remove_file(&self.fifo_path);
        // Stop pipe-pane targeting the stable pane_id
        let _ = tmux_cmd().args(["pipe-pane", "-t", &self.pane_id]).output();
    }
}

/// Discover all existing zremote tmux sessions and reattach to them.
/// Sessions that fail to reattach are logged and skipped.
pub fn discover_sessions(output_tx: mpsc::Sender<PtyOutput>) -> Vec<TmuxSession> {
    let names = match list_zremote_sessions() {
        Ok(names) => names,
        Err(e) => {
            tracing::warn!(error = %e, "failed to list tmux sessions for discovery");
            return Vec::new();
        }
    };

    let mut sessions = Vec::new();

    for name in &names {
        let Some(uuid_str) = name.strip_prefix(SESSION_PREFIX) else {
            continue;
        };

        let Ok(session_id) = Uuid::parse_str(uuid_str) else {
            tracing::warn!(session_name = %name, "invalid UUID in tmux session name, skipping");
            continue;
        };

        match TmuxSession::reattach(session_id, output_tx.clone()) {
            Ok(session) => {
                tracing::info!(session_id = %session_id, "discovered and reattached tmux session");
                sessions.push(session);
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "failed to reattach discovered tmux session"
                );
            }
        }
    }

    tracing::info!(
        discovered = names.len(),
        reattached = sessions.len(),
        "tmux session discovery complete"
    );

    sessions
}

/// Clean up stale tmux sessions older than `STALE_SESSION_HOURS` and orphaned FIFOs.
pub fn cleanup_stale() {
    // List all zremote sessions with their creation timestamps
    let output = tmux_cmd()
        .args(["list-sessions", "-F", "#{session_name}:#{session_created}"])
        .output();

    let entries = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(_) => {
            // No tmux server running or no sessions - that's fine
            tracing::debug!("no tmux sessions found for cleanup");
            String::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to list tmux sessions for cleanup");
            return;
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let stale_threshold = Duration::from_secs(STALE_SESSION_HOURS * 3600).as_secs();

    for line in entries.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Only process zremote- prefixed sessions
        let Some(rest) = line.strip_prefix(SESSION_PREFIX) else {
            continue;
        };

        // Format: "zremote-{uuid}:{unix_timestamp}"
        let Some((uuid_str, created_str)) = rest.rsplit_once(':') else {
            continue;
        };

        let session_name = format!("{SESSION_PREFIX}{uuid_str}");

        let Ok(created_ts) = created_str.parse::<u64>() else {
            tracing::warn!(session_name = %session_name, raw = %created_str, "could not parse session_created timestamp");
            continue;
        };

        let age_secs = now.saturating_sub(created_ts);
        if age_secs > stale_threshold {
            tracing::info!(
                session_name = %session_name,
                age_hours = age_secs / 3600,
                "killing stale tmux session"
            );
            let _ = tmux_cmd()
                .args(["kill-session", "-t", &session_name])
                .output();
        }
    }

    // Clean up orphaned FIFOs
    let dir = fifo_dir();
    if let Ok(entries) = fs::read_dir(&dir) {
        let active_sessions = list_zremote_sessions().unwrap_or_default();

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };

            // Check if the FIFO's session still has a living tmux session
            let expected_name = format!("{SESSION_PREFIX}{stem}");
            if !active_sessions.contains(&expected_name) {
                tracing::info!(path = %path.display(), "removing orphaned FIFO");
                let _ = fs::remove_file(&path);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Get the stable pane ID (`%N` format) for the first pane of the given tmux target.
///
/// The target can be a session name (returns the active pane), a fully-qualified
/// `session:window.pane` target, or an existing pane ID.
fn get_pane_id(target: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let output = tmux_cmd()
        .args(["list-panes", "-t", target, "-F", "#{pane_id}"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(
            format!("tmux list-panes (pane_id) failed for target '{target}': {stderr}").into(),
        );
    }

    let pane_id = String::from_utf8(output.stdout)?
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();

    if !pane_id.starts_with('%') || pane_id.len() < 2 {
        return Err(format!("invalid pane_id '{pane_id}' from tmux for target '{target}'").into());
    }

    Ok(pane_id)
}

/// Get the shell PID for a tmux session's pane.
fn get_pane_pid(tmux_name: &str) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
    let output = tmux_cmd()
        .args(["list-panes", "-t", tmux_name, "-F", "#{pane_pid}"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("tmux list-panes (pane_pid) failed: {stderr}").into());
    }

    let pid_str = String::from_utf8(output.stdout)?
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();

    let pid: u32 = pid_str
        .parse()
        .map_err(|e| format!("invalid pane_pid '{pid_str}': {e}"))?;

    Ok(pid)
}

/// Create a FIFO (named pipe) at the given path.
fn create_fifo(path: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Remove existing FIFO if present
    let _ = fs::remove_file(path);

    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-UTF8 FIFO path: {}", path.display()))?;

    let output = Command::new("mkfifo").arg(path_str).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("mkfifo failed for {path_str}: {stderr}").into());
    }

    Ok(())
}

/// Set up tmux pipe-pane to redirect pane output to a FIFO.
fn setup_pipe_pane(
    tmux_name: &str,
    fifo_path: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fifo_str = fifo_path
        .to_str()
        .ok_or_else(|| format!("non-UTF8 FIFO path: {}", fifo_path.display()))?;

    // Shell-quote the path to prevent injection via unexpected characters
    let pipe_cmd = format!("cat >> '{}'", fifo_str.replace('\'', "'\\''"));

    let output = tmux_cmd()
        .args(["pipe-pane", "-t", tmux_name, &pipe_cmd])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("tmux pipe-pane failed: {stderr}").into());
    }

    Ok(())
}

/// Check whether a tmux session with the given name exists.
fn tmux_session_exists(tmux_name: &str) -> bool {
    tmux_cmd()
        .args(["has-session", "-t", tmux_name])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// List all zremote-prefixed tmux session names.
fn list_zremote_sessions() -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let output = tmux_cmd()
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()?;

    if !output.status.success() {
        // tmux returns error when no server is running - treat as empty
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8(output.stdout)?;
    let names: Vec<String> = stdout
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with(SESSION_PREFIX))
        .map(String::from)
        .collect();

    Ok(names)
}

/// Spawn a blocking reader task that reads from a FIFO and sends data to the
/// output channel. Follows the same pattern as `pty.rs`: 4KB buffer, EOF
/// signaled with an empty vec.
fn spawn_fifo_reader(
    session_id: SessionId,
    pane_id: Option<String>,
    fifo_path: PathBuf,
    output_tx: mpsc::Sender<PtyOutput>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        // Opening a FIFO for reading blocks until a writer opens the other end.
        // This is expected: tmux pipe-pane will open the write side.
        let file = match fs::File::open(&fifo_path) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(
                    session_id = %session_id,
                    fifo = %fifo_path.display(),
                    error = %e,
                    "failed to open FIFO for reading"
                );
                let _ = output_tx.blocking_send(PtyOutput {
                    session_id,
                    pane_id: pane_id.clone(),
                    data: Vec::new(),
                });
                return;
            }
        };

        let mut reader = std::io::BufReader::new(file);
        let mut buf = [0u8; 4096];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // EOF -- pipe-pane stopped or tmux session ended
                    let _ = output_tx.blocking_send(PtyOutput {
                        session_id,
                        pane_id: pane_id.clone(),
                        data: Vec::new(),
                    });
                    break;
                }
                Ok(n) => {
                    if output_tx
                        .blocking_send(PtyOutput {
                            session_id,
                            pane_id: pane_id.clone(),
                            data: buf[..n].to_vec(),
                        })
                        .is_err()
                    {
                        // Receiver dropped -- connection gone
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "FIFO read error"
                    );
                    let _ = output_tx.blocking_send(PtyOutput {
                        session_id,
                        pane_id: pane_id.clone(),
                        data: Vec::new(),
                    });
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_cmd_uses_dedicated_socket() {
        let cmd = tmux_cmd();
        let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-L");
        assert_eq!(args[1], TMUX_SOCKET);
        assert_eq!(cmd.get_program(), "tmux");
    }

    #[test]
    fn fifo_dir_contains_prefix_and_uid() {
        let dir = fifo_dir();
        let dir_str = dir.to_str().expect("fifo_dir should be valid UTF-8");
        assert!(
            dir_str.starts_with(FIFO_DIR_PREFIX),
            "fifo_dir should start with {FIFO_DIR_PREFIX}, got: {dir_str}"
        );
        // Should be /tmp/zremote-tmux-{some_uid}
        let suffix = dir_str
            .strip_prefix(&format!("{FIFO_DIR_PREFIX}-"))
            .unwrap();
        assert!(!suffix.is_empty(), "fifo_dir should have a UID suffix");
        // UID should be numeric
        assert!(
            suffix.chars().all(|c| c.is_ascii_digit()),
            "UID suffix should be numeric, got: {suffix}"
        );
    }

    #[test]
    fn tmux_session_name_format() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let name = tmux_session_name(id);
        assert_eq!(name, "zremote-550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn tmux_session_name_starts_with_prefix() {
        let id = Uuid::new_v4();
        let name = tmux_session_name(id);
        assert!(
            name.starts_with(SESSION_PREFIX),
            "session name should start with {SESSION_PREFIX}, got: {name}"
        );
    }

    #[test]
    fn tmux_session_name_contains_uuid() {
        let id = Uuid::new_v4();
        let name = tmux_session_name(id);
        let uuid_part = name.strip_prefix(SESSION_PREFIX).unwrap();
        let parsed = Uuid::parse_str(uuid_part);
        assert!(
            parsed.is_ok(),
            "should be able to parse UUID from session name, got: {uuid_part}"
        );
        assert_eq!(parsed.unwrap(), id);
    }

    #[test]
    fn current_uid_returns_numeric_string() {
        let uid = current_uid();
        assert!(
            uid.chars().all(|c| c.is_ascii_digit()),
            "UID should be numeric, got: {uid}"
        );
    }

    #[test]
    fn fifo_dir_is_absolute() {
        let dir = fifo_dir();
        assert!(
            dir.is_absolute(),
            "fifo_dir should return an absolute path, got: {}",
            dir.display()
        );
    }

    #[test]
    fn get_pane_id_rejects_nonexistent_session() {
        // Calling get_pane_id on a non-existent target should return an error
        let result = get_pane_id("nonexistent-session-12345");
        assert!(result.is_err());
    }
}
