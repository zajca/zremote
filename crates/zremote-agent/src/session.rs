use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc;
use zremote_protocol::SessionId;

use crate::config::PersistenceBackend;
use crate::daemon::session::DaemonSession;
use crate::pty::PtySession;
use crate::tmux::TmuxSession;

/// Terminal output from either a PTY or tmux-backed session.
///
/// For PTY sessions, `pane_id` is always `None`.
/// For tmux sessions, `pane_id` identifies which pane produced the output.
/// `None` pane_id means the main (original) pane.
#[derive(Debug)]
pub struct PtyOutput {
    pub session_id: SessionId,
    pub pane_id: Option<String>,
    pub data: Vec<u8>,
}

pub enum SessionBackend {
    Pty(PtySession),
    Tmux(TmuxSession),
    Daemon(DaemonSession),
}

/// Connection info returned to the GUI for direct tmux attachment.
pub struct DirectAttachInfo {
    pub session_name: String,
    pub pane_id: String,
}

pub struct SessionManager {
    sessions: HashMap<SessionId, SessionBackend>,
    /// Shell name cached at creation time (avoids re-deriving via sysinfo on reconnect).
    shell_names: HashMap<SessionId, String>,
    output_tx: mpsc::Sender<PtyOutput>,
    backend: PersistenceBackend,
    direct_attached: HashSet<SessionId>,
}

impl SessionManager {
    pub fn new(output_tx: mpsc::Sender<PtyOutput>, backend: PersistenceBackend) -> Self {
        Self {
            sessions: HashMap::new(),
            shell_names: HashMap::new(),
            output_tx,
            backend,
            direct_attached: HashSet::new(),
        }
    }

    /// Spawn a new session (PTY, tmux, or daemon). Returns the child PID.
    ///
    /// Daemon sessions are async (need to spawn a process and wait for socket),
    /// so this method is now async.
    pub async fn create(
        &mut self,
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
        env: Option<&std::collections::HashMap<String, String>>,
    ) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        // Extract short shell name (e.g., "/bin/zsh" → "zsh") and cache it.
        // This avoids re-deriving via sysinfo on reconnect (which can degrade to "shell").
        let shell_name = std::path::Path::new(shell)
            .file_name()
            .map_or_else(|| shell.to_string(), |n| n.to_string_lossy().into_owned());

        match self.backend {
            PersistenceBackend::Tmux => {
                let (session, pid) = TmuxSession::spawn(
                    session_id,
                    shell,
                    cols,
                    rows,
                    working_dir,
                    env,
                    self.output_tx.clone(),
                )?;
                self.sessions
                    .insert(session_id, SessionBackend::Tmux(session));
                self.shell_names.insert(session_id, shell_name);
                Ok(pid)
            }
            PersistenceBackend::Daemon => {
                let (session, pid) = DaemonSession::spawn(
                    session_id,
                    shell,
                    cols,
                    rows,
                    working_dir,
                    env,
                    self.output_tx.clone(),
                )
                .await?;
                self.sessions
                    .insert(session_id, SessionBackend::Daemon(session));
                self.shell_names.insert(session_id, shell_name);
                Ok(pid)
            }
            PersistenceBackend::None => {
                let (session, pid) = PtySession::spawn(
                    session_id,
                    shell,
                    cols,
                    rows,
                    working_dir,
                    env,
                    self.output_tx.clone(),
                )?;
                self.sessions
                    .insert(session_id, SessionBackend::Pty(session));
                self.shell_names.insert(session_id, shell_name);
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
            SessionBackend::Tmux(session) => session.write(data)?,
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
            SessionBackend::Tmux(session) => session.resize(cols, rows),
            SessionBackend::Daemon(session) => session.resize(cols, rows),
        }
    }

    /// Close a session, killing the child process. Returns the exit code if available.
    pub fn close(&mut self, session_id: &SessionId) -> Option<i32> {
        self.shell_names.remove(session_id);
        let backend = self.sessions.remove(session_id)?;
        match backend {
            SessionBackend::Pty(mut session) => {
                session.kill();
                session.try_wait()
            }
            SessionBackend::Tmux(mut session) => {
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
            if let Some(backend) = self.sessions.remove(&id) {
                match backend {
                    SessionBackend::Tmux(mut session) => session.detach(),
                    SessionBackend::Pty(mut session) => session.kill(),
                    SessionBackend::Daemon(session) => session.detach(),
                }
            }
        }
    }

    /// Discover existing sessions from a previous agent lifecycle.
    ///
    /// For Tmux backend: discovers tmux sessions.
    /// For Daemon backend: discovers running daemon processes via state files.
    /// For None (plain PTY): no persistence, returns empty.
    ///
    /// Returns a list of `(session_id, shell_name, pid, captured_content)` for
    /// recovered sessions. The captured content is scrollback data at
    /// reattach time; the caller should put it into the scrollback synchronously.
    pub async fn discover_existing(&mut self) -> Vec<(SessionId, String, u32, Option<Vec<u8>>)> {
        match self.backend {
            PersistenceBackend::None => Vec::new(),
            PersistenceBackend::Tmux => {
                // Clean up stale sessions first
                crate::tmux::cleanup_stale();

                let recovered = crate::tmux::discover_sessions(self.output_tx.clone());
                let mut result = Vec::new();

                for (session, captured) in recovered {
                    let session_id = session.session_id();
                    // Skip sessions already tracked (reconnect case) to avoid
                    // duplicate reader tasks
                    if self.sessions.contains_key(&session_id) {
                        tracing::debug!(session_id = %session_id, "skipping already-tracked tmux session");
                        continue;
                    }
                    let pid = session.pid();
                    // Get shell name from sysinfo or default to "shell"
                    let shell = get_process_name(pid);
                    self.shell_names.insert(session_id, shell.clone());
                    result.push((session_id, shell, pid, captured));
                    self.sessions
                        .insert(session_id, SessionBackend::Tmux(session));
                }

                result
            }
            PersistenceBackend::Daemon => {
                // Clean up stale daemon files first
                crate::daemon::discovery::cleanup_stale_daemons();

                let recovered =
                    crate::daemon::discovery::discover_daemon_sessions(self.output_tx.clone())
                        .await;
                let mut result = Vec::new();

                for (session, scrollback) in recovered {
                    let session_id = session.session_id();
                    // Skip sessions already tracked (reconnect case) to avoid
                    // duplicate reader tasks
                    if self.sessions.contains_key(&session_id) {
                        tracing::debug!(session_id = %session_id, "skipping already-tracked daemon session");
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
                    SessionBackend::Tmux(s) => s.pid(),
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
                SessionBackend::Tmux(s) => s.pid(),
                SessionBackend::Daemon(s) => s.pid(),
            };
            (*id, pid)
        })
    }

    /// Write to a specific pane within a tmux session.
    pub fn write_to_pane(
        &mut self,
        session_id: &SessionId,
        pane_id: &str,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let backend = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;
        match backend {
            SessionBackend::Tmux(session) => {
                session.write_to_pane(pane_id, data)?;
                Ok(())
            }
            SessionBackend::Pty(_) | SessionBackend::Daemon(_) => {
                Err("pane targeting not supported for this session type".into())
            }
        }
    }

    /// Resize a specific pane within a tmux session.
    pub fn resize_pane(
        &self,
        session_id: &SessionId,
        pane_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let backend = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;
        match backend {
            SessionBackend::Tmux(session) => session.resize_pane(pane_id, cols, rows),
            SessionBackend::Pty(_) | SessionBackend::Daemon(_) => {
                Err("pane targeting not supported for this session type".into())
            }
        }
    }

    /// Sync panes for all tmux sessions. Returns (session_id, changes) pairs.
    pub fn sync_all_panes(&mut self) -> Vec<(SessionId, Vec<crate::tmux::PaneChange>)> {
        let mut results = Vec::new();
        for (session_id, backend) in &mut self.sessions {
            if let SessionBackend::Tmux(session) = backend {
                let changes = session.sync_panes();
                if !changes.is_empty() {
                    results.push((*session_id, changes));
                }
            }
        }
        results
    }

    /// Whether tmux-backed sessions are enabled.
    pub fn use_tmux(&self) -> bool {
        self.backend == PersistenceBackend::Tmux
    }

    /// Whether any persistent backend (tmux or daemon) is active.
    pub fn supports_persistence(&self) -> bool {
        self.backend != PersistenceBackend::None
    }

    /// Return the active persistence backend.
    pub fn backend(&self) -> PersistenceBackend {
        self.backend
    }

    /// Detach agent's FIFO reader, allowing GUI to connect directly to tmux.
    /// Returns connection info for the GUI.
    pub fn detach_for_direct(
        &mut self,
        session_id: &SessionId,
    ) -> Result<DirectAttachInfo, Box<dyn std::error::Error + Send + Sync>> {
        let backend = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;

        let tmux_session = match backend {
            SessionBackend::Tmux(session) => session,
            SessionBackend::Pty(_) | SessionBackend::Daemon(_) => {
                return Err("direct attach only supported for tmux sessions".into());
            }
        };

        let info = DirectAttachInfo {
            session_name: tmux_session.tmux_name().to_string(),
            pane_id: tmux_session.pane_id().to_string(),
        };

        // Detach agent's reader (stops pipe-pane, removes FIFO, aborts reader)
        tmux_session.detach();

        self.direct_attached.insert(*session_id);

        tracing::info!(
            session_id = %session_id,
            session_name = %info.session_name,
            "detached for direct GUI connection"
        );

        Ok(info)
    }

    /// Re-attach agent's FIFO reader after GUI disconnects from direct connection.
    pub fn reattach_after_direct(
        &mut self,
        session_id: &SessionId,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.direct_attached.remove(session_id) {
            return Err(format!("session {session_id} is not directly attached").into());
        }

        let backend = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;

        let tmux_session = match backend {
            SessionBackend::Tmux(session) => session,
            SessionBackend::Pty(_) | SessionBackend::Daemon(_) => {
                return Err("direct detach only supported for tmux sessions".into());
            }
        };

        tmux_session.reattach_reader(self.output_tx.clone())?;

        tracing::info!(
            session_id = %session_id,
            "re-attached after direct GUI connection released"
        );

        Ok(())
    }

    /// Check if a session is currently directly attached by the GUI.
    pub fn is_direct_attached(&self, session_id: &SessionId) -> bool {
        self.direct_attached.contains(session_id)
    }

    /// Find sessions that are marked as directly attached but whose GUI FIFO
    /// no longer exists (GUI crashed without calling direct-detach).
    pub fn find_orphaned_direct_attached(&self) -> Vec<SessionId> {
        let fifo_dir = crate::tmux::fifo_dir();
        self.direct_attached
            .iter()
            .filter(|session_id| {
                let gui_fifo = fifo_dir.join(format!("{session_id}-gui.fifo"));
                !gui_fifo.exists()
            })
            .copied()
            .collect()
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
    fn use_tmux_returns_configured_value() {
        let (tx, _rx) = mpsc::channel(64);
        let mgr_none = SessionManager::new(tx.clone(), PersistenceBackend::None);
        assert!(!mgr_none.use_tmux());

        let mgr_tmux = SessionManager::new(tx.clone(), PersistenceBackend::Tmux);
        assert!(mgr_tmux.use_tmux());

        let mgr_daemon = SessionManager::new(tx, PersistenceBackend::Daemon);
        assert!(!mgr_daemon.use_tmux());
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
        assert!(!mgr.use_tmux());
        let result1 = mgr.discover_existing().await;
        assert!(result1.is_empty());
        let result2 = mgr.discover_existing().await;
        assert!(result2.is_empty());
    }

    #[test]
    fn new_manager_with_tmux_backend() {
        let (tx, _rx) = mpsc::channel(64);
        let mgr = SessionManager::new(tx, PersistenceBackend::Tmux);
        assert!(mgr.use_tmux());
        assert_eq!(mgr.count(), 0);
    }

    #[test]
    fn new_manager_with_daemon_backend() {
        let (tx, _rx) = mpsc::channel(64);
        let mgr = SessionManager::new(tx, PersistenceBackend::Daemon);
        assert!(!mgr.use_tmux());
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

    #[test]
    fn detach_for_direct_nonexistent_session() {
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr.detach_for_direct(&session_id);
        assert!(result.is_err());
    }

    #[test]
    fn reattach_after_direct_not_attached() {
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr.reattach_after_direct(&session_id);
        assert!(result.is_err());
    }

    #[test]
    fn is_direct_attached_false_by_default() {
        let mgr = make_manager();
        let session_id = Uuid::new_v4();
        assert!(!mgr.is_direct_attached(&session_id));
    }

    #[test]
    fn find_orphaned_empty_when_no_direct() {
        let mgr = make_manager();
        let orphans = mgr.find_orphaned_direct_attached();
        assert!(orphans.is_empty());
    }

    #[test]
    fn write_to_pane_nonexistent_session() {
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr.write_to_pane(&session_id, "%0", b"test");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains(&session_id.to_string()),
            "error should contain session id, got: {err}"
        );
    }

    #[test]
    fn resize_pane_nonexistent_session() {
        let mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr.resize_pane(&session_id, "%0", 120, 40);
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains(&session_id.to_string()),
            "error should contain session id, got: {err}"
        );
    }

    #[test]
    fn sync_all_panes_empty_manager() {
        let mut mgr = make_manager();
        let results = mgr.sync_all_panes();
        assert!(results.is_empty());
    }
}
