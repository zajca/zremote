use std::io::Read;

use myremote_protocol::SessionId;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub struct PtySession {
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    reader_handle: JoinHandle<()>,
}

impl PtySession {
    /// Spawn a new PTY process. Returns `(session, pid)`.
    ///
    /// `output_tx` receives terminal output as `(SessionId, Vec<u8>)`.
    /// When the PTY reader encounters EOF or an error, it sends a zero-length
    /// vec to signal that the session has ended.
    pub fn spawn(
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
        output_tx: mpsc::Sender<(SessionId, Vec<u8>)>,
    ) -> Result<(Self, u32), Box<dyn std::error::Error + Send + Sync>> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system.openpty(size)?;

        let mut cmd = CommandBuilder::new(shell);
        if let Some(dir) = working_dir {
            cmd.cwd(dir);
        }

        let child = pair.slave.spawn_command(cmd)?;
        let pid = child.process_id().unwrap_or(0);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let reader_handle = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF -- child closed the PTY
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
                    Err(_) => {
                        // Read error -- PTY closed
                        let _ = output_tx.blocking_send((session_id, Vec::new()));
                        break;
                    }
                }
            }
        });

        let session = Self {
            writer,
            master: pair.master,
            child,
            reader_handle,
        };

        Ok((session, pid))
    }

    /// Write data to the PTY stdin.
    pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()
    }

    /// Resize the PTY terminal.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Kill the child process.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Check if the child has exited. Returns the exit code if so.
    pub fn try_wait(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.exit_code().cast_signed()),
            _ => None,
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill();
        self.reader_handle.abort();
    }
}
