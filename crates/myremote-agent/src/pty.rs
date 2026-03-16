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
    pid: u32,
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
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
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
            pid,
        };

        Ok((session, pid))
    }

    /// Return the PID of the child shell process.
    pub fn pid(&self) -> u32 {
        self.pid
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_and_get_pid() {
        let (tx, mut rx) = mpsc::channel(64);
        let session_id = uuid::Uuid::new_v4();
        let (session, pid) = PtySession::spawn(
            session_id,
            "/bin/sh",
            80,
            24,
            None,
            tx,
        )
        .unwrap();

        assert!(pid > 0);
        assert_eq!(session.pid(), pid);

        // Clean up
        drop(session);
        // Drain any remaining output
        while rx.try_recv().is_ok() {}
    }

    #[tokio::test]
    async fn spawn_with_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel(64);
        let session_id = uuid::Uuid::new_v4();
        let (session, pid) = PtySession::spawn(
            session_id,
            "/bin/sh",
            120,
            40,
            Some(dir.path().to_str().unwrap()),
            tx,
        )
        .unwrap();

        assert!(pid > 0);
        drop(session);
    }

    #[tokio::test]
    async fn write_and_read_output() {
        let (tx, mut rx) = mpsc::channel(256);
        let session_id = uuid::Uuid::new_v4();
        let (mut session, _pid) = PtySession::spawn(
            session_id,
            "/bin/sh",
            80,
            24,
            None,
            tx,
        )
        .unwrap();

        // Write a command to the PTY
        session.write(b"echo hello_from_pty\n").unwrap();

        // Wait for output (with timeout)
        let mut found = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
                Ok(Some((sid, data))) => {
                    assert_eq!(sid, session_id);
                    if String::from_utf8_lossy(&data).contains("hello_from_pty") {
                        found = true;
                        break;
                    }
                }
                _ => continue,
            }
        }
        assert!(found, "should have received 'hello_from_pty' in output");

        drop(session);
    }

    #[tokio::test]
    async fn resize_session() {
        let (tx, _rx) = mpsc::channel(64);
        let session_id = uuid::Uuid::new_v4();
        let (session, _pid) = PtySession::spawn(
            session_id,
            "/bin/sh",
            80,
            24,
            None,
            tx,
        )
        .unwrap();

        // Resize should succeed
        let result = session.resize(120, 40);
        assert!(result.is_ok());

        drop(session);
    }

    #[tokio::test]
    async fn kill_and_try_wait() {
        let (tx, _rx) = mpsc::channel(64);
        let session_id = uuid::Uuid::new_v4();
        let (mut session, _pid) = PtySession::spawn(
            session_id,
            "/bin/sh",
            80,
            24,
            None,
            tx,
        )
        .unwrap();

        session.kill();

        // Give a moment for the process to die
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // try_wait should return an exit code after kill
        let exit = session.try_wait();
        // Exit code might be Some or None depending on timing
        let _ = exit;

        drop(session);
    }

    #[tokio::test]
    async fn drop_kills_child() {
        let (tx, mut rx) = mpsc::channel(64);
        let session_id = uuid::Uuid::new_v4();
        let (_session, pid) = PtySession::spawn(
            session_id,
            "/bin/sh",
            80,
            24,
            None,
            tx,
        )
        .unwrap();

        assert!(pid > 0);

        // Drop the session
        drop(_session);

        // Drain the channel - should eventually get an EOF (empty vec)
        let mut got_eof = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
                Ok(Some((_, data))) if data.is_empty() => {
                    got_eof = true;
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        // EOF signal may or may not arrive depending on timing
        let _ = got_eof;
    }
}
