use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::types::TerminalEvent;

const TMUX_SOCKET: &str = "zremote";
const FIFO_DIR_PREFIX: &str = "/tmp/zremote-tmux";

/// Connection info returned by the agent's direct-attach endpoint.
pub struct TmuxConnectionInfo {
    pub socket: String,
    pub session_name: String,
    pub pane_id: String,
}

/// Direct tmux handle: bypasses WebSocket, communicates directly with tmux.
pub struct DirectTmuxHandle {
    pub input_tx: flume::Sender<Vec<u8>>,
    pub output_rx: flume::Receiver<TerminalEvent>,
    pub resize_tx: flume::Sender<(u16, u16)>,
    _shutdown_tx: flume::Sender<()>,
    _reader_handle: JoinHandle<()>,
}

/// Check if tmux binary is available on the system.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Connect directly to a tmux session, bypassing WebSocket.
///
/// # Errors
///
/// Returns an error if FIFO creation or tmux pipe-pane setup fails.
pub fn connect(
    info: TmuxConnectionInfo,
    session_id: String,
    tokio_handle: &tokio::runtime::Handle,
) -> Result<DirectTmuxHandle, Box<dyn std::error::Error + Send + Sync>> {
    let (input_tx, input_rx) = flume::bounded::<Vec<u8>>(256);
    let (output_tx, output_rx) = flume::bounded::<TerminalEvent>(256);
    let (resize_tx, resize_rx) = flume::bounded::<(u16, u16)>(16);
    let (shutdown_tx, shutdown_rx) = flume::bounded::<()>(1);

    let dir = fifo_dir();
    fs::create_dir_all(&dir)?;

    let fifo_path = dir.join(format!("{session_id}-gui.fifo"));
    create_fifo(&fifo_path)?;

    // Capture current screen content before setting up pipe-pane
    let pane_id = info.pane_id.clone();
    if let Ok(cap) = tmux_cmd()
        .args(["capture-pane", "-t", &pane_id, "-p", "-e"])
        .output()
        && cap.status.success()
        && !cap.stdout.is_empty()
    {
        let _ = output_tx.send(TerminalEvent::Output(cap.stdout));
    }

    // Stop any existing pipe-pane, then set up new one pointing to GUI's FIFO
    let _ = tmux_cmd().args(["pipe-pane", "-t", &pane_id]).output();
    setup_pipe_pane(&pane_id, &fifo_path)?;

    info!(session_id = %session_id, pane_id = %pane_id, "direct tmux connection established");

    // Spawn FIFO reader task (blocking I/O). Store handle for abort on cleanup.
    let reader_fifo = fifo_path.clone();
    let reader_output_tx = output_tx.clone();
    let reader_session_id = session_id.clone();
    let reader_handle = tokio_handle.spawn(async move {
        tokio::task::spawn_blocking(move || {
            let file = match fs::File::open(&reader_fifo) {
                Ok(f) => f,
                Err(e) => {
                    error!(session_id = %reader_session_id, error = %e, "failed to open GUI FIFO");
                    let _ = reader_output_tx.send(TerminalEvent::SessionClosed { exit_code: None });
                    return;
                }
            };

            let mut reader = std::io::BufReader::new(file);
            let mut buf = [0u8; 4096];

            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF - pipe-pane stopped or tmux session ended
                        let _ =
                            reader_output_tx.send(TerminalEvent::SessionClosed { exit_code: None });
                        break;
                    }
                    Ok(n) => {
                        if reader_output_tx
                            .send(TerminalEvent::Output(buf[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(session_id = %reader_session_id, error = %e, "GUI FIFO read error");
                        let _ =
                            reader_output_tx.send(TerminalEvent::SessionClosed { exit_code: None });
                        break;
                    }
                }
            }
        })
        .await
        .ok();
    });

    // Spawn input writer task (batches keystrokes into send-keys calls).
    // On send-keys failure, notify via output channel so the panel shows an error.
    let writer_pane_id = info.pane_id.clone();
    let writer_session_id = session_id.clone();
    let writer_output_tx = output_tx;
    tokio_handle.spawn(async move {
        loop {
            let Ok(first) = input_rx.recv_async().await else {
                break;
            };

            // Batch: drain any additional pending input
            let mut batch = first;
            while let Ok(more) = input_rx.try_recv() {
                batch.extend(more);
            }

            if batch.is_empty() {
                continue;
            }

            let pane_id = writer_pane_id.clone();
            let sid = writer_session_id.clone();
            // Run send-keys in blocking task to avoid blocking the async runtime
            let result = tokio::task::spawn_blocking(move || {
                let mut cmd = tmux_cmd();
                cmd.args(["send-keys", "-t", &pane_id, "-H"]);
                for byte in &batch {
                    cmd.arg(format!("{byte:02x}"));
                }
                cmd.output()
            })
            .await;

            match result {
                Ok(Ok(output)) if !output.status.success() => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(session_id = %sid, error = %stderr, "tmux send-keys failed");
                    // Signal session closed so the terminal panel shows an error
                    let _ = writer_output_tx
                        .send(TerminalEvent::SessionClosed { exit_code: None });
                    break;
                }
                Ok(Err(e)) => {
                    warn!(session_id = %sid, error = %e, "tmux send-keys error");
                    let _ = writer_output_tx
                        .send(TerminalEvent::SessionClosed { exit_code: None });
                    break;
                }
                Err(_) => break,
                _ => {}
            }
        }
    });

    // Spawn resize task
    let resize_session_name = info.session_name.clone();
    let resize_session_id = session_id.clone();
    tokio_handle.spawn(async move {
        while let Ok((cols, rows)) = resize_rx.recv_async().await {
            let name = resize_session_name.clone();
            let sid = resize_session_id.clone();
            let _ = tokio::task::spawn_blocking(move || {
                // Resize the window so the pane fills it
                let output = tmux_cmd()
                    .args([
                        "resize-window",
                        "-t",
                        &name,
                        "-x",
                        &cols.to_string(),
                        "-y",
                        &rows.to_string(),
                    ])
                    .output();
                if let Ok(o) = output
                    && !o.status.success()
                {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    warn!(session_id = %sid, error = %stderr, "tmux resize-window failed");
                }
            })
            .await;
        }
    });

    // Spawn cleanup task that waits for shutdown signal
    let cleanup_pane_id = info.pane_id;
    let cleanup_fifo = fifo_path;
    let cleanup_session_id = session_id;
    tokio_handle.spawn(async move {
        let _ = shutdown_rx.recv_async().await;
        // Stop pipe-pane
        let pid = cleanup_pane_id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = tmux_cmd().args(["pipe-pane", "-t", &pid]).output();
        })
        .await;
        // Remove GUI FIFO
        let _ = fs::remove_file(&cleanup_fifo);
        info!(session_id = %cleanup_session_id, "direct tmux cleanup complete");
    });

    Ok(DirectTmuxHandle {
        input_tx,
        output_rx,
        resize_tx,
        _shutdown_tx: shutdown_tx,
        _reader_handle: reader_handle,
    })
}

// -- Internal helpers --

fn tmux_cmd() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(["-L", TMUX_SOCKET]);
    cmd
}

fn fifo_dir() -> PathBuf {
    let uid = current_uid();
    PathBuf::from(format!("{FIFO_DIR_PREFIX}-{uid}"))
}

fn current_uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "0".to_owned(), |s| s.trim().to_owned())
}

fn create_fifo(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

fn setup_pipe_pane(
    pane_id: &str,
    fifo_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fifo_str = fifo_path
        .to_str()
        .ok_or_else(|| format!("non-UTF8 FIFO path: {}", fifo_path.display()))?;
    // Shell-quote the path to prevent injection via unexpected characters
    let pipe_cmd = format!("cat >> '{}'", fifo_str.replace('\'', "'\\''"));
    let output = tmux_cmd()
        .args(["pipe-pane", "-t", pane_id, &pipe_cmd])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("tmux pipe-pane failed: {stderr}").into());
    }
    Ok(())
}
