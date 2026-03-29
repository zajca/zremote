use clap::Subcommand;
use zremote_client::ApiClient;
use zremote_client::types::{
    CreateClaudeTaskRequest, ListClaudeTasksFilter, ResumeClaudeTaskRequest,
};

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// List Claude tasks
    #[command(alias = "ls")]
    List {
        /// Filter by project ID
        #[arg(long)]
        project: Option<String>,
        /// Filter by status
        #[arg(long)]
        status: Option<String>,
    },
    /// Show task details
    Get {
        /// Task ID
        task_id: String,
    },
    /// Create a new Claude task
    Create {
        /// Project path on the remote host (required)
        #[arg(long)]
        project_path: String,
        /// Project ID (optional, auto-resolved from path)
        #[arg(long)]
        project_id: Option<String>,
        /// Model override
        #[arg(long)]
        model: Option<String>,
        /// Initial prompt
        #[arg(long)]
        prompt: Option<String>,
        /// Comma-separated list of allowed tools
        #[arg(long, value_delimiter = ',')]
        allowed_tools: Option<Vec<String>>,
        /// Skip permission prompts
        #[arg(long)]
        skip_permissions: bool,
        /// Output format (text, json, stream-json)
        #[arg(long)]
        output_format: Option<String>,
        /// Custom CLI flags passed to Claude
        #[arg(long)]
        custom_flags: Option<String>,
    },
    /// Resume an existing task
    Resume {
        /// Task ID
        task_id: String,
        /// Resume prompt
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Discover Claude Code sessions on the host
    Discover {
        /// Project path on the remote host
        #[arg(long)]
        project_path: String,
    },
}

#[allow(clippy::too_many_lines)]
pub async fn run(
    client: &ApiClient,
    resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: TaskCommand,
) -> i32 {
    match command {
        TaskCommand::List { project, status } => {
            let filter = ListClaudeTasksFilter {
                host_id: None,
                status,
                project_id: project,
            };
            match client.list_claude_tasks(&filter).await {
                Ok(tasks) => {
                    println!("{}", fmt.tasks(&tasks));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        TaskCommand::Get { task_id } => match client.get_claude_task(&task_id).await {
            Ok(task) => {
                println!("{}", fmt.task(&task));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        TaskCommand::Create {
            project_path,
            project_id,
            model,
            prompt,
            allowed_tools,
            skip_permissions,
            output_format,
            custom_flags,
        } => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            let req = CreateClaudeTaskRequest {
                host_id,
                project_path,
                project_id,
                model,
                initial_prompt: prompt,
                allowed_tools,
                skip_permissions: if skip_permissions { Some(true) } else { None },
                output_format,
                custom_flags,
            };
            match client.create_claude_task(&req).await {
                Ok(task) => {
                    println!("{}", fmt.task(&task));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        TaskCommand::Resume { task_id, prompt } => {
            let req = ResumeClaudeTaskRequest {
                initial_prompt: prompt,
            };
            match client.resume_claude_task(&task_id, &req).await {
                Ok(task) => {
                    println!("{}", fmt.task(&task));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        TaskCommand::Discover { project_path } => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client
                .discover_claude_sessions(&host_id, &project_path)
                .await
            {
                Ok(sessions) => {
                    let json = serde_json::to_string_pretty(&sessions)
                        .unwrap_or_else(|e| format!("Error serializing: {e}"));
                    println!("{json}");
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
