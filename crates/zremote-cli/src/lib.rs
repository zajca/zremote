//! `ZRemote` CLI — command-line interface for managing hosts, sessions, projects, and more.

mod commands;
mod connection;
pub mod format;
mod terminal;

use clap::{Args, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::connection::ConnectionResolver;
use crate::format::OutputFormat;

/// Global options shared by all CLI commands.
#[derive(Debug, Args)]
pub struct GlobalOpts {
    /// Server URL (http/ws, path auto-stripped)
    #[arg(
        long,
        env = "ZREMOTE_SERVER_URL",
        default_value = "http://localhost:3000",
        global = true
    )]
    pub server: String,

    /// Shorthand for --server <http://127.0.0.1:3000>
    #[arg(long, global = true)]
    pub local: bool,

    /// Target host ID or name prefix (auto-detected in local mode)
    #[arg(long, env = "ZREMOTE_HOST_ID", global = true)]
    pub host: Option<String>,

    /// Output format
    #[arg(long, env = "ZREMOTE_OUTPUT", default_value = "table", global = true)]
    pub output: OutputFormat,

    /// Disable interactive prompts (CI mode)
    #[arg(long, global = true)]
    pub no_interactive: bool,

    /// Suppress non-essential output
    #[arg(short, long, global = true)]
    pub quiet: bool,
}

/// Top-level CLI commands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Manage hosts
    Host {
        #[command(subcommand)]
        command: commands::host::HostCommand,
    },
    /// Manage terminal sessions
    Session {
        #[command(subcommand)]
        command: commands::session::SessionCommand,
    },
    /// Manage projects
    Project {
        #[command(subcommand)]
        command: commands::project::ProjectCommand,
    },
    /// Manage git worktrees
    Worktree {
        #[command(subcommand)]
        command: commands::worktree::WorktreeCommand,
    },
    /// View agentic loops
    Loop {
        #[command(subcommand)]
        command: commands::loop_cmd::LoopCommand,
    },
    /// Manage Claude tasks
    Task {
        #[command(subcommand)]
        command: commands::task::TaskCommand,
    },
    /// Knowledge base operations
    Knowledge {
        #[command(subcommand)]
        command: commands::knowledge::KnowledgeCommand,
    },
    /// Manage extracted memories
    Memory {
        #[command(subcommand)]
        command: commands::memory::MemoryCommand,
    },
    /// Get/set configuration
    Config {
        #[command(subcommand)]
        command: commands::config::ConfigCommand,
    },
    /// Project settings
    Settings {
        #[command(subcommand)]
        command: commands::settings::SettingsCommand,
    },
    /// Project actions
    Action {
        #[command(subcommand)]
        command: commands::action::ActionCommand,
    },
    /// Channel Bridge operations
    Channel {
        #[command(subcommand)]
        command: commands::channel::ChannelCommand,
    },
    /// Commander orchestration
    Commander {
        #[command(subcommand)]
        command: commands::commander::CommanderCommand,
    },
    /// Stream real-time events
    Events {
        /// Filter by event types (comma-separated)
        #[arg(long)]
        filter: Option<String>,
    },
    /// Show server status
    Status,

    // --- Convenience aliases ---
    /// List sessions (alias for `session list`)
    #[command(hide = true)]
    Ps,
    /// Create and attach to a new session
    #[command(hide = true)]
    New {
        /// Shell binary
        #[arg(long)]
        shell: Option<String>,
        /// Starting directory
        #[arg(long)]
        working_dir: Option<String>,
        /// Session display name
        #[arg(long)]
        name: Option<String>,
    },
    /// Attach to a session (alias for `session attach`)
    #[command(hide = true)]
    Ssh {
        /// Session ID
        session_id: String,
    },
    /// List hosts (alias for `host list`)
    #[command(hide = true)]
    Hosts,
    /// List projects (alias for `project list`)
    #[command(hide = true)]
    Projects,
}

/// Entry point for the CLI, called from the unified binary.
pub fn run(global: GlobalOpts, command: Commands) {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .init();

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let exit_code = rt.block_on(run_async(global, command));
    std::process::exit(exit_code);
}

async fn run_async(global: GlobalOpts, command: Commands) -> i32 {
    let resolver = ConnectionResolver::new(&global);
    let client = match resolver.client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };
    let fmt = format::create_formatter(&global);

    match command {
        Commands::Host { command } => commands::host::run(&client, &resolver, &*fmt, command).await,
        Commands::Session { command } => {
            commands::session::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Project { command } => {
            commands::project::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Worktree { command } => {
            commands::worktree::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Loop { command } => {
            commands::loop_cmd::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Task { command } => commands::task::run(&client, &resolver, &*fmt, command).await,
        Commands::Knowledge { command } => {
            commands::knowledge::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Memory { command } => {
            commands::memory::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Config { command } => {
            commands::config::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Settings { command } => {
            commands::settings::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Action { command } => {
            commands::action::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Channel { command } => {
            commands::channel::run(&client, &resolver, &*fmt, command).await
        }
        Commands::Commander { command } => {
            commands::commander::run(&client, command, &global).await
        }
        Commands::Events { filter } => commands::events::run(&client, filter).await,
        Commands::Status => commands::status::run(&client, &*fmt).await,

        // Convenience aliases
        Commands::Ps => {
            commands::session::run(
                &client,
                &resolver,
                &*fmt,
                commands::session::SessionCommand::List { all: false },
            )
            .await
        }
        Commands::New {
            shell,
            working_dir,
            name,
        } => commands::session::run_new(&client, &resolver, shell, working_dir, name).await,
        Commands::Ssh { session_id } => {
            commands::session::run(
                &client,
                &resolver,
                &*fmt,
                commands::session::SessionCommand::Attach { session_id },
            )
            .await
        }
        Commands::Hosts => {
            commands::host::run(&client, &resolver, &*fmt, commands::host::HostCommand::List).await
        }
        Commands::Projects => {
            commands::project::run(
                &client,
                &resolver,
                &*fmt,
                commands::project::ProjectCommand::List,
            )
            .await
        }
    }
}
