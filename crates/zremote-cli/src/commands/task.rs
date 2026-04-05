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
        /// Enable Channel Bridge for bidirectional communication
        #[arg(long)]
        channel: bool,
        /// Run in non-interactive print mode (answer and exit)
        #[arg(long)]
        print: bool,
    },
    /// Resume an existing task
    Resume {
        /// Task ID
        task_id: String,
        /// Resume prompt
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Send a message to a running task via channel
    Send {
        /// Task ID
        task_id: String,
        /// Message to send
        message: String,
        /// Priority (normal, high, urgent)
        #[arg(long, default_value = "normal", value_parser = ["normal", "high", "urgent"])]
        priority: String,
    },
    /// Approve or deny a permission request
    Approve {
        /// Task ID
        task_id: String,
        /// Permission request ID
        request_id: String,
        /// Decision
        #[arg(value_parser = ["yes", "no"])]
        decision: String,
        /// Reason
        #[arg(long)]
        reason: Option<String>,
    },
    /// Cancel a running task
    Cancel {
        /// Task ID
        task_id: String,
        /// Force cancel without graceful abort
        #[arg(long)]
        force: bool,
    },
    /// Show task output
    Log {
        /// Task ID
        task_id: String,
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
            channel,
            print,
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
                channel_enabled: if channel { Some(true) } else { None },
                print_mode: if print { Some(true) } else { None },
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
        TaskCommand::Send {
            task_id,
            message,
            priority,
        } => {
            const MAX_MESSAGE_LEN: usize = 65_536;
            if message.len() > MAX_MESSAGE_LEN {
                eprintln!(
                    "Error: message too large ({} bytes, max {MAX_MESSAGE_LEN})",
                    message.len()
                );
                return 1;
            }
            let task = match client.get_claude_task(&task_id).await {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            let channel_msg = serde_json::json!({
                "type": "Instruction",
                "from": "cli",
                "content": message,
                "priority": priority,
            });
            match client.channel_send(&task.session_id, &channel_msg).await {
                Ok(()) => {
                    println!("Message sent to task {task_id}");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        TaskCommand::Approve {
            task_id,
            request_id,
            decision,
            reason,
        } => {
            let task = match client.get_claude_task(&task_id).await {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            let allowed = decision == "yes";
            match client
                .channel_permission_respond(
                    &task.session_id,
                    &request_id,
                    allowed,
                    reason.as_deref(),
                )
                .await
            {
                Ok(()) => {
                    let verb = if allowed { "approved" } else { "denied" };
                    println!("Permission request {request_id} {verb}");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        TaskCommand::Cancel { task_id, force } => {
            match client.cancel_claude_task(&task_id, force).await {
                Ok(()) => {
                    println!("Task {task_id} cancelled");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        TaskCommand::Log { task_id } => match client.get_task_log(&task_id).await {
            Ok(log) => {
                print!("{log}");
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
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
