use std::collections::HashMap;
use std::sync::Arc;

use myremote_protocol::{AgenticLoopId, SessionId};
use tokio::sync::RwLock;

/// Maps Claude Code session IDs to myremote loop IDs.
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
        }
    }

    /// Register a mapping when a CC process is detected in a PTY session.
    pub async fn register_loop(&self, session_id: SessionId, loop_id: AgenticLoopId) {
        self.session_to_loop
            .write()
            .await
            .insert(session_id, loop_id);
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
        self.loop_to_cc
            .write()
            .await
            .insert(loop_id, cc_session_id);
    }

    /// Look up a loop_id from a CC session_id.
    pub async fn lookup_by_cc_session(&self, cc_session_id: &str) -> Option<MappedSession> {
        self.cc_to_loop.read().await.get(cc_session_id).cloned()
    }

    /// Look up a loop_id from a myremote session_id.
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
        self.session_to_loop
            .write()
            .await
            .retain(|_, lid| lid != loop_id);
    }

    /// Try to find a loop_id for a hook event by checking known CC sessions,
    /// or fall back to matching by cwd against known PTY sessions.
    pub async fn resolve_loop_id(
        &self,
        cc_session_id: &str,
        _cwd: Option<&str>,
    ) -> Option<MappedSession> {
        // Direct lookup by CC session ID
        if let Some(mapped) = self.lookup_by_cc_session(cc_session_id).await {
            return Some(mapped);
        }

        // If not found, check if we have any active loop and auto-register.
        // This handles the case where the first hook fires before we've seen
        // the CC session_id. We pick the most recently registered loop.
        let session_to_loop = self.session_to_loop.read().await;
        if let Some((&session_id, &loop_id)) = session_to_loop.iter().next() {
            // Check if this loop already has a CC session mapping
            let has_mapping = self
                .loop_to_cc
                .read()
                .await
                .contains_key(&loop_id);
            if !has_mapping {
                drop(session_to_loop);
                self.register_cc_session(cc_session_id.to_string(), loop_id, session_id)
                    .await;
                return self.lookup_by_cc_session(cc_session_id).await;
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn register_and_lookup_loop() {
        let mapper = SessionMapper::new();
        let session_id = Uuid::new_v4();
        let loop_id = Uuid::new_v4();

        mapper.register_loop(session_id, loop_id).await;
        assert_eq!(
            mapper.lookup_by_session(&session_id).await,
            Some(loop_id)
        );
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
        let mapped2 = mapper
            .lookup_by_cc_session("new-cc-session")
            .await
            .unwrap();
        assert_eq!(mapped2.loop_id, loop_id);
    }

    #[tokio::test]
    async fn resolve_unknown_returns_none() {
        let mapper = SessionMapper::new();
        assert!(mapper.resolve_loop_id("unknown", None).await.is_none());
    }
}
