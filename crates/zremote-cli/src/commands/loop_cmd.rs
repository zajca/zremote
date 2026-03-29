use clap::Subcommand;
use zremote_client::ApiClient;
use zremote_client::types::ListLoopsFilter;

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

#[derive(Debug, Subcommand)]
pub enum LoopCommand {
    /// List agentic loops
    #[command(alias = "ls")]
    List {
        /// Filter by status
        #[arg(long)]
        status: Option<String>,
        /// Filter by session ID
        #[arg(long)]
        session: Option<String>,
        /// Filter by project ID
        #[arg(long)]
        project: Option<String>,
    },
    /// Show loop details
    Get {
        /// Loop ID
        loop_id: String,
    },
}

pub async fn run(
    client: &ApiClient,
    _resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: LoopCommand,
) -> i32 {
    match command {
        LoopCommand::List {
            status,
            session,
            project,
        } => {
            let filter = ListLoopsFilter {
                status,
                host_id: None,
                session_id: session,
                project_id: project,
            };
            match client.list_loops(&filter).await {
                Ok(loops) => {
                    println!("{}", fmt.loops(&loops));
                    0
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    1
                }
            }
        }
        LoopCommand::Get { loop_id } => match client.get_loop(&loop_id).await {
            Ok(l) => {
                println!("{}", fmt.agentic_loop(&l));
                0
            }
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        },
    }
}
