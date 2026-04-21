//! Server-side dispatch for streaming git diffs and single-shot reviews.
//!
//! A single `DiffDispatch` holds three independent registries:
//!
//! - **Stream registry** — `request_id → mpsc::Sender<DiffStreamChunk>`.
//!   Populated by `POST /api/projects/:id/diff`; drained by the match arms in
//!   `routes/agents/dispatch.rs` when `AgentMessage::DiffStarted /
//!   DiffFileChunk / DiffFinished` arrive. `forward` silently drops the chunk
//!   when the receiver is gone (client disconnected mid-stream); `finish`
//!   sends the terminal `Finished` variant and unregisters.
//!
//! - **Sources registry** — `request_id → oneshot::Sender<DiffSourcesReply>`.
//!   Populated by `GET /api/projects/:id/diff/sources`; drained by
//!   `AgentMessage::DiffSourcesResult`.
//!
//! - **Review registry** — `request_id → oneshot::Sender<SendReviewReply>`.
//!   Populated by `POST /api/projects/:id/review/send`; drained by
//!   `AgentMessage::SendReviewResult`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc, oneshot};
use uuid::Uuid;
use zremote_protocol::project::{
    DiffError, DiffFile, DiffFileSummary, DiffSourceOptions, SendReviewResponse,
};

/// One chunk of the NDJSON body emitted to the HTTP client. Matches the
/// agent-side `DiffEvent` in structure so the REST body is a 1:1 forward.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiffStreamChunk {
    Started {
        files: Vec<DiffFileSummary>,
    },
    File {
        file_index: u32,
        file: Box<DiffFile>,
    },
    Finished {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<DiffError>,
    },
}

/// Result delivered via the sources oneshot registry.
pub struct DiffSourcesReply {
    pub options: Option<Box<DiffSourceOptions>>,
    pub error: Option<DiffError>,
}

/// Result delivered via the review oneshot registry.
pub struct SendReviewReply {
    pub response: Option<Box<SendReviewResponse>>,
    pub error: Option<DiffError>,
}

/// Channel depth for the HTTP body stream. Must match the bound used in the
/// agent's local REST so backpressure behaves consistently in both modes.
pub const DIFF_STREAM_CHANNEL_DEPTH: usize = 32;

/// Central dispatch registry shared across the HTTP and WS layers.
pub struct DiffDispatch {
    stream: RwLock<HashMap<Uuid, mpsc::Sender<DiffStreamChunk>>>,
    sources: RwLock<HashMap<Uuid, oneshot::Sender<DiffSourcesReply>>>,
    review: RwLock<HashMap<Uuid, oneshot::Sender<SendReviewReply>>>,
}

impl DiffDispatch {
    #[must_use]
    pub fn new() -> Self {
        Self {
            stream: RwLock::new(HashMap::new()),
            sources: RwLock::new(HashMap::new()),
            review: RwLock::new(HashMap::new()),
        }
    }

    // ---- stream registry ---------------------------------------------------

    /// Register a new streaming diff request. The caller owns the matching
    /// `Receiver` that feeds the HTTP body.
    pub async fn register_stream(&self, request_id: Uuid, tx: mpsc::Sender<DiffStreamChunk>) {
        self.stream.write().await.insert(request_id, tx);
    }

    /// Forward a chunk to the HTTP body. Silently discards when the receiver
    /// is already gone (dropped body / cancelled request).
    pub async fn forward_stream(&self, request_id: Uuid, chunk: DiffStreamChunk) {
        let tx = {
            let map = self.stream.read().await;
            map.get(&request_id).cloned()
        };
        if let Some(tx) = tx
            && let Err(e) = tx.send(chunk).await
        {
            tracing::debug!(%request_id, error = %e, "diff stream receiver gone; chunk dropped");
        }
    }

    /// Send a terminal `Finished` chunk and unregister the request.
    pub async fn finish_stream(&self, request_id: Uuid, error: Option<DiffError>) {
        // Take the sender out so dropping happens after we send the final chunk.
        let tx = self.stream.write().await.remove(&request_id);
        if let Some(tx) = tx
            && let Err(e) = tx.send(DiffStreamChunk::Finished { error }).await
        {
            tracing::debug!(%request_id, error = %e, "diff stream receiver gone at finish");
        }
    }

    /// Unregister without sending any terminal chunk (used on REST-side
    /// cancellation where the body is already being dropped).
    pub async fn unregister_stream(&self, request_id: Uuid) {
        self.stream.write().await.remove(&request_id);
    }

    // ---- sources registry --------------------------------------------------

    pub async fn register_sources(&self, request_id: Uuid, tx: oneshot::Sender<DiffSourcesReply>) {
        self.sources.write().await.insert(request_id, tx);
    }

    pub async fn complete_sources(&self, request_id: Uuid, reply: DiffSourcesReply) {
        let tx = self.sources.write().await.remove(&request_id);
        if let Some(tx) = tx {
            let _ = tx.send(reply);
        } else {
            tracing::debug!(%request_id, "no pending sources request");
        }
    }

    pub async fn unregister_sources(&self, request_id: Uuid) {
        self.sources.write().await.remove(&request_id);
    }

    // ---- review registry ---------------------------------------------------

    pub async fn register_review(&self, request_id: Uuid, tx: oneshot::Sender<SendReviewReply>) {
        self.review.write().await.insert(request_id, tx);
    }

    pub async fn complete_review(&self, request_id: Uuid, reply: SendReviewReply) {
        let tx = self.review.write().await.remove(&request_id);
        if let Some(tx) = tx {
            let _ = tx.send(reply);
        } else {
            tracing::debug!(%request_id, "no pending review request");
        }
    }

    pub async fn unregister_review(&self, request_id: Uuid) {
        self.review.write().await.remove(&request_id);
    }
}

impl Default for DiffDispatch {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience alias used by `AppState`.
pub type SharedDiffDispatch = Arc<DiffDispatch>;

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_protocol::project::{DiffErrorCode, DiffFileStatus};

    fn sample_summary(path: &str) -> DiffFileSummary {
        DiffFileSummary {
            path: path.to_string(),
            old_path: None,
            status: DiffFileStatus::Modified,
            binary: false,
            submodule: false,
            too_large: false,
            additions: 1,
            deletions: 0,
            old_sha: None,
            new_sha: None,
            old_mode: None,
            new_mode: None,
        }
    }

    fn sample_file() -> DiffFile {
        DiffFile {
            summary: sample_summary("a.rs"),
            hunks: Vec::new(),
        }
    }

    #[tokio::test]
    async fn register_forward_finish_unregister_round_trip() {
        let dispatch = DiffDispatch::new();
        let id = Uuid::new_v4();
        let (tx, mut rx) = mpsc::channel::<DiffStreamChunk>(8);
        dispatch.register_stream(id, tx).await;

        dispatch
            .forward_stream(
                id,
                DiffStreamChunk::Started {
                    files: vec![sample_summary("a.rs")],
                },
            )
            .await;
        dispatch
            .forward_stream(
                id,
                DiffStreamChunk::File {
                    file_index: 0,
                    file: Box::new(sample_file()),
                },
            )
            .await;
        dispatch.finish_stream(id, None).await;

        let c1 = rx.recv().await.expect("started chunk");
        assert!(matches!(c1, DiffStreamChunk::Started { .. }));
        let c2 = rx.recv().await.expect("file chunk");
        assert!(matches!(c2, DiffStreamChunk::File { .. }));
        let c3 = rx.recv().await.expect("finished chunk");
        assert!(matches!(c3, DiffStreamChunk::Finished { error: None }));
        assert!(rx.recv().await.is_none(), "stream must close after finish");

        // Finish removes the entry; forwarding after that is a no-op.
        dispatch
            .forward_stream(id, DiffStreamChunk::Finished { error: None })
            .await;
    }

    #[tokio::test]
    async fn forward_after_receiver_drop_is_silent() {
        let dispatch = DiffDispatch::new();
        let id = Uuid::new_v4();
        let (tx, rx) = mpsc::channel::<DiffStreamChunk>(8);
        dispatch.register_stream(id, tx).await;
        drop(rx);

        // Must not panic, must not error.
        dispatch
            .forward_stream(id, DiffStreamChunk::Started { files: vec![] })
            .await;
        dispatch.finish_stream(id, None).await;
    }

    #[tokio::test]
    async fn sources_oneshot_delivers_reply() {
        let dispatch = DiffDispatch::new();
        let id = Uuid::new_v4();
        let (tx, rx) = oneshot::channel::<DiffSourcesReply>();
        dispatch.register_sources(id, tx).await;

        dispatch
            .complete_sources(
                id,
                DiffSourcesReply {
                    options: None,
                    error: Some(DiffError {
                        code: DiffErrorCode::NotGitRepo,
                        message: "x".to_string(),
                        hint: None,
                    }),
                },
            )
            .await;

        let reply = rx.await.expect("reply");
        assert!(reply.options.is_none());
        assert_eq!(reply.error.unwrap().code, DiffErrorCode::NotGitRepo);
    }

    #[tokio::test]
    async fn review_oneshot_delivers_reply() {
        let dispatch = DiffDispatch::new();
        let id = Uuid::new_v4();
        let (tx, rx) = oneshot::channel::<SendReviewReply>();
        dispatch.register_review(id, tx).await;

        dispatch
            .complete_review(
                id,
                SendReviewReply {
                    response: Some(Box::new(SendReviewResponse {
                        session_id: Uuid::new_v4(),
                        delivered: 2,
                    })),
                    error: None,
                },
            )
            .await;

        let reply = rx.await.expect("reply");
        assert_eq!(reply.response.unwrap().delivered, 2);
    }

    #[tokio::test]
    async fn complete_without_registration_is_noop() {
        let dispatch = DiffDispatch::new();
        dispatch
            .complete_sources(
                Uuid::new_v4(),
                DiffSourcesReply {
                    options: None,
                    error: None,
                },
            )
            .await;
        dispatch
            .complete_review(
                Uuid::new_v4(),
                SendReviewReply {
                    response: None,
                    error: None,
                },
            )
            .await;
    }
}
