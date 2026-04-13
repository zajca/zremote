pub mod discovery;
pub mod protocol;
pub mod session;

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::net::UnixListener;
use tokio::signal::unix::SignalKind;
use tokio::sync::mpsc;

use protocol::{DaemonRequest, DaemonResponse, RING_BUFFER_CAPACITY, read_request, send_response};

use crate::pty::shell_integration::ShellIntegrationConfig;

/// Timeout for PTY output writes (high frequency, latency-sensitive).
const OUTPUT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Timeout for GetState, Ping, and Exited response writes (less frequent, larger payloads).
const RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// State file structure written after socket bind.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct DaemonStateFile {
    pub version: u32,
    pub session_id: String,
    pub shell: String,
    pub shell_pid: u32,
    pub daemon_pid: u32,
    pub cols: u16,
    pub rows: u16,
    pub started_at: String,
}

/// Return a scoped socket directory for the current user and agent instance.
///
/// The directory name includes a hash of `instance_key` (the canonical DB path
/// in local mode, or the server URL in server mode) so that two agent instances
/// on the same machine use separate socket namespaces and never collide.
pub fn socket_dir(instance_key: &str) -> PathBuf {
    let uid = nix::unistd::getuid();
    let hash = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, instance_key.as_bytes());
    let b = hash.as_bytes();
    let short = format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]
    );
    PathBuf::from(format!("/tmp/zremote-pty-{uid}-{short}"))
}

/// Return the legacy (pre-scoping) socket directory path.
///
/// Used only for migration warnings when upgrading from an older agent version.
pub fn legacy_socket_dir() -> PathBuf {
    let uid = nix::unistd::getuid();
    PathBuf::from(format!("/tmp/zremote-pty-{uid}"))
}

/// Run the PTY daemon event loop.
///
/// IMPORTANT: `setsid()` must be called BEFORE this function (in main, before tokio runtime).
#[allow(clippy::too_many_arguments)]
pub async fn run_pty_daemon(
    session_id: String,
    socket_path: PathBuf,
    state_file_path: PathBuf,
    shell: String,
    cols: u16,
    rows: u16,
    working_dir: Option<PathBuf>,
    extra_env: std::collections::HashMap<String, String>,
    shell_config: Option<ShellIntegrationConfig>,
) {
    // 1. Ignore SIGHUP (safe, no unsafe block)
    let mut sighup = tokio::signal::unix::signal(SignalKind::hangup())
        .expect("failed to register SIGHUP handler");
    tokio::spawn(async move {
        loop {
            sighup.recv().await;
        }
    });

    // 2. Open PTY via portable-pty
    let pty_system = native_pty_system();
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pair = match pty_system.openpty(size) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to open PTY");
            return;
        }
    };

    // 3. Spawn shell
    let mut cmd = CommandBuilder::new(&shell);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    for (key, value) in &extra_env {
        cmd.env(key, value);
    }
    if let Some(dir) = &working_dir {
        cmd.cwd(dir);
    }

    // Apply shell integration (env vars, autosuggestion disabling, etc.)
    // Parse session_id as UUID for the prepare function
    let _integration_state = if let Some(ref config) = shell_config {
        if let Ok(sid) = uuid::Uuid::parse_str(&session_id) {
            match crate::pty::shell_integration::prepare(sid, &shell, config, &mut cmd) {
                Ok(state) => state,
                Err(e) => {
                    tracing::warn!(error = %e, "shell integration failed, continuing without it");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn shell");
            return;
        }
    };

    let daemon_pid = std::process::id();
    let shell_pid = match child.process_id() {
        Some(pid) if pid != 0 => pid,
        other => {
            tracing::warn!(
                raw_pid = ?other,
                "could not determine shell PID, using daemon PID as fallback"
            );
            // Use daemon_pid: it is always valid and never 0. The shutdown
            // handler guards with `shell_pid != daemon_pid` so no signal
            // is sent in this fallback case.
            daemon_pid
        }
    };

    let master = pair.master;
    let mut writer = match master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "failed to take PTY writer");
            return;
        }
    };
    let mut reader = match master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "failed to clone PTY reader");
            return;
        }
    };

    // 4. Create socket directory with 0700 permissions (atomic via DirBuilder mode)
    let socket_dir = socket_path.parent().expect("socket path must have parent");
    {
        use std::os::unix::fs::DirBuilderExt;
        if let Err(e) = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(socket_dir)
        {
            tracing::error!(error = %e, "failed to create socket directory");
            return;
        }
        // Always enforce permissions even if dir already existed (may have wrong perms)
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(socket_dir, std::fs::Permissions::from_mode(0o700))
        {
            tracing::error!(error = %e, "failed to set socket directory permissions");
            return;
        }
    }

    // 5. Unlink socket if it exists (cleanup after SIGKILL)
    let _ = std::fs::remove_file(&socket_path);

    // Validate socket path < 104 bytes (macOS sun_path limit)
    let socket_path_str = socket_path.to_string_lossy();
    if socket_path_str.len() >= 104 {
        tracing::error!(
            path = %socket_path_str,
            len = socket_path_str.len(),
            "socket path too long (>= 104 bytes, macOS sun_path limit)"
        );
        return;
    }

    // 6. Bind Unix socket listener
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, path = %socket_path.display(), "failed to bind Unix socket");
            return;
        }
    };

    // Set socket file permissions to 0600 (owner-only access)
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
        {
            tracing::error!(error = %e, "failed to set socket file permissions");
            return;
        }
    }

    tracing::info!(
        session_id = %session_id,
        socket = %socket_path.display(),
        shell_pid,
        daemon_pid,
        "daemon socket bound"
    );

    // 7. Write state file AFTER bind (atomic: write .tmp then rename)
    let started_at = chrono::Utc::now().to_rfc3339();
    let state = DaemonStateFile {
        version: 1,
        session_id: session_id.clone(),
        shell: shell.clone(),
        shell_pid,
        daemon_pid,
        cols,
        rows,
        started_at: started_at.clone(),
    };

    if let Err(e) = write_state_file_atomic(&state_file_path, &state) {
        tracing::error!(error = %e, "failed to write state file");
        return;
    }

    tracing::info!(
        state_file = %state_file_path.display(),
        "daemon state file written"
    );

    // 8. Event loop
    let mut ring_buffer: VecDeque<u8> = VecDeque::with_capacity(RING_BUFFER_CAPACITY);
    let mut current_cols = cols;
    let mut current_rows = rows;

    // Channel for PTY output from blocking reader
    let (pty_tx, mut pty_rx) = mpsc::channel::<Vec<u8>>(256);
    let (pty_eof_tx, mut pty_eof_rx) = mpsc::channel::<Option<i32>>(1);

    // Spawn blocking PTY reader
    let child_arc = std::sync::Arc::new(std::sync::Mutex::new(child));
    let child_for_reader = child_arc.clone();
    tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    // EOF or error - shell exited
                    let exit_code = child_for_reader
                        .lock()
                        .ok()
                        .and_then(|mut c| c.try_wait().ok().flatten())
                        .map(|s| s.exit_code().cast_signed());
                    let _ = pty_eof_tx.blocking_send(exit_code);
                    break;
                }
                Ok(n) => {
                    if pty_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Accept loop runs in background, forwarding new connections
    let (new_conn_tx, mut new_conn_rx) = mpsc::channel::<tokio::net::UnixStream>(4);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    if new_conn_tx.send(stream).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to accept connection");
                }
            }
        }
    });

    // Current client write half (Option because no client initially)
    let mut client_writer: Option<tokio::io::WriteHalf<tokio::net::UnixStream>> = None;
    let mut reader_handle: Option<tokio::task::JoinHandle<()>> = None;
    // Create a new channel pair for each client connection to avoid request leakage.
    // Initial sender is unused (overwritten on first connection).
    let (mut client_tx, mut client_rx) = mpsc::channel::<DaemonRequest>(64);
    let _ = &client_tx; // suppress unused_assignments for initial value

    loop {
        tokio::select! {
            // New client connection
            Some(stream) = new_conn_rx.recv() => {
                // Drop old reader task
                if let Some(handle) = reader_handle.take() {
                    handle.abort();
                }

                // Create a fresh channel for the new connection so old requests don't leak
                let (new_tx, new_rx) = mpsc::channel::<DaemonRequest>(64);
                client_tx = new_tx;
                client_rx = new_rx;

                let (read_half, write_half) = tokio::io::split(stream);
                client_writer = Some(write_half);

                // Spawn reader for this client
                let req_tx = client_tx.clone();
                reader_handle = Some(tokio::spawn(async move {
                    let mut reader = read_half;
                    while let Ok(req) = read_request(&mut reader).await {
                        if req_tx.send(req).await.is_err() {
                            break;
                        }
                    }
                }));

                tracing::debug!("new client connected");
            }

            // PTY output from blocking reader
            Some(data) = pty_rx.recv() => {
                // Append to ring buffer
                ring_buffer.extend(&data);
                let overflow = ring_buffer.len().saturating_sub(RING_BUFFER_CAPACITY);
                if overflow > 0 {
                    ring_buffer.drain(..overflow);
                }

                // Forward to connected client. On failure, disconnect to avoid
                // corrupting the length-prefixed protocol (partial frame from
                // cancelled write_all). Data is safe in ring buffer; agent can
                // reconnect and get scrollback via GetState.
                if let Some(ref mut w) = client_writer {
                    let resp = DaemonResponse::Output { data };
                    let failed = send_with_timeout(w, &resp, OUTPUT_TIMEOUT, "Output").await;
                    if failed {
                        client_writer = None;
                        if let Some(handle) = reader_handle.take() {
                            handle.abort();
                        }
                    }
                }
            }

            // Shell exited
            Some(exit_code) = pty_eof_rx.recv() => {
                tracing::info!(session_id = %session_id, ?exit_code, "shell exited");

                // Notify connected client (best-effort with timeout)
                if let Some(ref mut w) = client_writer {
                    let resp = DaemonResponse::Exited { code: exit_code };
                    let _ = send_with_timeout(w, &resp, RESPONSE_TIMEOUT, "Exited").await;
                }

                // Cleanup
                cleanup(&socket_path, &state_file_path);
                return;
            }

            // Client request
            Some(req) = client_rx.recv() => {
                match req {
                    DaemonRequest::Input { data } => {
                        // NOTE: Blocking write is acceptable here because the daemon runs on a
                        // single-thread tokio runtime. PTY writes are typically fast (kernel buffer).
                        if let Err(e) = writer.write_all(&data) {
                            tracing::warn!(error = %e, "failed to write to PTY");
                        }
                        let _ = writer.flush();
                    }
                    DaemonRequest::Resize { cols: new_cols, rows: new_rows } => {
                        current_cols = new_cols;
                        current_rows = new_rows;
                        let _ = master.resize(PtySize {
                            rows: new_rows,
                            cols: new_cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    DaemonRequest::GetState => {
                        if let Some(ref mut w) = client_writer {
                            let scrollback: Vec<u8> = ring_buffer.iter().copied().collect();
                            let resp = DaemonResponse::State {
                                session_id: session_id.clone(),
                                shell_pid,
                                daemon_pid,
                                cols: current_cols,
                                rows: current_rows,
                                scrollback,
                                started_at: started_at.clone(),
                            };
                            let failed = send_with_timeout(w, &resp, RESPONSE_TIMEOUT, "GetState").await;
                            if failed {
                                client_writer = None;
                                if let Some(handle) = reader_handle.take() {
                                    handle.abort();
                                }
                            }
                        }
                    }
                    DaemonRequest::Shutdown => {
                        tracing::info!(session_id = %session_id, "shutdown requested");
                        // Send SIGTERM to shell (not portable-pty's kill() which sends SIGHUP).
                        // Guard: skip if shell_pid == daemon_pid (fallback for unknown PID).
                        if shell_pid != daemon_pid {
                            let pid = nix::unistd::Pid::from_raw(shell_pid.cast_signed());
                            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
                        }
                        cleanup(&socket_path, &state_file_path);
                        return;
                    }
                    DaemonRequest::Ping => {
                        if let Some(ref mut w) = client_writer {
                            let failed = send_with_timeout(w, &DaemonResponse::Pong, RESPONSE_TIMEOUT, "Ping").await;
                            if failed {
                                client_writer = None;
                                if let Some(handle) = reader_handle.take() {
                                    handle.abort();
                                }
                            }
                        }
                    }
                }
            }

            else => {
                // All channels closed
                tracing::info!("all channels closed, daemon exiting");
                cleanup(&socket_path, &state_file_path);
                return;
            }
        }
    }
}

/// Send a daemon response with a timeout. Returns `true` if the write failed
/// (error or timeout), meaning the client should be disconnected.
async fn send_with_timeout<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    resp: &DaemonResponse,
    timeout: std::time::Duration,
    label: &str,
) -> bool {
    let result = tokio::time::timeout(timeout, send_response(writer, resp)).await;
    match result {
        Ok(Ok(())) => false,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "{label} socket write error, disconnecting client");
            true
        }
        Err(_) => {
            tracing::warn!("{label} socket write timed out, disconnecting client");
            true
        }
    }
}

/// Write state file atomically (write .tmp then rename).
pub(crate) fn write_state_file_atomic(path: &Path, state: &DaemonStateFile) -> std::io::Result<()> {
    let tmp_path = path.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    std::fs::write(&tmp_path, &data)?;
    // Set 0600 on the tmp file BEFORE rename to avoid a window with open perms
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Remove socket and state files on exit.
pub(crate) fn cleanup(socket_path: &Path, state_file_path: &Path) {
    let _ = std::fs::remove_file(socket_path);
    let _ = std::fs::remove_file(state_file_path);
    tracing::info!(
        socket = %socket_path.display(),
        state = %state_file_path.display(),
        "daemon cleaned up"
    );
}

use std::io::Write;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> DaemonStateFile {
        DaemonStateFile {
            version: 1,
            session_id: "test-session-001".to_string(),
            shell: "/bin/sh".to_string(),
            shell_pid: 12345,
            daemon_pid: 12346,
            cols: 80,
            rows: 24,
            started_at: "2026-03-25T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn write_state_file_atomic_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");

        let state = sample_state();
        write_state_file_atomic(&path, &state).unwrap();

        assert!(path.exists(), "state file should exist after write");
        // Tmp file should not remain
        assert!(
            !path.with_extension("json.tmp").exists(),
            "tmp file should be cleaned up"
        );

        // Read back and verify
        let contents = std::fs::read_to_string(&path).unwrap();
        let decoded: DaemonStateFile = serde_json::from_str(&contents).unwrap();
        assert_eq!(decoded.session_id, "test-session-001");
        assert_eq!(decoded.shell, "/bin/sh");
        assert_eq!(decoded.shell_pid, 12345);
        assert_eq!(decoded.daemon_pid, 12346);
        assert_eq!(decoded.cols, 80);
        assert_eq!(decoded.rows, 24);
    }

    #[test]
    fn write_state_file_atomic_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");

        let mut state = sample_state();
        write_state_file_atomic(&path, &state).unwrap();

        // Overwrite with different data
        state.session_id = "updated-session".to_string();
        state.cols = 120;
        state.rows = 40;
        write_state_file_atomic(&path, &state).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let decoded: DaemonStateFile = serde_json::from_str(&contents).unwrap();
        assert_eq!(decoded.session_id, "updated-session");
        assert_eq!(decoded.cols, 120);
        assert_eq!(decoded.rows, 40);
    }

    #[test]
    fn write_state_file_atomic_sets_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");
        let state = sample_state();
        write_state_file_atomic(&path, &state).unwrap();

        let metadata = std::fs::metadata(&path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "state file should have 0600 permissions");
    }

    #[test]
    fn write_state_file_atomic_fails_on_bad_path() {
        let state = sample_state();
        let result = write_state_file_atomic(Path::new("/nonexistent/dir/state.json"), &state);
        assert!(result.is_err());
    }

    #[test]
    fn cleanup_removes_both_files() {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("test.sock");
        let state_path = tmp.path().join("test.json");

        // Create both files
        std::fs::write(&socket_path, "socket").unwrap();
        std::fs::write(&state_path, "state").unwrap();
        assert!(socket_path.exists());
        assert!(state_path.exists());

        cleanup(&socket_path, &state_path);

        assert!(!socket_path.exists(), "socket should be removed");
        assert!(!state_path.exists(), "state file should be removed");
    }

    #[test]
    fn cleanup_handles_missing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("missing.sock");
        let state_path = tmp.path().join("missing.json");

        // Should not panic even if files don't exist
        cleanup(&socket_path, &state_path);
    }

    #[test]
    fn cleanup_handles_partial_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("test.sock");
        let state_path = tmp.path().join("test.json");

        // Only socket exists
        std::fs::write(&socket_path, "socket").unwrap();
        cleanup(&socket_path, &state_path);
        assert!(!socket_path.exists());

        // Only state exists
        std::fs::write(&state_path, "state").unwrap();
        cleanup(&socket_path, &state_path);
        assert!(!state_path.exists());
    }

    #[test]
    fn socket_dir_is_scoped_by_instance_key() {
        let uid = nix::unistd::getuid();
        let dir = socket_dir("test-key");
        let s = dir.to_string_lossy();
        assert!(s.starts_with(&format!("/tmp/zremote-pty-{uid}-")));
        // Hash suffix should be 16 hex chars (8 bytes)
        let suffix = s.strip_prefix(&format!("/tmp/zremote-pty-{uid}-")).unwrap();
        assert_eq!(suffix.len(), 16, "hash suffix should be 16 hex chars");
    }

    #[test]
    fn socket_dir_different_keys_produce_different_dirs() {
        let dir_a = socket_dir("key-a");
        let dir_b = socket_dir("key-b");
        assert_ne!(dir_a, dir_b);
    }

    #[test]
    fn socket_dir_same_key_is_deterministic() {
        let dir1 = socket_dir("same-key");
        let dir2 = socket_dir("same-key");
        assert_eq!(dir1, dir2);
    }

    #[test]
    fn socket_dir_db_path_key() {
        let dir = socket_dir("/home/user/.zremote/local.db");
        let uid = nix::unistd::getuid();
        assert!(
            dir.to_string_lossy()
                .starts_with(&format!("/tmp/zremote-pty-{uid}-"))
        );
    }

    #[test]
    fn socket_dir_server_url_key() {
        let dir = socket_dir("ws://myserver:3000/ws/agent");
        let uid = nix::unistd::getuid();
        assert!(
            dir.to_string_lossy()
                .starts_with(&format!("/tmp/zremote-pty-{uid}-"))
        );
    }

    #[test]
    fn socket_dir_path_under_macos_limit() {
        // Longest realistic instance key
        let long_key = "a".repeat(500);
        let dir = socket_dir(&long_key);
        // Socket path = dir + "/" + uuid + ".sock" ≈ dir + 42 chars
        let socket_path = dir.join("00000000-0000-0000-0000-000000000000.sock");
        assert!(
            socket_path.to_string_lossy().len() < 104,
            "socket path must be < 104 bytes (macOS sun_path limit), got {}",
            socket_path.to_string_lossy().len()
        );
    }

    #[test]
    fn legacy_socket_dir_has_no_hash() {
        let uid = nix::unistd::getuid();
        let dir = legacy_socket_dir();
        assert_eq!(dir, PathBuf::from(format!("/tmp/zremote-pty-{uid}")));
    }

    #[test]
    fn legacy_and_scoped_differ() {
        let legacy = legacy_socket_dir();
        let scoped = socket_dir("any-key");
        assert_ne!(legacy, scoped);
    }

    #[test]
    fn daemon_state_file_pretty_json() {
        let state = sample_state();
        let json = serde_json::to_string_pretty(&state).unwrap();
        assert!(json.contains("test-session-001"));
        assert!(json.contains("version"));
        assert!(json.contains("shell_pid"));
        // Verify it round-trips through pretty format
        let decoded: DaemonStateFile = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.version, 1);
    }
}
