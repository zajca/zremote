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

/// Return the socket directory for the current user.
pub fn socket_dir() -> PathBuf {
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

    let child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn shell");
            return;
        }
    };

    let shell_pid = child.process_id().unwrap_or(0);
    let daemon_pid = std::process::id();

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

                // Forward to connected client (with timeout, skip on failure)
                if let Some(ref mut w) = client_writer {
                    let resp = DaemonResponse::Output { data };
                    let result = tokio::time::timeout(
                        std::time::Duration::from_millis(100),
                        send_response(w, &resp),
                    ).await;
                    if result.is_err() || result.is_ok_and(|r| r.is_err()) {
                        // Write failed or timed out - data is in ring buffer
                        tracing::debug!("socket write failed/timeout, data in ring buffer only");
                    }
                }
            }

            // Shell exited
            Some(exit_code) = pty_eof_rx.recv() => {
                tracing::info!(session_id = %session_id, ?exit_code, "shell exited");

                // Notify connected client
                if let Some(ref mut w) = client_writer {
                    let resp = DaemonResponse::Exited { code: exit_code };
                    let _ = send_response(w, &resp).await;
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
                            let _ = send_response(w, &resp).await;
                        }
                    }
                    DaemonRequest::Shutdown => {
                        tracing::info!(session_id = %session_id, "shutdown requested");
                        // Kill shell
                        if let Ok(mut c) = child_arc.lock() {
                            let _ = c.kill();
                        }
                        cleanup(&socket_path, &state_file_path);
                        return;
                    }
                    DaemonRequest::Ping => {
                        if let Some(ref mut w) = client_writer {
                            let _ = send_response(w, &DaemonResponse::Pong).await;
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

/// Write state file atomically (write .tmp then rename).
fn write_state_file_atomic(path: &Path, state: &DaemonStateFile) -> std::io::Result<()> {
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
fn cleanup(socket_path: &Path, state_file_path: &Path) {
    let _ = std::fs::remove_file(socket_path);
    let _ = std::fs::remove_file(state_file_path);
    tracing::info!(
        socket = %socket_path.display(),
        state = %state_file_path.display(),
        "daemon cleaned up"
    );
}

use std::io::Write;
