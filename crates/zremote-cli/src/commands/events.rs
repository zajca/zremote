use zremote_client::{ApiClient, ClientEvent, EventStream, ServerEvent};

/// Stream server events as NDJSON until Ctrl+C.
pub async fn run(client: &ApiClient, filter: Option<String>) -> i32 {
    let events_url = client.events_ws_url();
    let filter_types: Option<Vec<String>> =
        filter.map(|f| f.split(',').map(|s| s.trim().to_lowercase()).collect());

    let handle = tokio::runtime::Handle::current();
    let stream = EventStream::connect(events_url, &handle);

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                break;
            }
            event = stream.rx.recv_async() => {
                match event {
                    Ok(ClientEvent::Server(server_event)) => {
                        if let Some(ref types) = filter_types {
                            let event_type = event_type_name(&server_event);
                            if !types.iter().any(|t| event_type.contains(t.as_str())) {
                                continue;
                            }
                        }
                        match serde_json::to_string(&*server_event) {
                            Ok(json) => println!("{json}"),
                            Err(e) => eprintln!("Error serializing event: {e}"),
                        }
                    }
                    Ok(ClientEvent::Connected) => {
                        eprintln!("Connected to event stream.");
                    }
                    Ok(ClientEvent::Disconnected) => {
                        eprintln!("Disconnected from event stream, reconnecting...");
                    }
                    Err(_) => {
                        eprintln!("Event stream closed.");
                        return 1;
                    }
                }
            }
        }
    }

    0
}

/// Extract the serde tag name from a `ServerEvent` variant.
fn event_type_name(event: &ServerEvent) -> &'static str {
    match event {
        ServerEvent::HostConnected { .. } => "host_connected",
        ServerEvent::HostDisconnected { .. } => "host_disconnected",
        ServerEvent::HostStatusChanged { .. } => "host_status_changed",
        ServerEvent::SessionCreated { .. } => "session_created",
        ServerEvent::SessionClosed { .. } => "session_closed",
        ServerEvent::SessionSuspended { .. } => "session_suspended",
        ServerEvent::SessionResumed { .. } => "session_resumed",
        ServerEvent::SessionUpdated { .. } => "session_updated",
        ServerEvent::LoopDetected { .. } => "agentic_loop_detected",
        ServerEvent::LoopStatusChanged { .. } => "agentic_loop_state_update",
        ServerEvent::LoopEnded { .. } => "agentic_loop_ended",
        ServerEvent::LoopMetricsUpdated { .. } => "agentic_loop_metrics_update",
        ServerEvent::ProjectsUpdated { .. } => "projects_updated",
        ServerEvent::KnowledgeStatusChanged { .. } => "knowledge_status_changed",
        ServerEvent::IndexingProgress { .. } => "indexing_progress",
        ServerEvent::MemoryExtracted { .. } => "memory_extracted",
        ServerEvent::WorktreeError { .. } => "worktree_error",
        ServerEvent::WorktreeCreationProgress { .. } => "worktree_creation_progress",
        ServerEvent::ClaudeTaskStarted { .. } => "claude_task_started",
        ServerEvent::ClaudeTaskUpdated { .. } => "claude_task_updated",
        ServerEvent::ClaudeTaskEnded { .. } => "claude_task_ended",
        ServerEvent::ClaudeSessionMetrics { .. } => "claude_session_metrics",
        ServerEvent::ExecutionNodeCreated { .. } => "execution_node_created",
        ServerEvent::ExecutionNodeUpdated { .. } => "execution_node_updated",
        ServerEvent::ChannelPermissionRequested { .. } => "channel_permission_requested",
        ServerEvent::ChannelWorkerReply { .. } => "channel_worker_reply",
        ServerEvent::EventsLagged { .. } => "events_lagged",
        ServerEvent::Unknown => "unknown",
    }
}
