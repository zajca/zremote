use std::collections::HashMap;

use myremote_protocol::SessionId;
use tokio::sync::mpsc;

use crate::pty::PtySession;

pub struct SessionManager {
    sessions: HashMap<SessionId, PtySession>,
    output_tx: mpsc::Sender<(SessionId, Vec<u8>)>,
}

impl SessionManager {
    pub fn new(output_tx: mpsc::Sender<(SessionId, Vec<u8>)>) -> Self {
        Self {
            sessions: HashMap::new(),
            output_tx,
        }
    }

    /// Spawn a new PTY session. Returns the child PID.
    pub fn create(
        &mut self,
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
    ) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        let (session, pid) = PtySession::spawn(
            session_id,
            shell,
            cols,
            rows,
            working_dir,
            self.output_tx.clone(),
        )?;
        self.sessions.insert(session_id, session);
        Ok(pid)
    }

    /// Write data to a session's PTY stdin.
    pub fn write_to(
        &mut self,
        session_id: &SessionId,
        data: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;
        session.write(data)?;
        Ok(())
    }

    /// Resize a session's PTY.
    pub fn resize(
        &self,
        session_id: &SessionId,
        cols: u16,
        rows: u16,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| format!("session {session_id} not found"))?;
        session.resize(cols, rows)
    }

    /// Close a session, killing the child process. Returns the exit code if available.
    pub fn close(&mut self, session_id: &SessionId) -> Option<i32> {
        let mut session = self.sessions.remove(session_id)?;
        session.kill();
        session.try_wait()
    }

    /// Close all sessions. Used during agent disconnect/shutdown.
    pub fn close_all(&mut self) {
        let ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for id in ids {
            self.close(&id);
        }
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
        SessionManager::new(tx)
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
}
