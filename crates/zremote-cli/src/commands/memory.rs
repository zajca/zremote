use clap::Subcommand;
use zremote_client::types::UpdateMemoryRequest;
use zremote_client::{ApiClient, MemoryCategory};

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

/// Parse a memory category string into the enum.
fn parse_category(s: &str) -> Result<MemoryCategory, String> {
    match s.to_lowercase().as_str() {
        "pattern" => Ok(MemoryCategory::Pattern),
        "decision" => Ok(MemoryCategory::Decision),
        "pitfall" => Ok(MemoryCategory::Pitfall),
        "preference" => Ok(MemoryCategory::Preference),
        "architecture" => Ok(MemoryCategory::Architecture),
        "convention" => Ok(MemoryCategory::Convention),
        _ => Err(format!(
            "unknown category '{s}' (valid: pattern, decision, pitfall, preference, architecture, convention)"
        )),
    }
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// List memories for a project
    #[command(alias = "ls")]
    List {
        /// Project ID
        project_id: String,
        /// Filter by category
        #[arg(long)]
        category: Option<String>,
    },
    /// Update a memory
    Update {
        /// Project ID
        project_id: String,
        /// Memory ID
        memory_id: String,
        /// New content
        #[arg(long)]
        content: Option<String>,
        /// New category (pattern, decision, pitfall, preference, architecture, convention)
        #[arg(long)]
        category: Option<String>,
    },
    /// Delete a memory
    Delete {
        /// Project ID
        project_id: String,
        /// Memory ID
        memory_id: String,
    },
}

pub async fn run(
    client: &ApiClient,
    _resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: MemoryCommand,
) -> i32 {
    match command {
        MemoryCommand::List {
            project_id,
            category,
        } => match client.list_memories(&project_id, category.as_deref()).await {
            Ok(memories) => {
                println!("{}", fmt.memories(&memories));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        MemoryCommand::Update {
            project_id,
            memory_id,
            content,
            category,
        } => {
            let category = match category {
                Some(s) => match parse_category(&s) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        eprintln!("Error: {e}");
                        return 1;
                    }
                },
                None => None,
            };
            let req = UpdateMemoryRequest { content, category };
            match client.update_memory(&project_id, &memory_id, &req).await {
                Ok(memory) => {
                    println!("{}", fmt.memory(&memory));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        MemoryCommand::Delete {
            project_id,
            memory_id,
        } => match client.delete_memory(&project_id, &memory_id).await {
            Ok(()) => {
                println!("Memory {memory_id} deleted.");
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
    }
}
