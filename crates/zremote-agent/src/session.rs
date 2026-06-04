use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

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
    /// Scoped socket directory for daemon sessions.
    socket_dir: PathBuf,
    /// Per-process instance UUID, used as owner marker in daemon state files
    /// so that multiple agents sharing the same socket directory do not steal
    /// each other's sessions.
    agent_instance_id: uuid::Uuid,
}

impl SessionManager {
    pub fn new(
        output_tx: mpsc::Sender<PtyOutput>,
        backend: PersistenceBackend,
        socket_dir: PathBuf,
        agent_instance_id: uuid::Uuid,
    ) -> Self {
        Self {
            sessions: HashMap::new(),
            shell_names: HashMap::new(),
            shell_integrations: HashMap::new(),
            output_tx,
            backend,
            socket_dir,
            agent_instance_id,
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
        self.create_inner(
            session_id,
            shell,
            cols,
            rows,
            working_dir,
            env,
            shell_config,
            None,
        )
        .await
    }

    /// Shared spawn path for `create` and `resume_session`. When `resume_argv`
    /// is `Some`, the spawned session's process is that argv directly (RFC-013
    /// resume) instead of a bare shell; otherwise it spawns the shell as usual.
    #[allow(clippy::too_many_arguments)]
    async fn create_inner(
        &mut self,
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
        env: Option<&std::collections::HashMap<String, String>>,
        shell_config: Option<&ShellIntegrationConfig>,
        resume_argv: Option<&[String]>,
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
                    &self.socket_dir,
                    Some(&self.agent_instance_id.to_string()),
                    resume_argv,
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
                    resume_argv,
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

    /// Resume a stopped agent session (RFC-013) by re-spawning a backend for the
    /// **same** `session_id` with `resume_argv` (e.g. `claude --resume <id>`) as
    /// the session's process. Deterministic — the agent IS the session command,
    /// so there is no shell-readiness race and no "type into a live shell".
    ///
    /// Double-launch guard: if a live daemon for this id already exists, this is
    /// a no-op attach (returns the existing pid) and the resume command is NOT
    /// run a second time. Stale daemon state files are cleaned up first.
    #[allow(clippy::too_many_arguments)]
    pub async fn resume_session(
        &mut self,
        session_id: SessionId,
        shell: &str,
        cols: u16,
        rows: u16,
        working_dir: Option<&str>,
        env: Option<&std::collections::HashMap<String, String>>,
        shell_config: Option<&ShellIntegrationConfig>,
        resume_argv: &[String],
    ) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        if resume_argv.is_empty() {
            return Err("resume_argv must not be empty".into());
        }

        // Defensive: a stale state file may linger after a reboot. Recreation
        // overwrites it, but clear known-dead entries first so discovery doesn't
        // re-adopt a corpse.
        if self.backend != PersistenceBackend::None {
            crate::daemon::discovery::cleanup_stale_daemons(&self.socket_dir);
        }

        // Double-launch guard: if ANY backend for this id is already tracked,
        // attach instead of relaunching so the resume command runs exactly once.
        // Guard on `has_session` (covers BOTH Daemon and Pty backends) rather
        // than `is_daemon_alive` (Daemon-only): with PersistenceBackend::None the
        // backend is a Pty, so a daemon-only guard would never fire and two
        // concurrent resume callers (REST + attach) could spawn duplicate agents.
        //
        // Atomicity: callers hold the `SessionManager` lock for the whole
        // `resume_session` call, so this guard check and the `create_inner` spawn
        // below happen under ONE continuous lock hold — a concurrent REST+WS
        // resume cannot both pass the guard and double-spawn. Callers must NOT do
        // a separate has_session/is_daemon_alive pre-check outside the lock.
        if self.has_session(&session_id) {
            // Sentinel for "tracked but pid unknown" — `u32::MAX`, never `0`
            // (POSIX pid 0 means the caller's process group; misleading even
            // though this value is never passed to kill()).
            let pid = self
                .session_pids()
                .find_map(|(id, pid)| (id == session_id).then_some(pid))
                .unwrap_or(u32::MAX);
            tracing::info!(
                %session_id,
                "resume requested but a backend for this id already exists; attaching instead of relaunching"
            );
            return Ok(pid);
        }

        self.create_inner(
            session_id,
            shell,
            cols,
            rows,
            working_dir,
            env,
            shell_config,
            Some(resume_argv),
        )
        .await
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
                crate::daemon::discovery::cleanup_stale_daemons(&self.socket_dir);

                let tracked_ids: HashSet<SessionId> = self.sessions.keys().copied().collect();
                let owner_id_str = self.agent_instance_id.to_string();
                let recovered = crate::daemon::discovery::discover_daemon_sessions(
                    self.output_tx.clone(),
                    &tracked_ids,
                    &self.socket_dir,
                    Some(&owner_id_str),
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

/// Map a persisted `sessions.agent_kind` string back to an [`AgentKind`].
/// Mirrors the `snake_case` serde representation (see RFC-012 capture).
fn agent_kind_from_db(kind: &str) -> zremote_protocol::AgentKind {
    match kind {
        "claude" => zremote_protocol::AgentKind::Claude,
        "codex" => zremote_protocol::AgentKind::Codex,
        _ => zremote_protocol::AgentKind::Unknown,
    }
}

/// Build the resume argv for a session from its persisted agent identity
/// (RFC-013). Reads `agent_kind` + `agent_session_ref` (RFC-012) and turns them
/// into the agent's native resume command via [`crate::agents::resume_argv`].
///
/// Returns `Ok(None)` when the session has no resumable agent ref or the agent
/// kind has no known resume command (e.g. `Unknown`). The resulting argv is then
/// passed to [`SessionManager::resume_session`], which spawns it directly as the
/// session's process — the native id stays a single argv token (injection-safe).
pub async fn build_resume_argv_for_session(
    pool: &sqlx::SqlitePool,
    session_id: &str,
) -> Result<Option<Vec<String>>, zremote_core::error::AppError> {
    let Some((kind, native_id)) =
        zremote_core::queries::sessions::get_agent_session_ref(pool, session_id).await?
    else {
        return Ok(None);
    };
    Ok(crate::agents::resume_argv(
        agent_kind_from_db(&kind),
        &native_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_manager() -> SessionManager {
        let (tx, _rx) = mpsc::channel(64);
        SessionManager::new(
            tx,
            PersistenceBackend::None,
            PathBuf::from("/tmp/zremote-test"),
            uuid::Uuid::new_v4(),
        )
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
        let mgr_none = SessionManager::new(
            tx.clone(),
            PersistenceBackend::None,
            PathBuf::from("/tmp/zremote-test"),
            uuid::Uuid::new_v4(),
        );
        assert!(!mgr_none.supports_persistence());

        let mgr_daemon = SessionManager::new(
            tx,
            PersistenceBackend::Daemon,
            PathBuf::from("/tmp/zremote-test"),
            uuid::Uuid::new_v4(),
        );
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
        let mgr = SessionManager::new(
            tx,
            PersistenceBackend::Daemon,
            PathBuf::from("/tmp/zremote-test"),
            uuid::Uuid::new_v4(),
        );
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

    #[tokio::test]
    async fn resume_session_rejects_empty_argv() {
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();
        let result = mgr
            .resume_session(session_id, "/bin/sh", 80, 24, None, None, None, &[])
            .await;
        assert!(result.is_err(), "empty resume_argv must be rejected");
        assert!(!mgr.has_session(&session_id));
    }

    #[tokio::test]
    async fn resume_session_spawns_session_for_same_id() {
        // None backend (PTY). resume_session re-creates a backend for the given
        // id with the resume argv as the process. Reuses the same session id.
        let mut mgr = make_manager();
        let session_id = Uuid::new_v4();
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 1".to_string(),
        ];
        if mgr
            .resume_session(session_id, "/bin/sh", 80, 24, None, None, None, &argv)
            .await
            .is_ok()
        {
            assert!(
                mgr.has_session(&session_id),
                "resumed session must be tracked under the same id"
            );
            let _ = mgr.close(&session_id);
            assert!(!mgr.has_session(&session_id));
        }
    }

    #[tokio::test]
    async fn resume_session_does_not_double_spawn_on_pty_backend() {
        // Regression (review finding): the double-launch guard must cover the
        // None/Pty backend, not just Daemon. A second resume_session for an
        // already-tracked id must NOT create a second backend — it attaches.
        let mut mgr = make_manager(); // None backend -> Pty
        let session_id = Uuid::new_v4();
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 5".to_string(),
        ];

        // First resume spawns the backend. If the env can't spawn a PTY at all,
        // skip (nothing to guard against).
        if mgr
            .resume_session(session_id, "/bin/sh", 80, 24, None, None, None, &argv)
            .await
            .is_err()
        {
            return;
        }
        assert!(mgr.has_session(&session_id));
        assert_eq!(mgr.count(), 1);

        // Second resume for the SAME id must be a no-op attach: still exactly one
        // backend, no duplicate process.
        mgr.resume_session(session_id, "/bin/sh", 80, 24, None, None, None, &argv)
            .await
            .expect("second resume should attach, not error");
        assert_eq!(
            mgr.count(),
            1,
            "second resume must not spawn a second backend for the same id"
        );

        let _ = mgr.close(&session_id);
    }

    async fn db_with_session(
        session_id: &str,
        kind: Option<&str>,
        native: Option<&str>,
    ) -> sqlx::SqlitePool {
        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO hosts (id, name, hostname, auth_token_hash, status) \
             VALUES ('h1', 'h1', 'h1', 'hash', 'online')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (id, host_id, status, agent_kind, agent_session_ref) VALUES (?, 'h1', 'resumable', ?, ?)")
            .bind(session_id)
            .bind(kind)
            .bind(native)
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn build_resume_argv_claude_session() {
        let pool = db_with_session("s1", Some("claude"), Some("cc-abc")).await;
        let argv = build_resume_argv_for_session(&pool, "s1").await.unwrap();
        // resume_argv(Claude, "cc-abc") = ["claude", "--resume", "cc-abc"]
        let argv = argv.expect("claude session should yield resume argv");
        assert_eq!(argv.first().map(String::as_str), Some("claude"));
        assert!(argv.iter().any(|a| a == "cc-abc"));
        // Native id is its own token (not concatenated into another arg).
        assert!(argv.contains(&"cc-abc".to_string()));
    }

    #[tokio::test]
    async fn build_resume_argv_none_without_ref() {
        // Session with no agent_session_ref -> no resume argv.
        let pool = db_with_session("s1", None, None).await;
        assert_eq!(
            build_resume_argv_for_session(&pool, "s1").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn build_resume_argv_none_for_unknown_kind() {
        // Unknown agent kind has no known resume command.
        let pool = db_with_session("s1", Some("future_agent"), Some("xyz")).await;
        assert_eq!(
            build_resume_argv_for_session(&pool, "s1").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn build_resume_argv_native_id_stays_single_token() {
        // Injection safety at the argv-building layer: a metachar-laden native id
        // must remain exactly one argv element.
        let evil = "$(touch pwned); rm -rf /";
        let pool = db_with_session("s1", Some("claude"), Some(evil)).await;
        let argv = build_resume_argv_for_session(&pool, "s1")
            .await
            .unwrap()
            .expect("claude yields argv");
        assert!(
            argv.iter().any(|a| a == evil),
            "native id must be a single un-split argv token; got {argv:?}"
        );
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
