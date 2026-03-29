use clap::Subcommand;
use zremote_client::ApiClient;
use zremote_client::types::AddProjectRequest;

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// List all projects
    #[command(alias = "ls")]
    List,
    /// Show project details
    Get {
        /// Project ID
        project_id: String,
    },
    /// Add a project by path
    Add {
        /// Filesystem path
        path: String,
    },
    /// Delete a project
    Delete {
        /// Project ID
        project_id: String,
        /// Skip confirmation
        #[arg(long)]
        confirm: bool,
    },
    /// Trigger project scan
    Scan,
    /// Refresh git metadata for a project
    #[command(name = "git-refresh")]
    GitRefresh {
        /// Project ID
        project_id: String,
    },
    /// List sessions for a project
    Sessions {
        /// Project ID
        project_id: String,
    },
}

#[allow(clippy::too_many_lines)]
pub async fn run(
    client: &ApiClient,
    resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: ProjectCommand,
) -> i32 {
    match command {
        ProjectCommand::List => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client.list_projects(&host_id).await {
                Ok(projects) => {
                    println!("{}", fmt.projects(&projects));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        ProjectCommand::Get { project_id } => match client.get_project(&project_id).await {
            Ok(project) => {
                println!("{}", fmt.project(&project));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        ProjectCommand::Add { path } => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            let req = AddProjectRequest { path };
            match client.add_project(&host_id, &req).await {
                Ok(()) => {
                    println!("Project added.");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        ProjectCommand::Delete {
            project_id,
            confirm,
        } => {
            if !confirm {
                eprintln!("Use --confirm to delete project {project_id}");
                return 1;
            }
            match client.delete_project(&project_id).await {
                Ok(()) => {
                    println!("Project {project_id} deleted.");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        ProjectCommand::Scan => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client.trigger_scan(&host_id).await {
                Ok(()) => {
                    println!("Project scan triggered.");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        ProjectCommand::GitRefresh { project_id } => {
            match client.trigger_git_refresh(&project_id).await {
                Ok(()) => {
                    println!("Git refresh triggered.");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        ProjectCommand::Sessions { project_id } => {
            match client.list_project_sessions(&project_id).await {
                Ok(sessions) => {
                    println!("{}", fmt.sessions(&sessions));
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
