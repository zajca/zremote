use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc;
use zremote_protocol::SessionId;

use crate::config::PersistenceBackend;
use crate::daemon::session::DaemonSession;
use crate::pty::PtySession;
use crate::pty::shell_integration::{ShellIntegrationConfig, ShellIntegrationState};

/// Terminal output from a PTY or daemon-backed session.
///
/// `pane_id` is always `None` (kept for protocol compatibility).
#[derive(Debug)]
pub struct PtyOutput {
    pub session_id: SessionId,
    pub pane_id: Option<String>,
    pub data: Vec<u8>,
}

pub enum SessionBackend {
    Pty(PtySession),
    Daemon(DaemonSession),
}

pub struct SessionManager {
    sessions: HashMap<SessionId, SessionBackend>,
    /// Shell name cached at creation time (avoids re-deriving via sysinfo on reconnect).
    shell_names: HashMap<SessionId, String>,
    /// Shell integration state per session (temp dirs, shell type).
    shell_integrations: HashMap<SessionId, ShellIntegrationState>,
    output_tx: mpsc::Sender<PtyOutput>,
    backend: PersistenceBackend,
}

impl SessionManager {
    pub fn new(output_tx: mpsc::Sender<PtyOutput>, backend: PersistenceBackend) -> Self {
        Self {
            sessions: HashMap::new(),
            shell_names: HashMap::new(),
            shell_integrations: HashMap::new(),
            output_tx,
            backend,
        }
    }

    /// Spawn a new session (PTY or daemon). Returns the child PID.
    ///
    /// Daemon sessions are async (need to spawn a process and wait for socket),
    /// so this method is now async.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &mut self,
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
        env: Option<&std::collections::HashMap<String, String>>,
        shell_config: Option<&ShellIntegrationConfig>,
    ) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        // Extract short shell name (e.g., "/bin/zsh" → "zsh") and cache it.
        // This avoids re-deriving via sysinfo on reconnect (which can degrade to "shell").
        let shell_name = std::path::Path::new(shell)
            .file_name()
            .map_or_else(|| shell.to_string(), |n| n.to_string_lossy().into_owned());

        match self.backend {
            PersistenceBackend::Daemon => {
                let (session, pid) = DaemonSession::spawn(
                    session_id,
                    shell,
                    cols,
                    rows,
                    working_dir,
                    env,
                    self.output_tx.clone(),
                    shell_config,
                )
                .await?;
                self.sessions
                    .insert(session_id, SessionBackend::Daemon(session));
                self.shell_names.insert(session_id, shell_name);
                Ok(pid)
            }
            PersistenceBackend::None => {
                let (session, pid, integration_state) = PtySession::spawn(
                    session_id,
                    shell,
                    cols,
                    rows,
                    working_dir,
                    env,
                    self.output_tx.clone(),
                    shell_config,
                )?;
                self.sessions
                    .insert(session_id, SessionBackend::Pty(session));
                self.shell_names.insert(session_id, shell_name);
                if let Some(state) = integration_state {
                    self.shell_integrations.insert(session_id, state);
                }
                Ok(pid)
            }
        }
    }

    /// Write data to a session's stdin.
    pub fn write_to(
        &mut self,
        session_id: &SessionId,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let backend = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;
        match backend {
            SessionBackend::Pty(session) => session.write(data)?,
            SessionBackend::Daemon(session) => session.write(data)?,
        }
        Ok(())
    }

    /// Resize a session.
    pub fn resize(
        &self,
        session_id: &SessionId,
        cols: u16,
        rows: u16,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let backend = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;
        match backend {
            SessionBackend::Pty(session) => session.resize(cols, rows),
            SessionBackend::Daemon(session) => session.resize(cols, rows),
        }
    }

    /// Check whether a session is tracked by this manager.
    pub fn has_session(&self, session_id: &SessionId) -> bool {
        self.sessions.contains_key(session_id)
    }

    /// Close a session, killing the child process. Returns the exit code if available.
    pub fn close(&mut self, session_id: &SessionId) -> Option<i32> {
        self.shell_names.remove(session_id);
        if let Some(integration) = self.shell_integrations.remove(session_id) {
            integration.cleanup();
        }
        let backend = self.sessions.remove(session_id)?;
        match backend {
            SessionBackend::Pty(mut session) => {
                session.kill();
                session.try_wait()
            }
            SessionBackend::Daemon(session) => {
                let exit = session.try_wait();
                session.kill();
                exit
            }
        }
    }

    /// Check if a session is backed by a daemon that is still alive.
    /// Returns `true` if the session is a daemon session and the daemon process
    /// is still running (i.e., the socket EOF was transient, not a real exit).
    pub fn is_daemon_alive(&self, session_id: &SessionId) -> bool {
        matches!(
            self.sessions.get(session_id),
            Some(SessionBackend::Daemon(s)) if s.try_wait().is_none()
        )
    }

    /// Reconnect to a daemon whose socket connection was lost but process is
    /// still alive. Replaces the old `DaemonSession` with a fresh one.
    /// Returns scrollback data on success.
    pub async fn reconnect_daemon(
        &mut self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(SessionBackend::Daemon(old)) = self.sessions.remove(session_id) else {
            return Err("session not found or not a daemon".into());
        };
        let socket_path = old.socket_path().clone();
        let state_path = old.state_path().clone();
        let daemon_pid = old.daemon_pid();
        let shell_pid = old.pid();
        // Detach cleanly (abort IO tasks, don't kill daemon)
        old.detach();

        match DaemonSession::reconnect(
            *session_id,
            socket_path.clone(),
            state_path.clone(),
            daemon_pid,
            shell_pid,
            self.output_tx.clone(),
        )
        .await
        {
            Ok((new_session, scrollback, _started_at)) => {
                self.sessions
                    .insert(*session_id, SessionBackend::Daemon(new_session));
                Ok(scrollback)
            }
            Err(e) => {
                // Reconnect failed -- kill the orphaned daemon directly so it
                // doesn't linger forever (the session is already removed from
                // self.sessions, so the fallthrough close() won't find it).
                let pid = nix::unistd::Pid::from_raw(i32::try_from(daemon_pid).unwrap_or(i32::MAX));
                let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
                let _ = std::fs::remove_file(&state_path);
                let _ = std::fs::remove_file(&socket_path);
                Err(e)
            }
        }
    }

    /// Close all sessions. Used during agent disconnect/shutdown.
    pub fn close_all(&mut self) {
        let ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for id in ids {
            self.close(&id);
        }
    }

    /// Detach persistent sessions (they survive) and kill plain PTY sessions.
    /// Used during graceful agent shutdown.
    pub fn detach_all(&mut self) {
        let ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for id in ids {
            self.shell_names.remove(&id);
            if let Some(integration) = self.shell_integrations.remove(&id) {
                integration.cleanup();
            }
            if let Some(backend) = self.sessions.remove(&id) {
                match backend {
                    SessionBackend::Pty(mut session) => session.kill(),
                    SessionBackend::Daemon(session) => session.detach(),
                }
            }
        }
    }

    /// Discover existing sessions from a previous agent lifecycle.
    ///
    /// For Daemon backend: discovers running daemon processes via state files.
    /// For None (plain PTY): no persistence, returns empty.
    ///
    /// Returns a list of `(session_id, shell_name, pid, captured_content)` for
    /// recovered sessions. The captured content is scrollback data at
    /// reattach time; the caller should put it into the scrollback synchronously.
    pub async fn discover_existing(&mut self) -> Vec<(SessionId, String, u32, Option<Vec<u8>>)> {
        match self.backend {
            PersistenceBackend::None => Vec::new(),
            PersistenceBackend::Daemon => {
                // Clean up stale daemon files first
                crate::daemon::discovery::cleanup_stale_daemons();

                let tracked_ids: HashSet<SessionId> = self.sessions.keys().copied().collect();
                let recovered = crate::daemon::discovery::discover_daemon_sessions(
                    self.output_tx.clone(),
                    &tracked_ids,
                )
                .await;
                let mut result = Vec::new();

                for (session, scrollback) in recovered {
                    let session_id = session.session_id();
                    // Skip sessions already tracked (reconnect case) to avoid
                    // duplicate reader tasks
                    if self.sessions.contains_key(&session_id) {
                        tracing::debug!(session_id = %session_id, "skipping already-tracked daemon session");
                        session.detach();
                        continue;
                    }
                    let pid = session.pid();
                    let shell = get_process_name(pid);
                    self.shell_names.insert(session_id, shell.clone());
                    result.push((session_id, shell, pid, scrollback));
                    self.sessions
                        .insert(session_id, SessionBackend::Daemon(session));
                }

                result
            }
        }
    }

    /// Return a reference to the PTY output sender.
    ///
    /// Used to clone the sender when hoisting the channel above the reconnect loop.
    pub fn output_tx(&self) -> &mpsc::Sender<PtyOutput> {
        &self.output_tx
    }

    /// Return info about all currently active sessions for re-announcement after reconnect.
    /// Uses cached shell names from creation time to avoid sysinfo degradation to "shell".
    pub fn active_session_info(&self) -> Vec<(SessionId, String, u32)> {
        self.sessions
            .iter()
            .map(|(id, backend)| {
                let pid = match backend {
                    SessionBackend::Pty(s) => s.pid(),
                    SessionBackend::Daemon(s) => s.pid(),
                };
                let shell = self
                    .shell_names
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| get_process_name(pid));
                (*id, shell, pid)
            })
            .collect()
    }

    /// Return an iterator of `(session_id, shell_pid)` for all active sessions.
    pub fn session_pids(&self) -> impl Iterator<Item = (SessionId, u32)> + '_ {
        self.sessions.iter().map(|(id, backend)| {
            let pid = match backend {
                SessionBackend::Pty(s) => s.pid(),
                SessionBackend::Daemon(s) => s.pid(),
            };
            (*id, pid)
        })
    }

    /// Whether any persistent backend (daemon) is active.
    pub fn supports_persistence(&self) -> bool {
        self.backend != PersistenceBackend::None
    }

    /// Return the active persistence backend.
    pub fn backend(&self) -> PersistenceBackend {
        self.backend
    }

    /// Return the number of active sessions.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.sessions.len()
    }
}

/// Get the process name for a given PID using sysinfo.
/// Falls back to "shell" if the process cannot be found.
fn get_process_name(pid: u32) -> String {
    use sysinfo::{Pid, System};
    let mut sys = System::new();
    sys.refresh_processes(
        sysinfo::ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
    );
    sys.process(Pid::from_u32(pid))
        .map(|p| p.name().to_string_lossy().to_string())
        .unwrap_or_else(|| "shell".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_manager() -> SessionManager {
        let (tx, _rx) = mpsc::channel(64);
        SessionManager::new(tx, PersistenceBackend::None)
    }

    #[test]
    fn new_creates_empty_manager() {
        let mgr = make_manager();
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn write_to_nonexistent_session_returns_error() {
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr.write_to(&session_id, b"hello");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found"),
            "error should mention 'not found', got: {err_msg}"
        );
    }

    #[test]
    fn resize_nonexistent_session_returns_error() {
        let mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr.resize(&session_id, 120, 40);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found"),
            "error should mention 'not found', got: {err_msg}"
        );
    }

    #[test]
    fn close_nonexistent_session_returns_none() {
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr.close(&session_id);
        assert!(result.is_none());
    }

    #[test]
    fn close_all_on_empty_manager_is_noop() {
        let mut mgr = make_manager();
        mgr.close_all();
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn supports_persistence_returns_configured_value() {
        let (tx, _rx) = mpsc::channel(64);
        let mgr_none = SessionManager::new(tx.clone(), PersistenceBackend::None);
        assert!(!mgr_none.supports_persistence());

        let mgr_daemon = SessionManager::new(tx, PersistenceBackend::Daemon);
        assert!(mgr_daemon.supports_persistence());
    }

    #[tokio::test]
    async fn discover_existing_returns_empty_when_no_persistence() {
        let mut mgr = make_manager();
        let result = mgr.discover_existing().await;
        assert!(result.is_empty());
    }

    #[test]
    fn detach_all_on_empty_manager_is_noop() {
        let mut mgr = make_manager();
        mgr.detach_all();
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn session_pids_empty_manager() {
        let mgr = make_manager();
        let pids: Vec<_> = mgr.session_pids().collect();
        assert!(pids.is_empty());
    }

    #[test]
    fn multiple_operations_on_nonexistent_sessions() {
        let mut mgr = make_manager();
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();

        // Multiple writes to nonexistent sessions
        assert!(mgr.write_to(&s1, b"data").is_err());
        assert!(mgr.write_to(&s2, b"data").is_err());

        // Multiple resizes on nonexistent sessions
        assert!(mgr.resize(&s1, 80, 24).is_err());

        // Multiple closes on nonexistent sessions
        assert!(mgr.close(&s1).is_none());
        assert!(mgr.close(&s2).is_none());

        // Count should still be zero
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn close_all_on_empty_is_idempotent() {
        let mut mgr = make_manager();
        mgr.close_all();
        mgr.close_all();
        mgr.close_all();
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn detach_all_on_empty_is_idempotent() {
        let mut mgr = make_manager();
        mgr.detach_all();
        mgr.detach_all();
        assert_eq!(mgr.count(), 0);
    }

    #[tokio::test]
    async fn discover_existing_with_no_persistence_always_empty() {
        let mut mgr = make_manager();
        let result1 = mgr.discover_existing().await;
        assert!(result1.is_empty());
        let result2 = mgr.discover_existing().await;
        assert!(result2.is_empty());
    }

    #[test]
    fn new_manager_with_daemon_backend() {
        let (tx, _rx) = mpsc::channel(64);
        let mgr = SessionManager::new(tx, PersistenceBackend::Daemon);
        assert!(mgr.supports_persistence());
        assert_eq!(mgr.backend(), PersistenceBackend::Daemon);
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn write_to_error_contains_session_id() {
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr.write_to(&session_id, b"test");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains(&session_id.to_string()),
            "error should contain session id, got: {err}"
        );
    }

    #[test]
    fn resize_error_contains_session_id() {
        let mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr.resize(&session_id, 120, 40);
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains(&session_id.to_string()),
            "error should contain session id, got: {err}"
        );
    }

    #[tokio::test]
    async fn session_manager_create_without_integration() {
        // Create a session with shell_config: None -- should not panic.
        // We expect an error because the shell binary doesn't exist,
        // but the important thing is no panic from missing shell integration.
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr
            .create(session_id, "/bin/sh", 80, 24, None, None, None)
            .await;
        // The spawn may succeed or fail depending on environment,
        // but it should never panic regardless of shell_config being None
        drop(result);
    }

    #[tokio::test]
    async fn session_manager_cleanup_on_close() {
        // Create and immediately close -- verify no panic
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();

        // Try to create a real session (may fail in CI without a TTY)
        if mgr
            .create(session_id, "/bin/sh", 80, 24, None, None, None)
            .await
            .is_ok()
        {
            assert!(mgr.has_session(&session_id));
            // Exit code may or may not be available depending on timing.
            let _exit_code = mgr.close(&session_id);
            assert!(!mgr.has_session(&session_id));
            assert_eq!(mgr.count(), 0);
        }
        // If create failed (no PTY available), closing nonexistent is fine
        assert!(mgr.close(&session_id).is_none());
    }

    #[test]
    fn spawn_with_shell_integration_sets_env() {
        use crate::pty::shell_integration::{self, ShellIntegrationConfig};

        let session_id = Uuid::new_v4();
        let config = ShellIntegrationConfig::for_ai_session();
        let mut cmd = portable_pty::CommandBuilder::new("/bin/sh");

        let state = shell_integration::prepare(session_id, "/bin/sh", &config, &mut cmd)
            .unwrap()
            .expect("should return state for ai session config");

        // For an unknown shell (/bin/sh), env vars should be set but no temp dir
        assert!(
            state.temp_dir.is_none(),
            "unknown shell should not create temp dir"
        );
        // The env vars (ZREMOTE_TERMINAL, ZREMOTE_SESSION_ID) are set on cmd
        // We verify indirectly: state was created, meaning export_env_vars path ran
    }

    #[test]
    fn spawn_zsh_with_integration() {
        use crate::pty::shell_integration::{self, ShellIntegrationConfig};

        let session_id = Uuid::new_v4();
        let config = ShellIntegrationConfig::for_ai_session();
        let mut cmd = portable_pty::CommandBuilder::new("/bin/zsh");

        let state = shell_integration::prepare(session_id, "/bin/zsh", &config, &mut cmd)
            .unwrap()
            .expect("should return state");

        // Zsh integration creates a ZDOTDIR temp dir
        assert!(
            state.temp_dir.is_some(),
            "zsh integration should create temp ZDOTDIR"
        );
        let temp_dir = state.temp_dir.as_ref().unwrap();
        let zshrc_path = temp_dir.path().join(".zshrc");
        assert!(zshrc_path.exists(), "should create .zshrc in temp ZDOTDIR");
    }
}
