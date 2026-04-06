use std::path::PathBuf;

use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zremote_protocol::SessionId;

use super::DaemonStateFile;
use super::protocol::{DaemonRequest, DaemonResponse, read_response, send_request};
use crate::pty::shell_integration::ShellIntegrationConfig;
use crate::session::PtyOutput;

/// Client-side handle to a PTY daemon process.
///
/// Each `DaemonSession` corresponds to one daemon process holding a single
/// PTY master fd. Communication happens over a Unix domain socket using
/// length-prefixed JSON frames.
pub struct DaemonSession {
    session_id: SessionId,
    socket_path: PathBuf,
    state_path: PathBuf,
    log_path: PathBuf,
    daemon_pid: u32,
    shell_pid: u32,
    writer_tx: mpsc::Sender<DaemonRequest>,
    reader_handle: JoinHandle<()>,
    writer_handle: JoinHandle<()>,
}

impl DaemonSession {
    /// Spawn a new PTY daemon process and connect to it.
    ///
    /// Tries `systemd-run --scope --user` first, falls back to direct spawn.
    /// Waits for the daemon state file (poll every 100ms, timeout 3s).
    ///
    /// Returns `(session, shell_pid)`.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
        env: Option<&std::collections::HashMap<String, String>>,
        output_tx: mpsc::Sender<PtyOutput>,
        shell_config: Option<&ShellIntegrationConfig>,
    ) -> Result<(Self, u32), Box<dyn std::error::Error + Send + Sync>> {
        // Use /proc/self/exe on Linux instead of current_exe() resolved path.
        // After recompilation, cargo replaces the binary on disk, so the
        // resolved path points to "(deleted)". /proc/self/exe still works
        // because the kernel keeps the inode alive while the process runs.
        let exe = if cfg!(target_os = "linux") {
            PathBuf::from(format!("/proc/{}/exe", std::process::id()))
        } else {
            std::env::current_exe()?
        };
        let uid = nix::unistd::getuid();
        let sock_dir = PathBuf::from(format!("/tmp/zremote-pty-{uid}"));
        let socket_path = sock_dir.join(format!("{session_id}.sock"));
        let state_path = sock_dir.join(format!("{session_id}.json"));
        let log_path = sock_dir.join(format!("{session_id}.log"));

        // Build args for the daemon subprocess.
        // When running as unified binary (`zremote`), pty-daemon is nested under
        // `agent` subcommand: `zremote agent pty-daemon ...`.
        // When running as standalone agent binary, it's a direct subcommand.
        let is_unified_binary = exe
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name == "zremote");
        let mut args: Vec<String> = Vec::new();
        if is_unified_binary {
            args.push("agent".to_string());
        }
        args.extend([
            "pty-daemon".to_string(),
            "--session-id".to_string(),
            session_id.to_string(),
            "--socket".to_string(),
            socket_path.to_string_lossy().to_string(),
            "--state-file".to_string(),
            state_path.to_string_lossy().to_string(),
            "--shell".to_string(),
            shell.to_string(),
            "--cols".to_string(),
            cols.to_string(),
            "--rows".to_string(),
            rows.to_string(),
        ]);
        if let Some(dir) = working_dir {
            args.push("--working-dir".to_string());
            args.push(dir.to_string());
        }
        if let Some(env_vars) = env {
            for (key, value) in env_vars {
                args.push("--env".to_string());
                args.push(format!("{key}={value}"));
            }
        }

        // Pass shell integration flags to daemon subprocess
        if let Some(config) = shell_config {
            if config.disable_autosuggestions {
                args.push("--disable-autosuggestions".to_string());
            }
            if config.export_env_vars {
                args.push("--export-env-vars".to_string());
            }
            if config.force_sigwinch {
                args.push("--force-sigwinch".to_string());
            }
        }

        // Ensure socket directory exists before opening log file
        {
            use std::os::unix::fs::DirBuilderExt;
            let _ = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&sock_dir);
        }

        // Open per-session log file for daemon stderr (diagnostics)
        let log_file = std::fs::File::create(&log_path).map_err(|e| {
            format!(
                "failed to create daemon log file {}: {e}",
                log_path.display()
            )
        })?;

        // Try systemd-run first, fall back to direct spawn
        let used_systemd = spawn_via_systemd(&exe, &args, &session_id, &log_file).is_ok();
        if !used_systemd {
            tracing::debug!("systemd-run unavailable, falling back to direct spawn");
            spawn_direct(&exe, &args, &log_file)?;
        }

        // Wait for state file (poll 100ms, timeout 3s).
        // If systemd-run "succeeded" (process spawned) but daemon never started,
        // retry with direct spawn as fallback.
        let state = match wait_for_state_file(&state_path).await {
            Ok(state) => state,
            Err(_) if used_systemd => {
                let daemon_log = std::fs::read_to_string(&log_path).unwrap_or_default();
                tracing::warn!(
                    session_id = %session_id,
                    daemon_stderr = %daemon_log.trim(),
                    "systemd scope daemon didn't start, falling back to direct spawn"
                );
                spawn_direct(&exe, &args, &log_file)?;
                wait_for_state_file(&state_path).await.map_err(|e2| {
                    let daemon_log = std::fs::read_to_string(&log_path).unwrap_or_default();
                    format!("{e2}\ndaemon stderr (direct spawn):\n{}", daemon_log.trim())
                })?
            }
            Err(e) => {
                let daemon_log = std::fs::read_to_string(&log_path).unwrap_or_default();
                return Err(format!("{e}\ndaemon stderr:\n{}", daemon_log.trim()).into());
            }
        };

        // Connect to the Unix socket
        let stream = UnixStream::connect(&socket_path).await.map_err(|e| {
            format!(
                "failed to connect to daemon socket {}: {e}",
                socket_path.display()
            )
        })?;

        let (reader_handle, writer_handle, writer_tx) =
            start_io_tasks(session_id, stream, output_tx);

        let session = Self {
            session_id,
            socket_path,
            state_path,
            log_path,
            daemon_pid: state.daemon_pid,
            shell_pid: state.shell_pid,
            writer_tx,
            reader_handle,
            writer_handle,
        };

        Ok((session, state.shell_pid))
    }

    /// Write data to the daemon's PTY stdin.
    pub fn write(&self, data: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.writer_tx
            .try_send(DaemonRequest::Input {
                data: data.to_vec(),
            })
            .map_err(|e| format!("failed to send input to daemon: {e}"))?;
        Ok(())
    }

    /// Resize the daemon's PTY terminal.
    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.writer_tx
            .try_send(DaemonRequest::Resize { cols, rows })
            .map_err(|e| format!("failed to send resize to daemon: {e}"))?;
        Ok(())
    }

    /// Kill the daemon process and clean up state/socket files.
    ///
    /// Sends SIGTERM directly to the daemon process instead of relying on the
    /// async Shutdown channel message (which is lost because `Drop` aborts the
    /// writer task before delivery). When the daemon dies, its PTY master fd
    /// closes, which delivers SIGHUP to the shell's process group.
    pub fn kill(&self) {
        // Kill daemon process directly (no race with Drop aborting writer task)
        let pid = nix::unistd::Pid::from_raw(i32::try_from(self.daemon_pid).unwrap_or(i32::MAX));
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);

        // Clean up files from agent side (don't rely on daemon cleanup running)
        let _ = std::fs::remove_file(&self.state_path);
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.log_path);
    }

    /// Detach from the daemon without sending shutdown.
    /// The daemon process and shell survive.
    pub fn detach(self) {
        self.reader_handle.abort();
        self.writer_handle.abort();
        // Drop writer_tx without sending Shutdown - daemon stays alive
    }

    /// Return the shell process PID.
    pub fn pid(&self) -> u32 {
        self.shell_pid
    }

    /// Return the daemon process PID.
    pub fn daemon_pid(&self) -> u32 {
        self.daemon_pid
    }

    /// Check if the daemon process is still alive.
    /// Returns `None` if alive, `Some(exit_code)` if dead (exit code unavailable, returns 1).
    pub fn try_wait(&self) -> Option<i32> {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(self.daemon_pid).unwrap_or(i32::MAX));
        // Signal 0 = check if process exists without actually signaling
        match nix::sys::signal::kill(pid, None) {
            Ok(()) => None,    // Process is alive
            Err(_) => Some(1), // Process is dead (or we don't have permission)
        }
    }

    /// Reconnect to an existing daemon via its socket path.
    ///
    /// Sends `GetState` to retrieve scrollback, then starts new reader/writer tasks.
    /// Returns `(session, scrollback_data, daemon_started_at)`.
    pub async fn reconnect(
        session_id: SessionId,
        socket_path: PathBuf,
        state_path: PathBuf,
        daemon_pid: u32,
        shell_pid: u32,
        output_tx: mpsc::Sender<PtyOutput>,
    ) -> Result<(Self, Option<Vec<u8>>, Option<String>), Box<dyn std::error::Error + Send + Sync>>
    {
        let stream = UnixStream::connect(&socket_path).await.map_err(|e| {
            format!(
                "failed to connect to daemon socket {}: {e}",
                socket_path.display()
            )
        })?;

        // Split stream to send GetState and read the response
        let (mut read_half, mut write_half) = tokio::io::split(stream);

        // Send GetState request
        send_request(&mut write_half, &DaemonRequest::GetState).await?;

        // Read state response
        let (scrollback, daemon_started_at) = match read_response(&mut read_half).await {
            Ok(DaemonResponse::State {
                scrollback,
                started_at,
                ..
            }) => {
                let sb = if scrollback.is_empty() {
                    None
                } else {
                    Some(scrollback)
                };
                (sb, Some(started_at))
            }
            Ok(other) => {
                tracing::warn!(?other, "unexpected response to GetState");
                (None, None)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to read GetState response");
                (None, None)
            }
        };

        // Reunite the stream halves for the IO tasks
        let stream = read_half.unsplit(write_half);

        let (reader_handle, writer_handle, writer_tx) =
            start_io_tasks(session_id, stream, output_tx);

        let log_path = socket_path.with_extension("log");
        let session = Self {
            session_id,
            socket_path,
            state_path,
            log_path,
            daemon_pid,
            shell_pid,
            writer_tx,
            reader_handle,
            writer_handle,
        };

        Ok((session, scrollback, daemon_started_at))
    }

    /// Return the session ID.
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Return the socket path.
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    /// Return the state file path.
    pub fn state_path(&self) -> &PathBuf {
        &self.state_path
    }
}

impl Drop for DaemonSession {
    fn drop(&mut self) {
        self.reader_handle.abort();
        self.writer_handle.abort();
    }
}

/// Start reader and writer I/O tasks for the daemon socket connection.
///
/// Reader: reads `DaemonResponse` from socket, forwards Output/Exited to `output_tx`.
/// Writer: reads `DaemonRequest` from `writer_tx`, sends to socket.
fn start_io_tasks(
    session_id: SessionId,
    stream: UnixStream,
    output_tx: mpsc::Sender<PtyOutput>,
) -> (JoinHandle<()>, JoinHandle<()>, mpsc::Sender<DaemonRequest>) {
    let (read_half, write_half) = tokio::io::split(stream);
    let (writer_tx, mut writer_rx) = mpsc::channel::<DaemonRequest>(256);

    // Reader task: socket -> output_tx
    let reader_handle = tokio::spawn(async move {
        let mut reader = read_half;
        loop {
            match read_response(&mut reader).await {
                Ok(DaemonResponse::Output { data }) => {
                    if output_tx
                        .send(PtyOutput {
                            session_id,
                            pane_id: None,
                            data,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(DaemonResponse::Exited { .. }) => {
                    // Signal EOF
                    let _ = output_tx
                        .send(PtyOutput {
                            session_id,
                            pane_id: None,
                            data: Vec::new(),
                        })
                        .await;
                    break;
                }
                Ok(DaemonResponse::State { .. } | DaemonResponse::Pong) => {
                    // Ignore state/pong in normal reader flow
                }
                Err(e) => {
                    // Connection lost - signal EOF
                    tracing::warn!(error = %e, session_id = %session_id, "daemon socket read error, session ending");
                    let _ = output_tx
                        .send(PtyOutput {
                            session_id,
                            pane_id: None,
                            data: Vec::new(),
                        })
                        .await;
                    break;
                }
            }
        }
    });

    // Writer task: writer_rx -> socket
    let writer_handle = tokio::spawn(async move {
        let mut writer = write_half;
        while let Some(req) = writer_rx.recv().await {
            if let Err(e) = send_request(&mut writer, &req).await {
                tracing::debug!(error = %e, "daemon socket write error");
                break;
            }
        }
    });

    (reader_handle, writer_handle, writer_tx)
}

/// Try to spawn the daemon via systemd-run for cgroup isolation.
///
/// Uses `--no-block` and `spawn()` to avoid waiting for the scope to finish.
/// The state file poll in `wait_for_state_file()` handles daemon readiness.
fn spawn_via_systemd(
    exe: &std::path::Path,
    args: &[String],
    session_id: &SessionId,
    log_file: &std::fs::File,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let unit_name = format!("zremote-pty-{session_id}");
    let stderr_file = log_file.try_clone()?;
    let mut cmd = tokio::process::Command::new("systemd-run");
    cmd.arg("--scope")
        .arg("--user")
        .arg("--unit")
        .arg(&unit_name)
        .arg("--no-block")
        .arg("--")
        .arg(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(stderr_file));

    // Note: do NOT use .process_group(0) here — systemd-run --scope needs to
    // stay in the caller's session to communicate with the user's systemd manager.
    // The daemon itself calls setsid() on startup, which provides process group
    // isolation. The systemd scope provides cgroup isolation.
    cmd.spawn()?;
    Ok(())
}

/// Spawn daemon directly via tokio Command (fallback when systemd is unavailable).
fn spawn_direct(
    exe: &std::path::Path,
    args: &[String],
    log_file: &std::fs::File,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stderr_file = log_file.try_clone()?;
    tokio::process::Command::new(exe)
        .args(args)
        // Note: do NOT use .process_group(0) here — it makes the child a process
        // group leader, which causes the daemon's setsid() call to fail with EPERM.
        // The daemon calls setsid() itself on startup, providing full isolation.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(stderr_file))
        .spawn()?;
    Ok(())
}

/// Wait for the daemon state file to appear (poll every 100ms, timeout 3s).
async fn wait_for_state_file(
    state_path: &std::path::Path,
) -> Result<DaemonStateFile, Box<dyn std::error::Error + Send + Sync>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if tokio::fs::try_exists(state_path).await.unwrap_or(false) {
            match tokio::fs::read_to_string(state_path).await {
                Ok(contents) => match serde_json::from_str::<DaemonStateFile>(&contents) {
                    Ok(state) => return Ok(state),
                    Err(e) => {
                        tracing::debug!(error = %e, "state file not ready yet (parse error)");
                    }
                },
                Err(e) => {
                    tracing::debug!(error = %e, "state file not ready yet (read error)");
                }
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "timeout waiting for daemon state file: {}",
                state_path.display()
            )
            .into());
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_dir_contains_uid() {
        let uid = nix::unistd::getuid();
        let dir = super::super::socket_dir();
        assert!(
            dir.to_string_lossy().contains(&uid.to_string()),
            "socket dir should contain uid: {}",
            dir.display()
        );
    }

    #[test]
    fn state_file_round_trip() {
        let state = DaemonStateFile {
            version: 1,
            session_id: "test-123".to_string(),
            shell: "/bin/zsh".to_string(),
            shell_pid: 1234,
            daemon_pid: 1235,
            cols: 80,
            rows: 24,
            started_at: "2026-03-25T10:00:00Z".to_string(),
        };

        let json = serde_json::to_string(&state).unwrap();
        let decoded: DaemonStateFile = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.session_id, "test-123");
        assert_eq!(decoded.shell_pid, 1234);
        assert_eq!(decoded.daemon_pid, 1235);
    }

    #[tokio::test]
    async fn wait_for_state_file_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent.json");
        let result = wait_for_state_file(&path).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("timeout"),
            "should mention timeout"
        );
    }

    #[tokio::test]
    async fn wait_for_state_file_success() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.json");

        let state = DaemonStateFile {
            version: 1,
            session_id: "abc".to_string(),
            shell: "/bin/sh".to_string(),
            shell_pid: 100,
            daemon_pid: 101,
            cols: 80,
            rows: 24,
            started_at: "2026-01-01T00:00:00Z".to_string(),
        };

        // Write state file before calling wait
        let json = serde_json::to_string(&state).unwrap();
        std::fs::write(&path, json).unwrap();

        let result = wait_for_state_file(&path).await.unwrap();
        assert_eq!(result.session_id, "abc");
        assert_eq!(result.shell_pid, 100);
    }

    #[tokio::test]
    async fn wait_for_state_file_delayed_write() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("delayed.json");
        let path_clone = path.clone();

        // Spawn a task that writes the state file after 500ms
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let state = DaemonStateFile {
                version: 1,
                session_id: "delayed".to_string(),
                shell: "/bin/sh".to_string(),
                shell_pid: 200,
                daemon_pid: 201,
                cols: 100,
                rows: 30,
                started_at: "2026-03-25T12:00:00Z".to_string(),
            };
            let json = serde_json::to_string(&state).unwrap();
            std::fs::write(&path_clone, json).unwrap();
        });

        let result = wait_for_state_file(&path).await.unwrap();
        assert_eq!(result.session_id, "delayed");
        assert_eq!(result.shell_pid, 200);
        assert_eq!(result.cols, 100);
        assert_eq!(result.rows, 30);
    }

    #[tokio::test]
    async fn wait_for_state_file_invalid_json_then_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("evolving.json");

        // Write invalid JSON first
        std::fs::write(&path, "not valid json yet").unwrap();

        let path_clone = path.clone();
        // After 500ms, overwrite with valid JSON
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let state = DaemonStateFile {
                version: 1,
                session_id: "evolved".to_string(),
                shell: "/bin/sh".to_string(),
                shell_pid: 300,
                daemon_pid: 301,
                cols: 80,
                rows: 24,
                started_at: "2026-03-25T13:00:00Z".to_string(),
            };
            let json = serde_json::to_string(&state).unwrap();
            std::fs::write(&path_clone, json).unwrap();
        });

        let result = wait_for_state_file(&path).await.unwrap();
        assert_eq!(result.session_id, "evolved");
    }

    #[tokio::test]
    async fn start_io_tasks_forwards_output() {
        use tokio::net::UnixStream;

        let (client_stream, daemon_stream) = UnixStream::pair().unwrap();
        let session_id: SessionId = uuid::Uuid::new_v4();

        let (output_tx, mut output_rx) = mpsc::channel::<PtyOutput>(64);

        let (reader_handle, writer_handle, writer_tx) =
            start_io_tasks(session_id, client_stream, output_tx);

        // Simulate daemon sending an Output response on daemon_stream
        let (mut daemon_read, mut daemon_write) = tokio::io::split(daemon_stream);
        let resp = super::super::protocol::DaemonResponse::Output {
            data: b"hello from daemon".to_vec(),
        };
        super::super::protocol::send_response(&mut daemon_write, &resp)
            .await
            .unwrap();

        // Read from output_rx
        let output = tokio::time::timeout(std::time::Duration::from_secs(2), output_rx.recv())
            .await
            .expect("should receive output within timeout")
            .expect("channel should not be closed");
        assert_eq!(output.data, b"hello from daemon");
        assert_eq!(output.session_id, session_id);

        // Test writer: send a request through writer_tx and read from daemon side
        writer_tx.send(DaemonRequest::Ping).await.unwrap();

        let req = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            super::super::protocol::read_request(&mut daemon_read),
        )
        .await
        .expect("should receive request within timeout")
        .unwrap();
        assert_eq!(req, DaemonRequest::Ping);

        // Clean up
        reader_handle.abort();
        writer_handle.abort();
    }

    #[tokio::test]
    async fn start_io_tasks_handles_exited() {
        use tokio::net::UnixStream;

        let (client_stream, daemon_stream) = UnixStream::pair().unwrap();
        let session_id: SessionId = uuid::Uuid::new_v4();

        let (output_tx, mut output_rx) = mpsc::channel::<PtyOutput>(64);

        let (reader_handle, writer_handle, _writer_tx) =
            start_io_tasks(session_id, client_stream, output_tx);

        // Daemon sends Exited
        let (_daemon_read, mut daemon_write) = tokio::io::split(daemon_stream);
        let resp = super::super::protocol::DaemonResponse::Exited { code: Some(0) };
        super::super::protocol::send_response(&mut daemon_write, &resp)
            .await
            .unwrap();

        // Should receive empty data (EOF signal)
        let output = tokio::time::timeout(std::time::Duration::from_secs(2), output_rx.recv())
            .await
            .expect("should receive within timeout")
            .expect("channel should not be closed");
        assert!(
            output.data.is_empty(),
            "Exited should produce empty data (EOF)"
        );

        reader_handle.abort();
        writer_handle.abort();
    }

    #[tokio::test]
    async fn start_io_tasks_handles_disconnect() {
        use tokio::net::UnixStream;

        let (client_stream, daemon_stream) = UnixStream::pair().unwrap();
        let session_id: SessionId = uuid::Uuid::new_v4();

        let (output_tx, mut output_rx) = mpsc::channel::<PtyOutput>(64);

        let (reader_handle, _writer_handle, _writer_tx) =
            start_io_tasks(session_id, client_stream, output_tx);

        // Drop daemon side to simulate disconnect
        drop(daemon_stream);

        // Should receive EOF signal (empty data)
        let output = tokio::time::timeout(std::time::Duration::from_secs(2), output_rx.recv())
            .await
            .expect("should receive within timeout")
            .expect("channel should not be closed");
        assert!(
            output.data.is_empty(),
            "disconnect should produce empty data (EOF)"
        );

        reader_handle.abort();
    }

    #[tokio::test]
    async fn start_io_tasks_ignores_state_and_pong() {
        use tokio::net::UnixStream;

        let (client_stream, daemon_stream) = UnixStream::pair().unwrap();
        let session_id: SessionId = uuid::Uuid::new_v4();

        let (output_tx, mut output_rx) = mpsc::channel::<PtyOutput>(64);

        let (reader_handle, writer_handle, _writer_tx) =
            start_io_tasks(session_id, client_stream, output_tx);

        let (_daemon_read, mut daemon_write) = tokio::io::split(daemon_stream);

        // Send State response (should be ignored by reader)
        let state_resp = super::super::protocol::DaemonResponse::State {
            session_id: session_id.to_string(),
            shell_pid: 100,
            daemon_pid: 101,
            cols: 80,
            rows: 24,
            scrollback: vec![],
            started_at: "2026-01-01T00:00:00Z".to_string(),
        };
        super::super::protocol::send_response(&mut daemon_write, &state_resp)
            .await
            .unwrap();

        // Send Pong (should be ignored)
        super::super::protocol::send_response(
            &mut daemon_write,
            &super::super::protocol::DaemonResponse::Pong,
        )
        .await
        .unwrap();

        // Send real Output after the ignored messages
        let output_resp = super::super::protocol::DaemonResponse::Output {
            data: b"real data".to_vec(),
        };
        super::super::protocol::send_response(&mut daemon_write, &output_resp)
            .await
            .unwrap();

        // Should only receive the real output, State and Pong are silently ignored
        let output = tokio::time::timeout(std::time::Duration::from_secs(2), output_rx.recv())
            .await
            .expect("should receive within timeout")
            .expect("channel should not be closed");
        assert_eq!(output.data, b"real data");

        reader_handle.abort();
        writer_handle.abort();
    }

    #[tokio::test]
    async fn start_io_tasks_writer_sends_multiple_requests() {
        use tokio::net::UnixStream;

        let (client_stream, daemon_stream) = UnixStream::pair().unwrap();
        let session_id: SessionId = uuid::Uuid::new_v4();

        let (output_tx, _output_rx) = mpsc::channel::<PtyOutput>(64);

        let (reader_handle, writer_handle, writer_tx) =
            start_io_tasks(session_id, client_stream, output_tx);

        let (mut daemon_read, _daemon_write) = tokio::io::split(daemon_stream);

        // Send several requests
        writer_tx
            .send(DaemonRequest::Input {
                data: b"ls\n".to_vec(),
            })
            .await
            .unwrap();
        writer_tx
            .send(DaemonRequest::Resize {
                cols: 120,
                rows: 40,
            })
            .await
            .unwrap();
        writer_tx.send(DaemonRequest::GetState).await.unwrap();

        // Read them all from daemon side
        let req1 = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            super::super::protocol::read_request(&mut daemon_read),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            req1,
            DaemonRequest::Input {
                data: b"ls\n".to_vec()
            }
        );

        let req2 = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            super::super::protocol::read_request(&mut daemon_read),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            req2,
            DaemonRequest::Resize {
                cols: 120,
                rows: 40
            }
        );

        let req3 = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            super::super::protocol::read_request(&mut daemon_read),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(req3, DaemonRequest::GetState);

        reader_handle.abort();
        writer_handle.abort();
    }

    #[test]
    fn try_wait_for_dead_process() {
        // We can't easily construct a DaemonSession without a real daemon,
        // but we can test the kill-check logic via nix directly
        let pid = nix::unistd::Pid::from_raw(99_999_999);
        let result = nix::sys::signal::kill(pid, None);
        assert!(result.is_err(), "nonexistent PID should fail signal check");
    }
}
