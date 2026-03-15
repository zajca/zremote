pub mod client;
pub mod config;
pub mod process;

use myremote_protocol::knowledge::{
    KnowledgeAgentMessage, KnowledgeServerMessage, ServiceAction,
};
use myremote_protocol::AgentMessage;
use tokio::sync::mpsc;

use self::client::OvClient;
use self::process::OvProcess;

/// Orchestrates `OpenViking` lifecycle and message handling.
pub struct KnowledgeManager {
    process: OvProcess,
    client: OvClient,
    outbound_tx: mpsc::Sender<AgentMessage>,
    enabled: bool,
}

impl KnowledgeManager {
    pub fn new(
        binary: String,
        port: u16,
        data_dir: std::path::PathBuf,
        outbound_tx: mpsc::Sender<AgentMessage>,
    ) -> Self {
        Self {
            process: OvProcess::new(binary, port, data_dir),
            client: OvClient::new(port),
            outbound_tx,
            enabled: false,
        }
    }

    /// Handle a `KnowledgeServerMessage` from the server.
    pub async fn handle_message(&mut self, msg: KnowledgeServerMessage) {
        match msg {
            KnowledgeServerMessage::ServiceControl { action } => match action {
                ServiceAction::Start => self.start_service().await,
                ServiceAction::Stop => self.stop_service().await,
                ServiceAction::Restart => {
                    self.stop_service().await;
                    self.start_service().await;
                }
            },
            KnowledgeServerMessage::IndexProject {
                project_path,
                force_reindex,
            } => {
                self.index_project(&project_path, force_reindex).await;
            }
            KnowledgeServerMessage::Search {
                project_path,
                request_id,
                query,
                tier,
                max_results,
            } => {
                self.search(&project_path, request_id, &query, tier, max_results)
                    .await;
            }
            KnowledgeServerMessage::ExtractMemory {
                loop_id,
                project_path,
                transcript,
            } => {
                self.extract_memory(loop_id, &project_path, &transcript)
                    .await;
            }
            KnowledgeServerMessage::GenerateInstructions { project_path } => {
                self.generate_instructions(&project_path).await;
            }
        }
    }

    fn send_knowledge_msg(&self, msg: KnowledgeAgentMessage) {
        if self
            .outbound_tx
            .try_send(AgentMessage::KnowledgeAction(msg))
            .is_err()
        {
            tracing::warn!("outbound channel full, knowledge message dropped");
        }
    }

    async fn start_service(&mut self) {
        self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
            status: myremote_protocol::knowledge::KnowledgeServiceStatus::Starting,
            version: None,
            error: None,
        });

        match self.process.start().await {
            Ok(()) => {
                self.enabled = true;
                self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
                    status: myremote_protocol::knowledge::KnowledgeServiceStatus::Ready,
                    version: None,
                    error: None,
                });
                tracing::info!("OpenViking service started");
            }
            Err(e) => {
                self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
                    status: myremote_protocol::knowledge::KnowledgeServiceStatus::Error,
                    version: None,
                    error: Some(e.to_string()),
                });
                tracing::error!(error = %e, "failed to start OpenViking");
            }
        }
    }

    async fn stop_service(&mut self) {
        if let Err(e) = self.process.stop().await {
            tracing::error!(error = %e, "failed to stop OpenViking");
        }
        self.enabled = false;
        self.send_knowledge_msg(KnowledgeAgentMessage::ServiceStatus {
            status: myremote_protocol::knowledge::KnowledgeServiceStatus::Stopped,
            version: None,
            error: None,
        });
    }

    async fn index_project(&self, project_path: &str, force_reindex: bool) {
        if !self.enabled {
            tracing::warn!("OpenViking not running, cannot index");
            return;
        }
        let namespace = format!(
            "viking://resources/{}/",
            project_name_from_path(project_path)
        );
        match self
            .client
            .index_project(&namespace, project_path, force_reindex)
            .await
        {
            Ok(()) => {
                tracing::info!(project_path, "indexing started");
            }
            Err(e) => {
                tracing::error!(project_path, error = %e, "failed to start indexing");
                self.send_knowledge_msg(KnowledgeAgentMessage::IndexingProgress {
                    project_path: project_path.to_string(),
                    status: myremote_protocol::knowledge::IndexingStatus::Failed,
                    files_processed: 0,
                    files_total: 0,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    async fn search(
        &self,
        project_path: &str,
        request_id: uuid::Uuid,
        query: &str,
        tier: myremote_protocol::knowledge::SearchTier,
        max_results: Option<u32>,
    ) {
        if !self.enabled {
            self.send_knowledge_msg(KnowledgeAgentMessage::SearchResults {
                project_path: project_path.to_string(),
                request_id,
                results: vec![],
                duration_ms: 0,
            });
            return;
        }
        let namespace = format!(
            "viking://resources/{}/",
            project_name_from_path(project_path)
        );
        let tier_str = serde_json::to_value(tier)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "l1".to_string());
        let start = std::time::Instant::now();
        match self
            .client
            .search(&namespace, query, &tier_str, max_results.unwrap_or(20))
            .await
        {
            Ok(results) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                self.send_knowledge_msg(KnowledgeAgentMessage::SearchResults {
                    project_path: project_path.to_string(),
                    request_id,
                    results,
                    duration_ms,
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "search failed");
                self.send_knowledge_msg(KnowledgeAgentMessage::SearchResults {
                    project_path: project_path.to_string(),
                    request_id,
                    results: vec![],
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
        }
    }

    async fn extract_memory(
        &self,
        loop_id: myremote_protocol::AgenticLoopId,
        project_path: &str,
        transcript: &[myremote_protocol::knowledge::TranscriptFragment],
    ) {
        if !self.enabled {
            tracing::warn!("OpenViking not running, cannot extract memories");
            return;
        }
        let namespace = format!(
            "viking://resources/{}/",
            project_name_from_path(project_path)
        );
        match self
            .client
            .extract_memories(&namespace, transcript, loop_id)
            .await
        {
            Ok(memories) => {
                self.send_knowledge_msg(KnowledgeAgentMessage::MemoryExtracted {
                    loop_id,
                    memories,
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "memory extraction failed");
            }
        }
    }

    async fn generate_instructions(&self, project_path: &str) {
        if !self.enabled {
            tracing::warn!("OpenViking not running, cannot generate instructions");
            self.send_knowledge_msg(KnowledgeAgentMessage::InstructionsGenerated {
                project_path: project_path.to_string(),
                content: "# Project Knowledge\n\nOpenViking service is not running. Start it first.\n".to_string(),
                memories_used: 0,
            });
            return;
        }
        let namespace = format!(
            "viking://resources/{}/",
            project_name_from_path(project_path)
        );
        match self.client.synthesize_knowledge(&namespace).await {
            Ok((content, memories_used)) => {
                self.send_knowledge_msg(KnowledgeAgentMessage::InstructionsGenerated {
                    project_path: project_path.to_string(),
                    content,
                    memories_used,
                });
            }
            Err(e) => {
                tracing::error!(error = %e, "instruction generation failed");
                self.send_knowledge_msg(KnowledgeAgentMessage::InstructionsGenerated {
                    project_path: project_path.to_string(),
                    content: format!("# Error\n\nFailed to generate instructions: {e}\n"),
                    memories_used: 0,
                });
            }
        }
    }

    pub fn is_running(&self) -> bool {
        self.enabled
    }
}

/// Extract project name from path (last component).
fn project_name_from_path(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name_from_path_extracts_last_component() {
        assert_eq!(project_name_from_path("/home/user/project"), "project");
        assert_eq!(project_name_from_path("/home/user/my-app"), "my-app");
        assert_eq!(project_name_from_path("simple"), "simple");
    }
}
