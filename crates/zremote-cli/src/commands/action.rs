use clap::Subcommand;
use zremote_client::ApiClient;

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum ActionCommand {
    /// List project actions
    #[command(alias = "ls")]
    List {
        /// Project ID
        project_id: String,
    },
    /// Run a project action
    Run {
        /// Project ID
        project_id: String,
        /// Action name
        action_name: String,
    },
}

pub async fn run(
    client: &ApiClient,
    _resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: ActionCommand,
) -> i32 {
    match command {
        ActionCommand::List { project_id } => match client.list_actions(&project_id).await {
            Ok(actions) => {
                println!("{}", fmt.actions(&actions));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        ActionCommand::Run {
            project_id,
            action_name,
        } => match client.run_action(&project_id, &action_name).await {
            Ok(resp) => {
                let output =
                    serde_json::to_string_pretty(&resp).unwrap_or_else(|e| format!("Error: {e}"));
                println!("{output}");
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
    }
}
