use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::debug;
use zremote_client::{ClientEvent, EventStream};

use crate::types::{FfiHostInfo, FfiLoopInfo, FfiSessionInfo};

/// Callback interface for receiving server events.
///
/// Implement this trait in Kotlin/Swift to receive real-time events.
/// All methods are called from a background thread -- dispatch to the
/// main thread if updating UI.
#[uniffi::export(callback_interface)]
pub trait EventListener: Send + Sync {
    // Connection lifecycle
    fn on_connected(&self);
    fn on_disconnected(&self);

    // Hosts
    fn on_host_connected(&self, host: FfiHostInfo);
    fn on_host_disconnected(&self, host_id: String);
    fn on_host_status_changed(&self, host_id: String, status: String);

    // Sessions
    fn on_session_created(&self, session: FfiSessionInfo);
    fn on_session_closed(&self, session_id: String, exit_code: Option<i32>);
    fn on_session_updated(&self, session_id: String);
    fn on_session_suspended(&self, session_id: String);
    fn on_session_resumed(&self, session_id: String);

    // Projects
    fn on_projects_updated(&self, host_id: String);

    // Agentic loops
    fn on_loop_detected(&self, loop_info: FfiLoopInfo, host_id: String, hostname: String);
    fn on_loop_status_changed(&self, loop_info: FfiLoopInfo, host_id: String, hostname: String);
    fn on_loop_ended(&self, loop_info: FfiLoopInfo, host_id: String, hostname: String);

    // Knowledge
    fn on_knowledge_status_changed(&self, host_id: String, status: String, error: Option<String>);
    fn on_indexing_progress(
        &self,
        project_id: String,
        project_path: String,
        status: String,
        files_processed: u64,
        files_total: u64,
    );
    fn on_memory_extracted(&self, project_id: String, loop_id: String, memory_count: u32);

    // Worktrees
    fn on_worktree_error(&self, host_id: String, project_path: String, message: String);

    // Claude tasks
    fn on_claude_task_started(
        &self,
        task_id: String,
        session_id: String,
        host_id: String,
        project_path: String,
    );
    fn on_claude_task_updated(&self, task_id: String, status: String, loop_id: Option<String>);
    fn on_claude_task_ended(&self, task_id: String, status: String, summary: Option<String>);
    fn on_claude_session_metrics(&self, metrics: crate::types::FfiClaudeSessionMetrics);
}

/// Handle to a running event stream connection.
/// Call `disconnect()` or drop to stop receiving events.
#[derive(uniffi::Object)]
pub struct ZRemoteEventStream {
    cancel: CancellationToken,
    _runtime: Arc<tokio::runtime::Runtime>,
}

#[uniffi::export]
impl ZRemoteEventStream {
    /// Stop the event stream and disconnect the WebSocket.
    pub fn disconnect(&self) {
        self.cancel.cancel();
    }
}

impl ZRemoteEventStream {
    /// Start the event stream dispatcher.
    pub(crate) fn start(
        event_stream: EventStream,
        listener: Box<dyn EventListener>,
        runtime: &Arc<tokio::runtime::Runtime>,
    ) -> Arc<Self> {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let listener = Arc::from(listener);

        runtime.spawn(async move {
            dispatch_events(event_stream, listener, cancel_clone).await;
        });

        Arc::new(Self {
            cancel,
            _runtime: Arc::clone(runtime),
        })
    }
}

async fn dispatch_events(
    event_stream: EventStream,
    listener: Arc<dyn EventListener>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                debug!("event stream dispatcher cancelled");
                listener.on_disconnected();
                return;
            }
            result = event_stream.rx.recv_async() => {
                if let Ok(event) = result {
                    dispatch_single_event(&listener, event);
                } else {
                    debug!("event stream channel closed");
                    return;
                }
            }
        }
    }
}

#[allow(clippy::too_many_lines)] // One match arm per ServerEvent variant
fn dispatch_single_event(listener: &Arc<dyn EventListener>, event: ClientEvent) {
    match event {
        ClientEvent::Connected => listener.on_connected(),
        ClientEvent::Disconnected => listener.on_disconnected(),
        ClientEvent::Server(boxed) => match *boxed {
            zremote_client::types::ServerEvent::HostConnected { host } => {
                listener.on_host_connected(host.into());
            }
            zremote_client::types::ServerEvent::HostDisconnected { host_id } => {
                listener.on_host_disconnected(host_id);
            }
            zremote_client::types::ServerEvent::HostStatusChanged { host_id, status } => {
                listener.on_host_status_changed(host_id, status);
            }
            zremote_client::types::ServerEvent::SessionCreated { session } => {
                listener.on_session_created(session.into());
            }
            zremote_client::types::ServerEvent::SessionClosed {
                session_id,
                exit_code,
            } => {
                listener.on_session_closed(session_id, exit_code);
            }
            zremote_client::types::ServerEvent::SessionUpdated { session_id } => {
                listener.on_session_updated(session_id);
            }
            zremote_client::types::ServerEvent::SessionSuspended { session_id } => {
                listener.on_session_suspended(session_id);
            }
            zremote_client::types::ServerEvent::SessionResumed { session_id } => {
                listener.on_session_resumed(session_id);
            }
            zremote_client::types::ServerEvent::ProjectsUpdated { host_id } => {
                listener.on_projects_updated(host_id);
            }
            zremote_client::types::ServerEvent::LoopDetected {
                loop_info,
                host_id,
                hostname,
            } => {
                listener.on_loop_detected(loop_info.into(), host_id, hostname);
            }
            zremote_client::types::ServerEvent::LoopStatusChanged {
                loop_info,
                host_id,
                hostname,
            } => {
                listener.on_loop_status_changed(loop_info.into(), host_id, hostname);
            }
            zremote_client::types::ServerEvent::LoopEnded {
                loop_info,
                host_id,
                hostname,
            } => {
                listener.on_loop_ended(loop_info.into(), host_id, hostname);
            }
            zremote_client::types::ServerEvent::KnowledgeStatusChanged {
                host_id,
                status,
                error,
            } => {
                listener.on_knowledge_status_changed(host_id, status, error);
            }
            zremote_client::types::ServerEvent::IndexingProgress {
                project_id,
                project_path,
                status,
                files_processed,
                files_total,
            } => {
                listener.on_indexing_progress(
                    project_id,
                    project_path,
                    status,
                    files_processed,
                    files_total,
                );
            }
            zremote_client::types::ServerEvent::MemoryExtracted {
                project_id,
                loop_id,
                memory_count,
            } => {
                listener.on_memory_extracted(project_id, loop_id, memory_count);
            }
            zremote_client::types::ServerEvent::WorktreeError {
                host_id,
                project_path,
                message,
            } => {
                listener.on_worktree_error(host_id, project_path, message);
            }
            zremote_client::types::ServerEvent::ClaudeTaskStarted {
                task_id,
                session_id,
                host_id,
                project_path,
            } => {
                listener.on_claude_task_started(task_id, session_id, host_id, project_path);
            }
            zremote_client::types::ServerEvent::ClaudeTaskUpdated {
                task_id,
                status,
                loop_id,
            } => {
                listener.on_claude_task_updated(task_id, status, loop_id);
            }
            zremote_client::types::ServerEvent::ClaudeTaskEnded {
                task_id,
                status,
                summary,
            } => {
                listener.on_claude_task_ended(task_id, status, summary);
            }
            zremote_client::types::ServerEvent::ClaudeSessionMetrics {
                session_id,
                model,
                context_used_pct,
                context_window_size,
                cost_usd,
                tokens_in,
                tokens_out,
                lines_added,
                lines_removed,
                rate_limit_5h_pct,
                rate_limit_7d_pct,
            } => {
                listener.on_claude_session_metrics(crate::types::FfiClaudeSessionMetrics {
                    session_id,
                    model,
                    context_used_pct,
                    context_window_size,
                    cost_usd,
                    tokens_in,
                    tokens_out,
                    lines_added,
                    lines_removed,
                    rate_limit_5h_pct,
                    rate_limit_7d_pct,
                });
            }
        },
    }
}
