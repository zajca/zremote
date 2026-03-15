use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AgenticLoopId;

pub type KnowledgeBaseId = Uuid;

/// Status of the OpenViking service on a host.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeServiceStatus {
    Starting,
    Ready,
    Indexing,
    Error,
    Stopped,
}

/// Status of an indexing operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndexingStatus {
    Queued,
    InProgress,
    Completed,
    Failed,
}

/// Search result tier.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchTier {
    L0,
    L1,
    L2,
}

/// Category of extracted memory.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    Pattern,
    Decision,
    Pitfall,
    Preference,
    Architecture,
    Convention,
}

/// Service lifecycle action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

/// A single search result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchResult {
    pub path: String,
    pub score: f64,
    pub snippet: String,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub tier: SearchTier,
}

/// An extracted memory from a transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedMemory {
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub confidence: f64,
    pub source_loop_id: AgenticLoopId,
}

/// Minimal transcript fragment for memory extraction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptFragment {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

/// Knowledge messages sent from agent to server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum KnowledgeAgentMessage {
    ServiceStatus {
        status: KnowledgeServiceStatus,
        version: Option<String>,
        error: Option<String>,
    },
    KnowledgeBaseReady {
        project_path: String,
        total_files: u64,
        total_chunks: u64,
    },
    IndexingProgress {
        project_path: String,
        status: IndexingStatus,
        files_processed: u64,
        files_total: u64,
        error: Option<String>,
    },
    SearchResults {
        project_path: String,
        request_id: Uuid,
        results: Vec<SearchResult>,
        duration_ms: u64,
    },
    MemoryExtracted {
        loop_id: AgenticLoopId,
        memories: Vec<ExtractedMemory>,
    },
    InstructionsGenerated {
        project_path: String,
        content: String,
        memories_used: u32,
    },
}

/// Knowledge messages sent from server to agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "payload")]
pub enum KnowledgeServerMessage {
    ServiceControl {
        action: ServiceAction,
    },
    IndexProject {
        project_path: String,
        force_reindex: bool,
    },
    Search {
        project_path: String,
        request_id: Uuid,
        query: String,
        tier: SearchTier,
        max_results: Option<u32>,
    },
    ExtractMemory {
        loop_id: AgenticLoopId,
        project_path: String,
        transcript: Vec<TranscriptFragment>,
    },
    GenerateInstructions {
        project_path: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn roundtrip_agent(msg: &KnowledgeAgentMessage) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: KnowledgeAgentMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    fn roundtrip_server(msg: &KnowledgeServerMessage) {
        let json = serde_json::to_string(msg).expect("serialize");
        let parsed: KnowledgeServerMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(*msg, parsed);
    }

    #[test]
    fn service_status_roundtrip() {
        roundtrip_agent(&KnowledgeAgentMessage::ServiceStatus {
            status: KnowledgeServiceStatus::Ready,
            version: Some("0.1.0".to_string()),
            error: None,
        });
        roundtrip_agent(&KnowledgeAgentMessage::ServiceStatus {
            status: KnowledgeServiceStatus::Error,
            version: None,
            error: Some("failed to start".to_string()),
        });
    }

    #[test]
    fn knowledge_base_ready_roundtrip() {
        roundtrip_agent(&KnowledgeAgentMessage::KnowledgeBaseReady {
            project_path: "/home/user/project".to_string(),
            total_files: 150,
            total_chunks: 3200,
        });
    }

    #[test]
    fn indexing_progress_roundtrip() {
        roundtrip_agent(&KnowledgeAgentMessage::IndexingProgress {
            project_path: "/home/user/project".to_string(),
            status: IndexingStatus::InProgress,
            files_processed: 42,
            files_total: 150,
            error: None,
        });
        roundtrip_agent(&KnowledgeAgentMessage::IndexingProgress {
            project_path: "/home/user/project".to_string(),
            status: IndexingStatus::Failed,
            files_processed: 42,
            files_total: 150,
            error: Some("disk full".to_string()),
        });
    }

    #[test]
    fn search_results_roundtrip() {
        roundtrip_agent(&KnowledgeAgentMessage::SearchResults {
            project_path: "/home/user/project".to_string(),
            request_id: Uuid::new_v4(),
            results: vec![
                SearchResult {
                    path: "src/main.rs".to_string(),
                    score: 0.95,
                    snippet: "fn main() { ... }".to_string(),
                    line_start: Some(1),
                    line_end: Some(10),
                    tier: SearchTier::L0,
                },
                SearchResult {
                    path: "src/lib.rs".to_string(),
                    score: 0.72,
                    snippet: "pub mod utils;".to_string(),
                    line_start: None,
                    line_end: None,
                    tier: SearchTier::L1,
                },
            ],
            duration_ms: 42,
        });
        roundtrip_agent(&KnowledgeAgentMessage::SearchResults {
            project_path: "/home/user/project".to_string(),
            request_id: Uuid::new_v4(),
            results: vec![],
            duration_ms: 5,
        });
    }

    #[test]
    fn memory_extracted_roundtrip() {
        roundtrip_agent(&KnowledgeAgentMessage::MemoryExtracted {
            loop_id: Uuid::new_v4(),
            memories: vec![
                ExtractedMemory {
                    key: "error-handling-pattern".to_string(),
                    content: "Always use Result<T, AppError> for route handlers".to_string(),
                    category: MemoryCategory::Pattern,
                    confidence: 0.92,
                    source_loop_id: Uuid::new_v4(),
                },
                ExtractedMemory {
                    key: "no-unwrap-in-prod".to_string(),
                    content: "Never use .unwrap() in production code".to_string(),
                    category: MemoryCategory::Pitfall,
                    confidence: 0.88,
                    source_loop_id: Uuid::new_v4(),
                },
            ],
        });
    }

    #[test]
    fn instructions_generated_roundtrip() {
        roundtrip_agent(&KnowledgeAgentMessage::InstructionsGenerated {
            project_path: "/home/user/project".to_string(),
            content: "# Project Instructions\n\nUse Result types everywhere.".to_string(),
            memories_used: 5,
        });
    }

    #[test]
    fn service_control_roundtrip() {
        roundtrip_server(&KnowledgeServerMessage::ServiceControl {
            action: ServiceAction::Start,
        });
        roundtrip_server(&KnowledgeServerMessage::ServiceControl {
            action: ServiceAction::Stop,
        });
        roundtrip_server(&KnowledgeServerMessage::ServiceControl {
            action: ServiceAction::Restart,
        });
    }

    #[test]
    fn index_project_roundtrip() {
        roundtrip_server(&KnowledgeServerMessage::IndexProject {
            project_path: "/home/user/project".to_string(),
            force_reindex: false,
        });
        roundtrip_server(&KnowledgeServerMessage::IndexProject {
            project_path: "/home/user/project".to_string(),
            force_reindex: true,
        });
    }

    #[test]
    fn search_roundtrip() {
        roundtrip_server(&KnowledgeServerMessage::Search {
            project_path: "/home/user/project".to_string(),
            request_id: Uuid::new_v4(),
            query: "error handling".to_string(),
            tier: SearchTier::L0,
            max_results: Some(10),
        });
        roundtrip_server(&KnowledgeServerMessage::Search {
            project_path: "/home/user/project".to_string(),
            request_id: Uuid::new_v4(),
            query: "database connection".to_string(),
            tier: SearchTier::L2,
            max_results: None,
        });
    }

    #[test]
    fn extract_memory_roundtrip() {
        roundtrip_server(&KnowledgeServerMessage::ExtractMemory {
            loop_id: Uuid::new_v4(),
            project_path: "/home/user/project".to_string(),
            transcript: vec![
                TranscriptFragment {
                    role: "user".to_string(),
                    content: "Fix the bug in main.rs".to_string(),
                    timestamp: chrono::Utc::now(),
                },
                TranscriptFragment {
                    role: "assistant".to_string(),
                    content: "I found the issue...".to_string(),
                    timestamp: chrono::Utc::now(),
                },
            ],
        });
    }

    #[test]
    fn generate_instructions_roundtrip() {
        roundtrip_server(&KnowledgeServerMessage::GenerateInstructions {
            project_path: "/home/user/project".to_string(),
        });
    }

    #[test]
    fn knowledge_service_status_serialization() {
        assert_eq!(
            serde_json::to_string(&KnowledgeServiceStatus::Starting).unwrap(),
            r#""starting""#
        );
        assert_eq!(
            serde_json::to_string(&KnowledgeServiceStatus::Ready).unwrap(),
            r#""ready""#
        );
        assert_eq!(
            serde_json::to_string(&KnowledgeServiceStatus::Indexing).unwrap(),
            r#""indexing""#
        );
        assert_eq!(
            serde_json::to_string(&KnowledgeServiceStatus::Error).unwrap(),
            r#""error""#
        );
        assert_eq!(
            serde_json::to_string(&KnowledgeServiceStatus::Stopped).unwrap(),
            r#""stopped""#
        );
    }

    #[test]
    fn indexing_status_serialization() {
        assert_eq!(
            serde_json::to_string(&IndexingStatus::Queued).unwrap(),
            r#""queued""#
        );
        assert_eq!(
            serde_json::to_string(&IndexingStatus::InProgress).unwrap(),
            r#""in_progress""#
        );
        assert_eq!(
            serde_json::to_string(&IndexingStatus::Completed).unwrap(),
            r#""completed""#
        );
        assert_eq!(
            serde_json::to_string(&IndexingStatus::Failed).unwrap(),
            r#""failed""#
        );
    }

    #[test]
    fn search_tier_serialization() {
        assert_eq!(
            serde_json::to_string(&SearchTier::L0).unwrap(),
            r#""l0""#
        );
        assert_eq!(
            serde_json::to_string(&SearchTier::L1).unwrap(),
            r#""l1""#
        );
        assert_eq!(
            serde_json::to_string(&SearchTier::L2).unwrap(),
            r#""l2""#
        );
    }

    #[test]
    fn memory_category_serialization() {
        assert_eq!(
            serde_json::to_string(&MemoryCategory::Pattern).unwrap(),
            r#""pattern""#
        );
        assert_eq!(
            serde_json::to_string(&MemoryCategory::Decision).unwrap(),
            r#""decision""#
        );
        assert_eq!(
            serde_json::to_string(&MemoryCategory::Pitfall).unwrap(),
            r#""pitfall""#
        );
        assert_eq!(
            serde_json::to_string(&MemoryCategory::Preference).unwrap(),
            r#""preference""#
        );
        assert_eq!(
            serde_json::to_string(&MemoryCategory::Architecture).unwrap(),
            r#""architecture""#
        );
        assert_eq!(
            serde_json::to_string(&MemoryCategory::Convention).unwrap(),
            r#""convention""#
        );
    }

    #[test]
    fn service_action_serialization() {
        assert_eq!(
            serde_json::to_string(&ServiceAction::Start).unwrap(),
            r#""start""#
        );
        assert_eq!(
            serde_json::to_string(&ServiceAction::Stop).unwrap(),
            r#""stop""#
        );
        assert_eq!(
            serde_json::to_string(&ServiceAction::Restart).unwrap(),
            r#""restart""#
        );
    }
}
