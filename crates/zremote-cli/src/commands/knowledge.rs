use clap::Subcommand;
use zremote_client::types::{
    ExtractRequest, IndexRequest, ListLoopsFilter, SearchRequest, ServiceControlRequest,
};
use zremote_client::{ApiClient, SearchTier};

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

/// Parse a search tier string into the enum.
fn parse_tier(s: &str) -> Result<SearchTier, String> {
    match s.to_uppercase().as_str() {
        "L0" => Ok(SearchTier::L0),
        "L1" => Ok(SearchTier::L1),
        "L2" => Ok(SearchTier::L2),
        _ => Err(format!("unknown tier '{s}' (valid: L0, L1, L2)")),
    }
}

#[derive(Debug, Subcommand)]
pub enum KnowledgeCommand {
    /// Show knowledge base status
    Status {
        /// Project ID
        project_id: String,
    },
    /// Control knowledge service (start/stop/restart)
    Service {
        /// Action: start, stop, or restart
        action: String,
    },
    /// Trigger knowledge indexing
    Index {
        /// Project ID
        project_id: String,
        /// Force full re-index
        #[arg(long)]
        force: bool,
    },
    /// Search the knowledge base
    Search {
        /// Project ID
        project_id: String,
        /// Search query
        query: String,
        /// Search tier filter (L0, L1, L2)
        #[arg(long)]
        tier: Option<String>,
        /// Maximum number of results
        #[arg(long)]
        max_results: Option<u32>,
    },
    /// Bootstrap project knowledge
    Bootstrap {
        /// Project ID
        project_id: String,
    },
    /// Generate CLAUDE.md instructions from memories
    #[command(name = "generate-instructions")]
    GenerateInstructions {
        /// Project ID
        project_id: String,
    },
    /// Write CLAUDE.md on the remote host
    #[command(name = "write-claude-md")]
    WriteClaudeMd {
        /// Project ID
        project_id: String,
    },
    /// Extract memories from an agentic loop transcript
    Extract {
        /// Project ID
        project_id: String,
        /// Agentic loop ID to extract from
        #[arg(long)]
        loop_id: Option<String>,
        /// Session ID (extracts from latest loop in session)
        #[arg(long)]
        session_id: Option<String>,
        /// Automatically save extracted memories to the project
        #[arg(long)]
        save: bool,
    },
}

#[allow(clippy::too_many_lines)]
pub async fn run(
    client: &ApiClient,
    resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: KnowledgeCommand,
) -> i32 {
    match command {
        KnowledgeCommand::Status { project_id } => {
            match client.get_knowledge_status(&project_id).await {
                Ok(Some(kb)) => {
                    println!("{}", fmt.knowledge_status(&kb));
                    0
                }
                Ok(None) => {
                    println!("No knowledge base found for project {project_id}.");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        KnowledgeCommand::Service { action } => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            let req = ServiceControlRequest { action };
            match client.control_knowledge_service(&host_id, &req).await {
                Ok(resp) => {
                    let output = serde_json::to_string_pretty(&resp)
                        .unwrap_or_else(|e| format!("Error: {e}"));
                    println!("{output}");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        KnowledgeCommand::Index { project_id, force } => {
            let req = IndexRequest {
                force_reindex: force,
            };
            match client.trigger_index(&project_id, &req).await {
                Ok(()) => {
                    println!("Indexing triggered for project {project_id}.");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        KnowledgeCommand::Search {
            project_id,
            query,
            tier,
            max_results,
        } => {
            let tier = match tier {
                Some(s) => match parse_tier(&s) {
                    Ok(t) => Some(t),
                    Err(e) => {
                        eprintln!("Error: {e}");
                        return 1;
                    }
                },
                None => None,
            };
            let req = SearchRequest {
                query,
                tier,
                max_results,
            };
            match client.search_knowledge(&project_id, &req).await {
                Ok(results) => {
                    for result in &results {
                        println!("{}", fmt.search_results(result));
                    }
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        KnowledgeCommand::Bootstrap { project_id } => {
            match client.bootstrap_project(&project_id).await {
                Ok(resp) => {
                    let output = serde_json::to_string_pretty(&resp)
                        .unwrap_or_else(|e| format!("Error: {e}"));
                    println!("{output}");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        KnowledgeCommand::GenerateInstructions { project_id } => {
            match client.generate_instructions(&project_id).await {
                Ok(resp) => {
                    let output = serde_json::to_string_pretty(&resp)
                        .unwrap_or_else(|e| format!("Error: {e}"));
                    println!("{output}");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        KnowledgeCommand::WriteClaudeMd { project_id } => {
            match client.write_claude_md(&project_id).await {
                Ok(resp) => {
                    let output = serde_json::to_string_pretty(&resp)
                        .unwrap_or_else(|e| format!("Error: {e}"));
                    println!("{output}");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        KnowledgeCommand::Extract {
            project_id,
            loop_id,
            session_id,
            save,
        } => {
            let resolved_loop_id = match (loop_id, session_id) {
                (Some(lid), _) => lid,
                (None, Some(sid)) => {
                    let filter = ListLoopsFilter {
                        session_id: Some(sid.clone()),
                        ..ListLoopsFilter::default()
                    };
                    match client.list_loops(&filter).await {
                        Ok(loops) if !loops.is_empty() => loops[0].id.clone(),
                        Ok(_) => {
                            eprintln!("Error: no agentic loops found for session {sid}");
                            return 1;
                        }
                        Err(e) => {
                            eprintln!("Error: {e}");
                            return 1;
                        }
                    }
                }
                (None, None) => {
                    eprintln!("Error: either --loop-id or --session-id is required");
                    return 1;
                }
            };

            let req = ExtractRequest {
                loop_id: resolved_loop_id,
            };
            match client.extract_memories(&project_id, &req).await {
                Ok(memories) => {
                    if memories.is_empty() {
                        println!("No memories extracted.");
                    } else {
                        for mem in &memories {
                            let output = serde_json::to_string_pretty(mem)
                                .unwrap_or_else(|e| format!("Error: {e}"));
                            println!("{output}");
                        }
                        println!("\nExtracted {} memories.", memories.len());
                    }
                    if save {
                        println!(
                            "Bulk save not yet supported. Use `memory update` to save individual memories."
                        );
                    }
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
    }
}
