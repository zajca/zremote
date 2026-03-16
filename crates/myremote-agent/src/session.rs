use std::collections::HashMap;

use myremote_protocol::SessionId;
use tokio::sync::mpsc;

use crate::pty::PtySession;
use crate::tmux::TmuxSession;

pub enum SessionBackend {
    Pty(PtySession),
    Tmux(TmuxSession),
}

pub struct SessionManager {
    sessions: HashMap<SessionId, SessionBackend>,
    output_tx: mpsc::Sender<(SessionId, Vec<u8>)>,
    use_tmux: bool,
}

impl SessionManager {
    pub fn new(output_tx: mpsc::Sender<(SessionId, Vec<u8>)>, use_tmux: bool) -> Self {
        Self {
            sessions: HashMap::new(),
            output_tx,
            use_tmux,
        }
    }

    /// Spawn a new session (PTY or tmux). Returns the child PID.
    pub fn create(
        &mut self,
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
    ) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        if self.use_tmux {
            let (session, pid) = TmuxSession::spawn(
                session_id,
                shell,
                cols,
                rows,
                working_dir,
                self.output_tx.clone(),
            )?;
            self.sessions.insert(session_id, SessionBackend::Tmux(session));
            Ok(pid)
        } else {
            let (session, pid) = PtySession::spawn(
                session_id,
                shell,
                cols,
                rows,
                working_dir,
                self.output_tx.clone(),
            )?;
            self.sessions.insert(session_id, SessionBackend::Pty(session));
            Ok(pid)
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
        }
    }

    /// Close a session, killing the child process. Returns the exit code if available.
    pub fn close(&mut self, session_id: &SessionId) -> Option<i32> {
        let mut backend = self.sessions.remove(session_id)?;
        match &mut backend {
            SessionBackend::Pty(session) => {
                session.kill();
                session.try_wait()
            }
            SessionBackend::Tmux(session) => {
                session.kill();
                session.try_wait()
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

    /// Detach tmux sessions (they survive) and kill PTY sessions.
    /// Used during graceful agent shutdown when tmux is enabled.
    pub fn detach_all(&mut self) {
        let ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for id in ids {
            if let Some(backend) = self.sessions.remove(&id) {
                match backend {
                    SessionBackend::Tmux(mut session) => session.detach(),
                    SessionBackend::Pty(mut session) => session.kill(),
                }
            }
        }
    }

    /// Discover existing tmux sessions from a previous agent lifecycle.
    /// Returns a list of (session_id, shell_name, pid) for recovered sessions.
    pub fn discover_existing(&mut self) -> Vec<(SessionId, String, u32)> {
        if !self.use_tmux {
            return Vec::new();
        }

        // Clean up stale sessions first
        crate::tmux::cleanup_stale();

        let recovered = crate::tmux::discover_sessions(self.output_tx.clone());
        let mut result = Vec::new();

        for session in recovered {
            let session_id = session.session_id();
            let pid = session.pid();
            // Get shell from /proc/{pid}/comm or default to "shell"
            let shell = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "shell".to_string());
            result.push((session_id, shell, pid));
            self.sessions.insert(session_id, SessionBackend::Tmux(session));
        }

        result
    }

    /// Return an iterator of `(session_id, shell_pid)` for all active sessions.
    pub fn session_pids(&self) -> impl Iterator<Item = (SessionId, u32)> + '_ {
        self.sessions.iter().map(|(id, backend)| {
            let pid = match backend {
                SessionBackend::Pty(s) => s.pid(),
                SessionBackend::Tmux(s) => s.pid(),
            };
            (*id, pid)
        })
    }

    /// Whether tmux-backed sessions are enabled.
    pub fn use_tmux(&self) -> bool {
        self.use_tmux
    }

    /// Return the number of active sessions.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_manager() -> SessionManager {
        let (tx, _rx) = mpsc::channel(64);
        SessionManager::new(tx, false)
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
        let mgr_false = SessionManager::new(tx.clone(), false);
        assert!(!mgr_false.use_tmux());

        let mgr_true = SessionManager::new(tx, true);
        assert!(mgr_true.use_tmux());
    }

    #[test]
    fn discover_existing_returns_empty_when_tmux_disabled() {
        let mut mgr = make_manager();
        let result = mgr.discover_existing();
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

    #[test]
    fn discover_existing_with_tmux_false_always_empty() {
        let mut mgr = make_manager();
        assert!(!mgr.use_tmux());
        let result1 = mgr.discover_existing();
        assert!(result1.is_empty());
        let result2 = mgr.discover_existing();
        assert!(result2.is_empty());
    }

    #[test]
    fn new_manager_with_tmux_true() {
        let (tx, _rx) = mpsc::channel(64);
        let mgr = SessionManager::new(tx, true);
        assert!(mgr.use_tmux());
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
}
