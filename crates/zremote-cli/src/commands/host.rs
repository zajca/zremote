use clap::Subcommand;
use zremote_client::ApiClient;
use zremote_client::types::UpdateHostRequest;

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum HostCommand {
    /// List all hosts
    #[command(alias = "ls")]
    List,
    /// Show host details
    Get {
        /// Host ID
        host_id: String,
    },
    /// Rename a host
    Rename {
        /// Host ID
        host_id: String,
        /// New display name
        new_name: String,
    },
    /// Delete a host
    Delete {
        /// Host ID
        host_id: String,
        /// Skip confirmation
        #[arg(long)]
        confirm: bool,
    },
    /// Browse remote directory
    Browse {
        /// Directory path
        #[arg(long, default_value = "/")]
        path: String,
    },
}

pub async fn run(
    client: &ApiClient,
    resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: HostCommand,
) -> i32 {
    match command {
        HostCommand::List => match client.list_hosts().await {
            Ok(hosts) => {
                println!("{}", fmt.hosts(&hosts));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        HostCommand::Get { host_id } => match client.get_host(&host_id).await {
            Ok(host) => {
                println!("{}", fmt.host(&host));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        HostCommand::Rename { host_id, new_name } => {
            let req = UpdateHostRequest { name: new_name };
            match client.update_host(&host_id, &req).await {
                Ok(host) => {
                    println!("{}", fmt.host(&host));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        HostCommand::Delete { host_id, confirm } => {
            if !confirm {
                eprintln!("Use --confirm to delete host {host_id}");
                return 1;
            }
            match client.delete_host(&host_id).await {
                Ok(()) => {
                    println!("Host {host_id} deleted.");
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        HostCommand::Browse { path } => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client.browse_directory(&host_id, Some(&path)).await {
                Ok(entries) => {
                    println!("{}", fmt.directory_entries(&entries));
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
