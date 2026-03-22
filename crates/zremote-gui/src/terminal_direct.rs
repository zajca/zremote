use std::fmt::Write as _;
use std::process::Command;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::types::TerminalEvent;

const TMUX_SOCKET: &str = "zremote";

/// Direct tmux handle: bypasses WebSocket, communicates directly with tmux via control mode.
pub struct DirectTmuxHandle {
    pub input_tx: flume::Sender<Vec<u8>>,
    pub output_rx: flume::Receiver<TerminalEvent>,
    pub resize_tx: flume::Sender<(u16, u16)>,
    _shutdown_tx: flume::Sender<()>,
    _task_handles: Vec<JoinHandle<()>>,
}

/// Check if tmux binary is available on the system.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Check if a tmux session exists on the local `zremote` socket.
/// Returns the pane ID if the session is alive.
pub fn probe_local_session(session_id: &str) -> Option<String> {
    let session_name = format!("zremote-{session_id}");
    let output = tmux_cmd()
        .args(["list-panes", "-t", &session_name, "-F", "#{pane_id}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let pane_id = String::from_utf8(output.stdout).ok()?;
    let pane_id = pane_id.trim();
    if pane_id.is_empty() {
        return None;
    }
    Some(pane_id.to_string())
}

/// Connect to a local tmux session via control mode (`tmux -C`).
///
/// Spawns `tmux -L zremote -C attach-session -t zremote-{session_id}` and communicates
/// via stdin/stdout. This coexists with the agent's pipe-pane without interference.
///
/// # Errors
///
/// Returns an error if the tmux child process fails to spawn.
#[allow(clippy::too_many_lines)]
pub fn connect_standalone(
    session_id: String,
    pane_id: String,
    tokio_handle: &tokio::runtime::Handle,
) -> Result<DirectTmuxHandle, Box<dyn std::error::Error + Send + Sync>> {
    let session_name = format!("zremote-{session_id}");

    let (input_tx, input_rx) = flume::bounded::<Vec<u8>>(256);
    let (output_tx, output_rx) = flume::bounded::<TerminalEvent>(256);
    let (resize_tx, resize_rx) = flume::bounded::<(u16, u16)>(16);
    let (shutdown_tx, shutdown_rx) = flume::bounded::<()>(1);

    // Shared stdin channel: writer, resize, and cleanup tasks all send commands here.
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<String>(256);

    // Enter tokio runtime context for spawning the async child process.
    let guard = tokio_handle.enter();
    let mut child = TokioCommand::new("tmux")
        .args([
            "-L",
            TMUX_SOCKET,
            "-C",
            "attach-session",
            "-t",
            &session_name,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    drop(guard);

    let child_stdin = child.stdin.take().expect("stdin was piped");
    let child_stdout = child.stdout.take().expect("stdout was piped");

    // Queue initial capture-pane command (gets current screen content).
    let _ = stdin_tx.try_send(format!("capture-pane -t {pane_id} -p -e\n"));

    info!(session_id = %session_id, pane_id = %pane_id, "direct tmux connection established");

    let mut handles = Vec::new();

    // -- Stdin writer task: single owner of child stdin --
    let stdin_handle = tokio_handle.spawn(async move {
        let mut stdin = child_stdin;
        while let Some(cmd) = stdin_rx.recv().await {
            if stdin.write_all(cmd.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });
    handles.push(stdin_handle);

    // -- Reader task: parse control mode stdout --
    let reader_pane_id = pane_id.clone();
    let reader_output_tx = output_tx;
    let reader_handle = tokio_handle.spawn(async move {
        let mut reader = BufReader::new(child_stdout);
        let mut line = String::new();
        let mut in_block = false;
        let mut capture_done = false;
        let mut block_lines: Vec<String> = Vec::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    // EOF -- child process exited
                    let _ = reader_output_tx.send(TerminalEvent::SessionClosed { exit_code: None });
                    break;
                }
                Ok(_) => {
                    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');

                    if trimmed.starts_with("%begin ") {
                        in_block = true;
                        block_lines.clear();
                    } else if trimmed.starts_with("%end ") {
                        // Forward capture-pane response (first block) as terminal output.
                        if in_block && !capture_done && !block_lines.is_empty() {
                            capture_done = true;
                            // Frame capture-pane output with ScrollbackStart/End so
                            // the terminal panel recreates Term at correct window size.
                            if reader_output_tx
                                .send(TerminalEvent::ScrollbackStart)
                                .is_err()
                            {
                                break;
                            }
                            let mut content = block_lines.join("\n");
                            content.push('\n');
                            if reader_output_tx
                                .send(TerminalEvent::Output(content.into_bytes()))
                                .is_err()
                            {
                                break;
                            }
                            if reader_output_tx
                                .send(TerminalEvent::ScrollbackEnd)
                                .is_err()
                            {
                                break;
                            }
                        }
                        in_block = false;
                        block_lines.clear();
                    } else if in_block && !capture_done {
                        block_lines.push(trimmed.to_string());
                    } else if !in_block {
                        if let Some(rest) = trimmed.strip_prefix("%output ") {
                            // Format: %PANE_ID value
                            if let Some(space_idx) = rest.find(' ') {
                                let output_pane_id = &rest[..space_idx];
                                if output_pane_id == reader_pane_id {
                                    let value = &rest[space_idx + 1..];
                                    let decoded = decode_tmux_octal(value);
                                    if reader_output_tx
                                        .send(TerminalEvent::Output(decoded))
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                            }
                        } else if trimmed.starts_with("%exit") {
                            let _ = reader_output_tx
                                .send(TerminalEvent::SessionClosed { exit_code: None });
                            break;
                        }
                        // Ignore other notifications (%layout-change, %session-changed, etc.)
                    }
                }
                Err(e) => {
                    warn!(error = %e, "tmux control mode read error");
                    let _ = reader_output_tx.send(TerminalEvent::SessionClosed { exit_code: None });
                    break;
                }
            }
        }
    });
    handles.push(reader_handle);

    // -- Writer task: batch input → send-keys --
    let writer_pane_id = pane_id;
    let writer_stdin_tx = stdin_tx.clone();
    let writer_handle = tokio_handle.spawn(async move {
        loop {
            let Ok(first) = input_rx.recv_async().await else {
                break;
            };

            let mut batch = first;
            while let Ok(more) = input_rx.try_recv() {
                batch.extend(more);
            }

            if batch.is_empty() {
                continue;
            }

            // Format as hex-encoded send-keys command.
            let mut cmd = format!("send-keys -t {writer_pane_id} -H");
            for b in &batch {
                let _ = write!(cmd, " {b:02x}");
            }
            cmd.push('\n');
            if writer_stdin_tx.send(cmd).await.is_err() {
                break;
            }
        }
    });
    handles.push(writer_handle);

    // -- Resize task --
    let resize_stdin_tx = stdin_tx.clone();
    let resize_session_name = session_name.clone();
    let resize_handle = tokio_handle.spawn(async move {
        while let Ok((cols, rows)) = resize_rx.recv_async().await {
            let cmd = format!(
                "resize-window -t {resize_session_name} -x {cols} -y {rows}\nrefresh-client -C {cols}x{rows}\n"
            );
            if resize_stdin_tx.send(cmd).await.is_err() {
                break;
            }
        }
    });
    handles.push(resize_handle);

    // -- Cleanup task: detach and kill child on shutdown --
    let cleanup_stdin_tx = stdin_tx;
    let cleanup_handle = tokio_handle.spawn(async move {
        let _ = shutdown_rx.recv_async().await;
        let _ = cleanup_stdin_tx.send("detach-client\n".to_string()).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut child = child;
        let _ = child.start_kill();
        info!(session_id = %session_id, "direct tmux cleanup complete");
    });
    handles.push(cleanup_handle);

    Ok(DirectTmuxHandle {
        input_tx,
        output_rx,
        resize_tx,
        _shutdown_tx: shutdown_tx,
        _task_handles: handles,
    })
}

// -- Internal helpers --

fn tmux_cmd() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(["-L", TMUX_SOCKET]);
    cmd
}

/// Decode tmux control mode octal escapes.
///
/// Scans for `\` followed by exactly 3 octal digits (0-7), converts to byte value.
/// All other bytes pass through unchanged.
fn decode_tmux_octal(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && (b'0'..=b'7').contains(&bytes[i + 1])
            && (b'0'..=b'7').contains(&bytes[i + 2])
            && (b'0'..=b'7').contains(&bytes[i + 3])
        {
            let d1 = u16::from(bytes[i + 1] - b'0');
            let d2 = u16::from(bytes[i + 2] - b'0');
            let d3 = u16::from(bytes[i + 3] - b'0');
            #[allow(clippy::cast_possible_truncation)]
            let val = ((d1 << 6) | (d2 << 3) | d3) as u8;
            result.push(val);
            i += 4;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    result
}
