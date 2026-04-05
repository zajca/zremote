use clap::Subcommand;
use zremote_client::ApiClient;
use zremote_client::types::CreateWorktreeRequest;

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// List worktrees for a project
    #[command(alias = "ls")]
    List {
        /// Project ID
        project_id: String,
    },
    /// Create a new worktree
    Create {
        /// Project ID
        project_id: String,
        /// Branch name
        #[arg(long)]
        branch: String,
        /// Custom worktree path
        #[arg(long)]
        path: Option<String>,
        /// Create as a new branch
        #[arg(long)]
        new_branch: bool,
    },
    /// Delete a worktree
    Delete {
        /// Project ID
        project_id: String,
        /// Worktree ID
        worktree_id: String,
        /// Force deletion
        #[arg(long)]
        force: bool,
    },
}

pub async fn run(
    client: &ApiClient,
    _resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: WorktreeCommand,
) -> i32 {
    match command {
        WorktreeCommand::List { project_id } => match client.list_worktrees(&project_id).await {
            Ok(worktrees) => {
                println!("{}", fmt.worktrees(&worktrees));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        WorktreeCommand::Create {
            project_id,
            branch,
            path,
            new_branch,
        } => {
            let req = CreateWorktreeRequest {
                branch,
                path,
                new_branch,
            };
            match client.create_worktree(&project_id, &req).await {
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
        WorktreeCommand::Delete {
            project_id,
            worktree_id,
            force,
        } => {
            if !force {
                eprintln!("Use --force to delete worktree {worktree_id}");
                return 1;
            }
            match client.delete_worktree(&project_id, &worktree_id).await {
                Ok(()) => {
                    println!("Worktree {worktree_id} deleted.");
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
