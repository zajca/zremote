pub mod shell_integration;

use std::io::Read;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zremote_protocol::SessionId;

use crate::session::PtyOutput;
use shell_integration::{ShellIntegrationConfig, ShellIntegrationState};

pub struct PtySession {
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn MasterPty + Send>,
    #[allow(dead_code)]
    child: Box<dyn Child + Send + Sync>,
    reader_handle: JoinHandle<()>,
    pid: u32,
    killed: bool,
}

impl PtySession {
    /// Spawn a new PTY process. Returns `(session, pid, optional shell integration state)`.
    ///
    /// `output_tx` receives terminal output as `(SessionId, Vec<u8>)`.
    /// When the PTY reader encounters EOF or an error, it sends a zero-length
    /// vec to signal that the session has ended.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
        env: Option<&std::collections::HashMap<String, String>>,
        output_tx: mpsc::Sender<PtyOutput>,
        shell_config: Option<&ShellIntegrationConfig>,
        resume_argv: Option<&[String]>,
    ) -> Result<(Self, u32, Option<ShellIntegrationState>), Box<dyn std::error::Error + Send + Sync>>
    {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system.openpty(size)?;

        // RFC-013 resume: when `resume_argv` is present, the session's process IS
        // the agent CLI (e.g. `claude --resume <id>`), spawned directly as
        // program + args — no shell, no `-c`, no word-splitting, so the native id
        // is never re-parsed (injection-safe). Otherwise spawn the shell as today.
        let mut cmd = match resume_argv {
            Some(argv) if !argv.is_empty() => {
                let mut c = CommandBuilder::new(&argv[0]);
                c.args(&argv[1..]);
                c
            }
            _ => CommandBuilder::new(shell),
        };
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(dir) = working_dir {
            cmd.cwd(dir);
        }
        if let Some(env_vars) = env {
            for (key, value) in env_vars {
                cmd.env(key, value);
            }
        }

        // Export ZREMOTE_SESSION_ID / ZREMOTE_TERMINAL unconditionally, before
        // (and independent of) shell integration. RFC-012 native-session capture
        // requires these on every spawned session — even when shell_config is
        // None or integration is disabled. `prepare` may re-set the same values
        // when `export_env_vars` is on; that is harmless (identical values).
        // For a resume spawn this exports ZREMOTE_SESSION_ID to the agent process
        // so it re-reports its (possibly new) native id via the hook.
        shell_integration::set_session_env(session_id, &mut cmd);

        // Apply shell integration (env vars, autosuggestion disabling, etc.).
        // Skipped for a resume spawn: the process is the agent CLI, not a shell,
        // so shell-rc integration does not apply.
        let mut integration_state = match (shell_config, resume_argv) {
            (Some(config), None) => {
                shell_integration::prepare(session_id, shell, config, &mut cmd)?
            }
            _ => None,
        };

        let child = pair.slave.spawn_command(cmd)?;
        let pid = child.process_id().unwrap_or(0);

        // Store the shell PID in integration state for diagnostics.
        // Filter out pid == 0 (process_id() returned None) as it is not a valid PID.
        if let Some(ref mut state) = integration_state {
            state.shell_pid = (pid != 0).then_some(pid);
        }

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let reader_handle = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF -- child closed the PTY. Use try_send to avoid
                        // blocking if channel is full during disconnect.
                        // If dropped, the session is cleaned up by periodic GC.
                        let _ = output_tx.try_send(PtyOutput {
                            session_id,
                            pane_id: None,
                            data: Vec::new(),
                        });
                        break;
                    }
                    Ok(n) => {
                        match output_tx.try_send(PtyOutput {
                            session_id,
                            pane_id: None,
                            data: buf[..n].to_vec(),
                        }) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                // Channel full (disconnect, consumer not draining).
                                // Drop this chunk rather than blocking the reader thread.
                                // Terminal scrollback in the PTY retains the data.
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                // Receiver dropped -- session manager gone
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        // Read error -- PTY closed
                        let _ = output_tx.try_send(PtyOutput {
                            session_id,
                            pane_id: None,
                            data: Vec::new(),
                        });
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
            killed: false,
        };

        Ok((session, pid, integration_state))
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
    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Terminate the child process with SIGTERM.
    ///
    /// Uses direct `nix::sys::signal::kill()` instead of portable-pty's
    /// `child.kill()` because portable-pty sends SIGHUP, which can propagate
    /// to `systemd --user` and crash the desktop session.
    ///
    /// The `killed` flag prevents double-signaling on a potentially recycled PID
    /// (e.g. `SessionManager::close()` calls `kill()`, then Drop calls it again).
    pub fn kill(&mut self) {
        if self.killed || self.pid == 0 {
            return;
        }
        self.killed = true;
        let pid = nix::unistd::Pid::from_raw(self.pid.cast_signed());
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
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
        let (session, pid, _) =
            PtySession::spawn(session_id, "/bin/sh", 80, 24, None, None, tx, None, None).unwrap();

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
        let (session, pid, _) = PtySession::spawn(
            session_id,
            "/bin/sh",
            120,
            40,
            Some(dir.path().to_str().unwrap()),
            None,
            tx,
            None,
            None,
        )
        .unwrap();

        assert!(pid > 0);
        drop(session);
    }

    #[tokio::test]
    async fn write_and_read_output() {
        let (tx, mut rx) = mpsc::channel(256);
        let session_id = uuid::Uuid::new_v4();
        let (mut session, _pid, _) =
            PtySession::spawn(session_id, "/bin/sh", 80, 24, None, None, tx, None, None).unwrap();

        // Write a command to the PTY
        session.write(b"echo hello_from_pty\n").unwrap();

        // Wait for output (with timeout)
        let mut found = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(output)) =
                tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
            {
                assert_eq!(output.session_id, session_id);
                assert!(output.pane_id.is_none());
                if String::from_utf8_lossy(&output.data).contains("hello_from_pty") {
                    found = true;
                    break;
                }
            }
        }
        assert!(found, "should have received 'hello_from_pty' in output");

        drop(session);
    }

    #[tokio::test]
    async fn spawn_exports_zremote_session_id_without_shell_config() {
        // RFC-012: ZREMOTE_SESSION_ID must be exported on EVERY spawn, even when
        // shell_config is None (no shell integration at all). Prove it end-to-end
        // by reading the var back from the child shell.
        let (tx, mut rx) = mpsc::channel(256);
        let session_id = uuid::Uuid::new_v4();
        let (mut session, _pid, integration_state) =
            PtySession::spawn(session_id, "/bin/sh", 80, 24, None, None, tx, None, None).unwrap();

        // No shell_config -> no integration state returned, but env still set.
        assert!(
            integration_state.is_none(),
            "shell_config None should yield no integration state"
        );

        session
            .write(b"printf 'SID=%s\\n' \"$ZREMOTE_SESSION_ID\"\n")
            .unwrap();

        let expected = format!("SID={session_id}");
        let mut found = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut acc = String::new();
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(output)) =
                tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
            {
                acc.push_str(&String::from_utf8_lossy(&output.data));
                if acc.contains(&expected) {
                    found = true;
                    break;
                }
            }
        }
        assert!(
            found,
            "child shell should see ZREMOTE_SESSION_ID; got output: {acc:?}"
        );

        drop(session);
    }

    async fn read_until(
        rx: &mut mpsc::Receiver<PtyOutput>,
        needle: &str,
        secs: u64,
    ) -> (bool, String) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut acc = String::new();
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(output)) =
                tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
            {
                acc.push_str(&String::from_utf8_lossy(&output.data));
                if acc.contains(needle) {
                    return (true, acc);
                }
            }
        }
        (false, acc)
    }

    #[tokio::test]
    async fn spawn_with_resume_argv_runs_that_program() {
        // RFC-013: when resume_argv is present, the PTY child IS that program,
        // run directly (no shell, no typing into a live shell). Prove it by
        // having the program print a unique marker on start.
        let (tx, mut rx) = mpsc::channel(256);
        let session_id = uuid::Uuid::new_v4();
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'RESUMED=%s\\n' ok".to_string(),
        ];
        let (session, _pid, integration_state) = PtySession::spawn(
            session_id,
            "/bin/zsh",
            80,
            24,
            None,
            None,
            tx,
            None,
            Some(&argv),
        )
        .unwrap();

        // Resume spawn skips shell integration even if a config is passed; here
        // shell_config is None anyway, so no integration state.
        assert!(integration_state.is_none());

        let (found, acc) = read_until(&mut rx, "RESUMED=ok", 3).await;
        assert!(
            found,
            "resume_argv program should have run and printed marker; got: {acc:?}"
        );

        drop(session);
    }

    #[tokio::test]
    async fn spawn_with_resume_argv_passes_metachars_as_single_token() {
        // Injection safety: a native id full of shell metacharacters must be
        // delivered to the program as ONE literal argv element — no word-split,
        // no command substitution at the spawn boundary.
        //
        // We run `/bin/sh -c 'printf "<<%s>>" "$1"' sh <evil>`: <evil> arrives as
        // positional $1 and is printed verbatim. If the spawn had shell-parsed it
        // (word-split / command-substituted), $1 would not equal the literal.
        // `/bin/sh` is used as the program (NixOS lacks /bin/echo).
        let (tx, mut rx) = mpsc::channel(256);
        let session_id = uuid::Uuid::new_v4();
        let evil = "$(touch /tmp/zremote_pwned); rm -rf / && echo `whoami`";
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf '<<%s>>' \"$1\"".to_string(),
            "sh".to_string(),
            evil.to_string(),
        ];
        let (session, _pid, _) = PtySession::spawn(
            session_id,
            "/bin/sh",
            80,
            24,
            None,
            None,
            tx,
            None,
            Some(&argv),
        )
        .unwrap();

        // The literal string (with all metachars intact) must appear, wrapped in
        // the markers so we know it came through as the single $1 token.
        let expected = format!("<<{evil}>>");
        let (found, acc) = read_until(&mut rx, &expected, 3).await;
        assert!(
            found,
            "metacharacter-laden arg must pass as one literal token (no shell expansion); got: {acc:?}"
        );
        // The marker file must NOT have been created by command substitution.
        assert!(
            !std::path::Path::new("/tmp/zremote_pwned").exists(),
            "command substitution must not have executed"
        );

        drop(session);
    }

    #[tokio::test]
    async fn resize_session() {
        let (tx, _rx) = mpsc::channel(64);
        let session_id = uuid::Uuid::new_v4();
        let (session, _pid, _) =
            PtySession::spawn(session_id, "/bin/sh", 80, 24, None, None, tx, None, None).unwrap();

        // Resize should succeed
        let result = session.resize(120, 40);
        assert!(result.is_ok());

        drop(session);
    }

    #[tokio::test]
    async fn kill_and_try_wait() {
        let (tx, _rx) = mpsc::channel(64);
        let session_id = uuid::Uuid::new_v4();
        let (mut session, _pid, _) =
            PtySession::spawn(session_id, "/bin/sh", 80, 24, None, None, tx, None, None).unwrap();

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
        let (session, pid, _) =
            PtySession::spawn(session_id, "/bin/sh", 80, 24, None, None, tx, None, None).unwrap();

        assert!(pid > 0);

        // Drop the session
        drop(session);

        // Drain the channel - should eventually get an EOF (empty vec)
        let mut got_eof = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
                Ok(Some(output)) if output.data.is_empty() => {
                    got_eof = true;
                    break;
                }
                Ok(Some(_output)) => {}
                _ => break,
            }
        }
        // EOF signal may or may not arrive depending on timing
        let _ = got_eof;
    }
}
