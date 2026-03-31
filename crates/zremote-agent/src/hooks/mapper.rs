use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::{Notify, RwLock};
use zremote_protocol::{AgenticLoopId, SessionId};

/// Maps Claude Code session IDs to zremote loop IDs.
///
/// When the agentic detector finds a CC process in a PTY session,
/// it registers the mapping. When a hook fires, we look up the
/// CC session_id (from the hook JSON) to find our internal loop_id.
#[derive(Clone)]
pub struct SessionMapper {
    /// CC session_id -> (loop_id, session_id)
    cc_to_loop: Arc<RwLock<HashMap<String, MappedSession>>>,
    /// loop_id -> CC session_id (reverse lookup)
    loop_to_cc: Arc<RwLock<HashMap<AgenticLoopId, String>>>,
    /// session_id -> loop_id (for PTY-based lookup)
    session_to_loop: Arc<RwLock<HashMap<SessionId, AgenticLoopId>>>,
    /// `session_id` -> `claude_task_id` (tracks which PTY sessions are Claude tasks)
    claude_task_ids: Arc<RwLock<HashMap<SessionId, uuid::Uuid>>>,
    /// Notified whenever a new loop is registered via `register_loop()`.
    loop_registered: Arc<Notify>,
    /// Tracks when the last hook event fired for each PTY session.
    /// Used by the output analyzer to suppress updates when hooks are active.
    hook_activity: Arc<DashMap<SessionId, Instant>>,
}

#[derive(Debug, Clone)]
pub struct MappedSession {
    pub loop_id: AgenticLoopId,
    pub session_id: SessionId,
    pub transcript_path: Option<String>,
    /// Byte offset for incremental transcript parsing.
    pub transcript_offset: u64,
}

impl SessionMapper {
    pub fn new() -> Self {
        Self {
            cc_to_loop: Arc::new(RwLock::new(HashMap::new())),
            loop_to_cc: Arc::new(RwLock::new(HashMap::new())),
            session_to_loop: Arc::new(RwLock::new(HashMap::new())),
            claude_task_ids: Arc::new(RwLock::new(HashMap::new())),
            loop_registered: Arc::new(Notify::new()),
            hook_activity: Arc::new(DashMap::new()),
        }
    }

    /// Register a mapping when a CC process is detected in a PTY session.
    pub async fn register_loop(&self, session_id: SessionId, loop_id: AgenticLoopId) {
        self.session_to_loop
            .write()
            .await
            .insert(session_id, loop_id);
        self.loop_registered.notify_waiters();
    }

    /// Record that a hook event fired for a PTY session.
    /// Called from hook handlers so the output analyzer can suppress
    /// duplicate phase updates while hooks are actively providing state.
    pub fn mark_hook_activity(&self, session_id: SessionId) {
        self.hook_activity.insert(session_id, Instant::now());
    }

    /// Check if hooks have been active for this session within the last `window`.
    /// Returns `true` if a hook fired recently (analyzer should defer).
    pub fn has_recent_hook_activity(&self, session_id: &SessionId, window: Duration) -> bool {
        self.hook_activity
            .get(session_id)
            .is_some_and(|t| t.elapsed() < window)
    }

    /// Register a PTY session as a Claude task (started via UI).
    pub async fn register_claude_task(&self, session_id: SessionId, claude_task_id: uuid::Uuid) {
        self.claude_task_ids
            .write()
            .await
            .insert(session_id, claude_task_id);
    }

    /// Get the `claude_task_id` for a PTY session, if it is a Claude task.
    pub async fn get_claude_task_id(&self, session_id: &SessionId) -> Option<uuid::Uuid> {
        self.claude_task_ids.read().await.get(session_id).copied()
    }

    /// Register a CC session_id mapping (called when we learn the CC session ID from a hook).
    pub async fn register_cc_session(
        &self,
        cc_session_id: String,
        loop_id: AgenticLoopId,
        session_id: SessionId,
    ) {
        self.cc_to_loop.write().await.insert(
            cc_session_id.clone(),
            MappedSession {
                loop_id,
                session_id,
                transcript_path: None,
                transcript_offset: 0,
            },
        );
        self.loop_to_cc.write().await.insert(loop_id, cc_session_id);
    }

    /// Look up a loop_id from a CC session_id.
    pub async fn lookup_by_cc_session(&self, cc_session_id: &str) -> Option<MappedSession> {
        self.cc_to_loop.read().await.get(cc_session_id).cloned()
    }

    /// Look up a loop_id from a zremote session_id.
    pub async fn lookup_by_session(&self, session_id: &SessionId) -> Option<AgenticLoopId> {
        self.session_to_loop.read().await.get(session_id).copied()
    }

    /// Update the transcript path for a CC session.
    pub async fn set_transcript_path(&self, cc_session_id: &str, path: String) {
        if let Some(mapped) = self.cc_to_loop.write().await.get_mut(cc_session_id) {
            mapped.transcript_path = Some(path);
        }
    }

    /// Update the transcript read offset for incremental parsing.
    pub async fn set_transcript_offset(&self, cc_session_id: &str, offset: u64) {
        if let Some(mapped) = self.cc_to_loop.write().await.get_mut(cc_session_id) {
            mapped.transcript_offset = offset;
        }
    }

    /// Remove mappings when a loop ends.
    pub async fn remove_loop(&self, loop_id: &AgenticLoopId) {
        if let Some(cc_session_id) = self.loop_to_cc.write().await.remove(loop_id) {
            self.cc_to_loop.write().await.remove(&cc_session_id);
        }
        // Clean up session_to_loop and hook_activity for the removed loop
        let mut s2l = self.session_to_loop.write().await;
        let session_ids: Vec<SessionId> = s2l
            .iter()
            .filter(|(_, lid)| *lid == loop_id)
            .map(|(sid, _)| *sid)
            .collect();
        for sid in &session_ids {
            s2l.remove(sid);
            self.hook_activity.remove(sid);
        }
    }

    /// Try to resolve a CC session ID by checking active loops that don't
    /// yet have a CC session mapping. If found, auto-registers the mapping.
    async fn try_resolve_fallback(&self, cc_session_id: &str) -> Option<MappedSession> {
        let session_to_loop = self.session_to_loop.read().await;
        if let Some((&session_id, &loop_id)) = session_to_loop.iter().next() {
            // Check if this loop already has a CC session mapping
            let has_mapping = self.loop_to_cc.read().await.contains_key(&loop_id);
            if !has_mapping {
                drop(session_to_loop);
                self.register_cc_session(cc_session_id.to_string(), loop_id, session_id)
                    .await;
                return self.lookup_by_cc_session(cc_session_id).await;
            }
        }
        None
    }

    /// Try to find a loop_id for a hook event by checking known CC sessions,
    /// or fall back to matching by cwd against known PTY sessions.
    /// If no mapping is found immediately, retries up to 5 times waiting for
    /// a loop registration (handles the race where hooks fire before the
    /// 3-second agentic detection polling registers the loop).
    pub async fn resolve_loop_id(
        &self,
        cc_session_id: &str,
        _cwd: Option<&str>,
    ) -> Option<MappedSession> {
        // Direct lookup by CC session ID
        if let Some(mapped) = self.lookup_by_cc_session(cc_session_id).await {
            return Some(mapped);
        }

        // Fallback: check if we have any active loop and auto-register.
        if let Some(mapped) = self.try_resolve_fallback(cc_session_id).await {
            return Some(mapped);
        }

        // Both lookups failed. Wait for a loop registration and retry.
        // This handles the race where CC hooks fire before the 3s agentic
        // detection polling has registered the loop.
        for _ in 0..5 {
            tokio::select! {
                () = self.loop_registered.notified() => {
                    if let Some(mapped) = self.try_resolve_fallback(cc_session_id).await {
                        return Some(mapped);
                    }
                }
                () = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use uuid::Uuid;

    #[tokio::test]
    async fn register_and_lookup_loop() {
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        mapper.register_loop(session_id, loop_id).await;
        assert_eq!(mapper.lookup_by_session(&session_id).await, Some(loop_id));
    }

    #[tokio::test]
    async fn register_cc_session_and_lookup() {
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let cc_id = "cc-session-123".to_string();

        mapper
            .register_cc_session(cc_id.clone(), loop_id, session_id)
            .await;

        let mapped = mapper.lookup_by_cc_session(&cc_id).await.unwrap();
        assert_eq!(mapped.loop_id, loop_id);
        assert_eq!(mapped.session_id, session_id);
    }

    #[tokio::test]
    async fn remove_loop_cleans_all_mappings() {
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let cc_id = "cc-session-456".to_string();

        mapper.register_loop(session_id, loop_id).await;
        mapper
            .register_cc_session(cc_id.clone(), loop_id, session_id)
            .await;

        mapper.remove_loop(&loop_id).await;

        assert!(mapper.lookup_by_cc_session(&cc_id).await.is_none());
        assert!(mapper.lookup_by_session(&session_id).await.is_none());
    }

    #[tokio::test]
    async fn set_transcript_path_and_offset() {
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();
        let cc_id = "cc-session-789".to_string();

        mapper
            .register_cc_session(cc_id.clone(), loop_id, session_id)
            .await;
        mapper
            .set_transcript_path(&cc_id, "/tmp/transcript.jsonl".to_string())
            .await;
        mapper.set_transcript_offset(&cc_id, 1024).await;

        let mapped = mapper.lookup_by_cc_session(&cc_id).await.unwrap();
        assert_eq!(
            mapped.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl")
        );
        assert_eq!(mapped.transcript_offset, 1024);
    }

    #[tokio::test]
    async fn resolve_auto_registers_first_hook() {
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        // Register a loop via detector (no CC session ID yet)
        mapper.register_loop(session_id, loop_id).await;

        // First hook comes in with a CC session ID we haven't seen
        let mapped = mapper
            .resolve_loop_id("new-cc-session", None)
            .await
            .unwrap();
        assert_eq!(mapped.loop_id, loop_id);

        // Subsequent lookups work directly
        let mapped2 = mapper.lookup_by_cc_session("new-cc-session").await.unwrap();
        assert_eq!(mapped2.loop_id, loop_id);
    }

    #[tokio::test]
    async fn resolve_unknown_returns_none() {
        let mapper = SessionMapper::new();
        assert!(mapper.resolve_loop_id("unknown", None).await.is_none());
    }

    #[tokio::test]
    async fn resolve_retry_succeeds_after_late_registration() {
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        // Simulate the agentic detector registering a loop after 500ms
        let mapper_clone = mapper.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            mapper_clone.register_loop(session_id, loop_id).await;
        });

        // resolve_loop_id should retry and succeed once the loop is registered
        let mapped = mapper
            .resolve_loop_id("late-cc-session", None)
            .await
            .expect("should resolve after late registration");
        assert_eq!(mapped.loop_id, loop_id);
        assert_eq!(mapped.session_id, session_id);

        // Verify the CC session was auto-registered for subsequent lookups
        let mapped2 = mapper
            .lookup_by_cc_session("late-cc-session")
            .await
            .expect("CC session should be registered after resolve");
        assert_eq!(mapped2.loop_id, loop_id);
    }
}
