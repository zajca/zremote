use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use myremote_protocol::SessionId;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

const TMUX_SOCKET: &str = "myremote";
const FIFO_DIR_PREFIX: &str = "/tmp/myremote-tmux";
const STALE_SESSION_HOURS: u64 = 24;

/// Session name prefix used for all myremote-managed tmux sessions.
const SESSION_PREFIX: &str = "myremote-";

pub struct TmuxSession {
    session_id: SessionId,
    tmux_name: String,
    fifo_path: PathBuf,
    reader_handle: JoinHandle<()>,
    pid: u32,
}

/// Create a `Command` pre-configured with `tmux -L myremote`.
fn tmux_cmd() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(["-L", TMUX_SOCKET]);
    cmd
}

/// Return the per-UID FIFO directory path: `/tmp/myremote-tmux-{uid}/`.
fn fifo_dir() -> PathBuf {
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
        output_tx: mpsc::Sender<(SessionId, Vec<u8>)>,
    ) -> Result<(Self, u32), Box<dyn std::error::Error + Send + Sync>> {
        let tmux_name = tmux_session_name(session_id);
        let dir = fifo_dir();

        // Ensure FIFO directory exists
        fs::create_dir_all(&dir)?;

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

        // Get shell PID
        let pid = get_pane_pid(&tmux_name)?;

        // Create FIFO
        let fifo_path = dir.join(format!("{session_id}.fifo"));
        create_fifo(&fifo_path)?;

        // Set up pipe-pane to redirect output into the FIFO
        setup_pipe_pane(&tmux_name, &fifo_path)?;

        // Spawn async reader task on FIFO
        let reader_handle = spawn_fifo_reader(session_id, fifo_path.clone(), output_tx);

        let session = Self {
            session_id,
            tmux_name,
            fifo_path,
            reader_handle,
            pid,
        };

        Ok((session, pid))
    }

    /// Reattach to an existing tmux session. Used for session recovery after
    /// agent restart or reconnection.
    pub fn reattach(
        session_id: SessionId,
        output_tx: mpsc::Sender<(SessionId, Vec<u8>)>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let tmux_name = tmux_session_name(session_id);

        // Verify the tmux session exists
        if !tmux_session_exists(&tmux_name) {
            return Err(format!("tmux session {tmux_name} does not exist").into());
        }

        tracing::info!(session_id = %session_id, tmux_name = %tmux_name, "reattaching to tmux session");

        // Get shell PID
        let pid = get_pane_pid(&tmux_name)?;

        // Create/recreate FIFO
        let dir = fifo_dir();
        fs::create_dir_all(&dir)?;
        let fifo_path = dir.join(format!("{session_id}.fifo"));

        // Remove stale FIFO if it exists, then create fresh
        let _ = fs::remove_file(&fifo_path);
        create_fifo(&fifo_path)?;

        // Stop any existing pipe-pane, then set up fresh
        let _ = tmux_cmd().args(["pipe-pane", "-t", &tmux_name]).output();
        setup_pipe_pane(&tmux_name, &fifo_path)?;

        // Spawn async reader task on FIFO
        let reader_handle = spawn_fifo_reader(session_id, fifo_path.clone(), output_tx);

        Ok(Self {
            session_id,
            tmux_name,
            fifo_path,
            reader_handle,
            pid,
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
        cmd.args(["send-keys", "-t", &self.tmux_name, "-H"]);
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

    /// Resize the tmux window.
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

    /// Kill the tmux session and clean up the FIFO.
    pub fn kill(&mut self) {
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
        self.reader_handle.abort();

        // Stop pipe-pane so the FIFO writer side closes
        let _ = tmux_cmd()
            .args(["pipe-pane", "-t", &self.tmux_name])
            .output();

        // Remove the FIFO file
        let _ = fs::remove_file(&self.fifo_path);

        tracing::info!(
            session_id = %self.session_id,
            tmux_name = %self.tmux_name,
            "detached from tmux session (session remains alive)"
        );
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        // Only detach, do NOT kill the tmux session
        self.reader_handle.abort();
        // Remove FIFO
        let _ = fs::remove_file(&self.fifo_path);
        // Stop pipe-pane
        let _ = tmux_cmd()
            .args(["pipe-pane", "-t", &self.tmux_name])
            .output();
    }
}

/// Discover all existing myremote tmux sessions and reattach to them.
/// Sessions that fail to reattach are logged and skipped.
pub fn discover_sessions(output_tx: mpsc::Sender<(SessionId, Vec<u8>)>) -> Vec<TmuxSession> {
    let names = match list_myremote_sessions() {
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
    // List all myremote sessions with their creation timestamps
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

        // Only process myremote- prefixed sessions
        let Some(rest) = line.strip_prefix(SESSION_PREFIX) else {
            continue;
        };

        // Format: "myremote-{uuid}:{unix_timestamp}"
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
        let active_sessions = list_myremote_sessions().unwrap_or_default();

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

    let pipe_cmd = format!("cat >> {fifo_str}");

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

/// List all myremote-prefixed tmux session names.
fn list_myremote_sessions() -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
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
    fifo_path: PathBuf,
    output_tx: mpsc::Sender<(SessionId, Vec<u8>)>,
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
                let _ = output_tx.blocking_send((session_id, Vec::new()));
                return;
            }
        };

        let mut reader = std::io::BufReader::new(file);
        let mut buf = [0u8; 4096];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // EOF -- pipe-pane stopped or tmux session ended
                    let _ = output_tx.blocking_send((session_id, Vec::new()));
                    break;
                }
                Ok(n) => {
                    if output_tx
                        .blocking_send((session_id, buf[..n].to_vec()))
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
                    let _ = output_tx.blocking_send((session_id, Vec::new()));
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
        // Should be /tmp/myremote-tmux-{some_uid}
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
        assert_eq!(name, "myremote-550e8400-e29b-41d4-a716-446655440000");
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
}
