use clap::Subcommand;
use zremote_client::ApiClient;

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Get a global config value
    Get {
        /// Config key
        key: String,
    },
    /// Set a global config value
    Set {
        /// Config key
        key: String,
        /// Config value
        value: String,
    },
    /// Get a host-scoped config value
    #[command(name = "get-host")]
    GetHost {
        /// Config key
        key: String,
    },
    /// Set a host-scoped config value
    #[command(name = "set-host")]
    SetHost {
        /// Config key
        key: String,
        /// Config value
        value: String,
    },
}

pub async fn run(
    client: &ApiClient,
    resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: ConfigCommand,
) -> i32 {
    match command {
        ConfigCommand::Get { key } => match client.get_global_config(&key).await {
            Ok(cv) => {
                println!("{}", fmt.config_value(&cv));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        ConfigCommand::Set { key, value } => match client.set_global_config(&key, &value).await {
            Ok(cv) => {
                println!("{}", fmt.config_value(&cv));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
        ConfigCommand::GetHost { key } => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client.get_host_config(&host_id, &key).await {
                Ok(cv) => {
                    println!("{}", fmt.config_value(&cv));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        ConfigCommand::SetHost { key, value } => {
            let host_id = match resolver.resolve_host_id(client).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            };
            match client.set_host_config(&host_id, &key, &value).await {
                Ok(cv) => {
                    println!("{}", fmt.config_value(&cv));
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
