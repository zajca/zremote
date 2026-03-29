use clap::Subcommand;
use zremote_client::ApiClient;
use zremote_client::types::ProjectSettings;

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Get project settings
    Get {
        /// Project ID
        project_id: String,
    },
    /// Save project settings from a JSON file
    Save {
        /// Project ID
        project_id: String,
        /// Path to JSON file with settings
        #[arg(long)]
        file: String,
    },
    /// Configure project with Claude AI
    Configure {
        /// Project ID
        project_id: String,
    },
}

pub async fn run(
    client: &ApiClient,
    _resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: SettingsCommand,
) -> i32 {
    match command {
        SettingsCommand::Get { project_id } => match client.get_settings(&project_id).await {
            Ok(settings) => {
                println!("{}", fmt.settings(&settings));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        SettingsCommand::Save { project_id, file } => {
            let content = match tokio::fs::read_to_string(&file).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error reading file '{file}': {e}");
                    return 1;
                }
            };
            let settings: ProjectSettings = match serde_json::from_str(&content) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error parsing JSON: {e}");
                    return 1;
                }
            };
            match client.save_settings(&project_id, &settings).await {
                Ok(saved) => {
                    println!("{}", fmt.settings(&saved));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        SettingsCommand::Configure { project_id } => {
            match client.configure_with_claude(&project_id).await {
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
    }
}
