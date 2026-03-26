// Pre-existing pedantic clippy lints — suppress at crate level for now
#![allow(
    clippy::too_many_lines,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants,
    clippy::redundant_closure_for_method_calls,
    clippy::items_after_statements,
    clippy::needless_continue,
    clippy::doc_markdown,
    clippy::assigning_clones,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    clippy::cast_possible_truncation,
    clippy::map_unwrap_or,
    clippy::needless_pass_by_value,
    clippy::format_push_string,
    clippy::single_match_else,
    clippy::similar_names,
    dead_code,
    unused_imports
)]

mod agentic;
mod ccline;
mod claude;
mod config;
mod connection;
mod daemon;
mod hooks;
mod knowledge;
mod linear;
#[cfg(feature = "local")]
mod local;
mod mcp;
mod project;
mod pty;
mod session;
mod tmux;

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use rand::Rng;
use tracing_subscriber::EnvFilter;

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// Maximum jitter fraction (25%) added to backoff delay.
const JITTER_FRACTION: f64 = 0.25;

#[derive(Default, Parser)]
#[command(name = "zremote-agent", version, about = "ZRemote agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Default, Subcommand)]
enum Commands {
    /// Connect to remote server (default)
    #[default]
    Run,
    /// Run in local mode with HTTP/WS server
    #[cfg(feature = "local")]
    Local {
        /// HTTP/WS listen port
        #[arg(long, default_value = "3000")]
        port: u16,
        /// SQLite database path
        #[arg(long, default_value = "~/.zremote/local.db")]
        db: String,
        /// Bind address
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },
    /// Run as MCP server for Claude Code (stdio transport)
    McpServe {
        /// Project path to serve knowledge for
        #[arg(long)]
        project: PathBuf,
        /// `OpenViking` port
        #[arg(long, default_value = "8741")]
        ov_port: u16,
    },
    /// Configure project settings with Claude
    Configure {
        /// Path to the project to configure
        #[arg(long)]
        project: PathBuf,
        /// Claude model to use
        #[arg(long, default_value = "sonnet")]
        model: String,
        /// Skip Claude Code permission prompts
        #[arg(long)]
        skip_permissions: bool,
    },
    /// Internal: Claude Code status line handler (reads JSON from stdin, outputs formatted status)
    #[command(hide = true)]
    Ccline,
    /// Internal: run as a PTY daemon process (not for direct use)
    #[command(hide = true)]
    PtyDaemon {
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        state_file: PathBuf,
        #[arg(long)]
        shell: String,
        #[arg(long)]
        cols: u16,
        #[arg(long)]
        rows: u16,
        #[arg(long)]
        working_dir: Option<PathBuf>,
        /// Extra environment variables as KEY=VALUE pairs
        #[arg(long = "env")]
        env_vars: Vec<String>,
    },
}

fn main() {
    // Parse CLI first (before tokio) so PtyDaemon can call setsid() before runtime
    let cli = Cli::try_parse().unwrap_or_default();

    // Ccline: synchronous, no tokio runtime needed
    if matches!(cli.command, Some(Commands::Ccline)) {
        ccline::run_ccline();
        return;
    }

    // PtyDaemon: setsid() FIRST, then single-thread tokio runtime
    if let Some(Commands::PtyDaemon {
        session_id,
        socket,
        state_file,
        shell,
        cols,
        rows,
        working_dir,
        env_vars,
    }) = cli.command
    {
        // Validate session_id as UUID to prevent path traversal (e.g. "../")
        if uuid::Uuid::parse_str(&session_id).is_err() {
            eprintln!("invalid session_id: must be a valid UUID");
            std::process::exit(1);
        }

        // setsid() must be called before tokio runtime starts
        if let Err(e) = nix::unistd::setsid() {
            eprintln!("setsid failed: {e}");
            std::process::exit(1);
        }

        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            .json()
            .init();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");

        // Parse --env KEY=VALUE pairs into a HashMap
        let extra_env: std::collections::HashMap<String, String> = env_vars
            .into_iter()
            .filter_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                // Validate env key: must be non-empty, start with letter/underscore,
                // contain only alphanumeric/underscore
                if k.is_empty()
                    || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    || k.starts_with(|c: char| c.is_ascii_digit())
                {
                    tracing::warn!(key = k, "ignoring invalid environment variable key");
                    return None;
                }
                Some((k.to_string(), v.to_string()))
            })
            .collect();

        rt.block_on(daemon::run_pty_daemon(
            session_id,
            socket,
            state_file,
            shell,
            cols,
            rows,
            working_dir,
            extra_env,
        ));
        return;
    }

    // All other commands use the multi-thread runtime
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(async_main(cli));
}

async fn async_main(cli: Cli) {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .json()
        .init();

    match cli.command.unwrap_or_default() {
        Commands::Run => run_agent().await,
        #[cfg(feature = "local")]
        Commands::Local { port, db, bind } => {
            if let Err(e) = local::run_local(port, &db, &bind).await {
                tracing::error!(error = %e, "local mode failed");
                std::process::exit(1);
            }
        }
        Commands::McpServe { project, ov_port } => {
            mcp::run_mcp_server(project, ov_port).await;
        }
        Commands::Configure {
            project,
            model,
            skip_permissions,
        } => {
            run_configure(&project, &model, skip_permissions);
        }
        Commands::Ccline | Commands::PtyDaemon { .. } => unreachable!("handled above"),
    }
}

fn run_configure(project: &std::path::Path, model: &str, skip_permissions: bool) {
    if !project.exists() {
        tracing::error!(path = %project.display(), "project path does not exist");
        std::process::exit(1);
    }

    let project_type = project::configure::detect_project_type(project);
    tracing::info!(
        path = %project.display(),
        project_type,
        "configuring project"
    );

    let existing_json = match project::settings::read_settings(project) {
        Ok(Some(settings)) => serde_json::to_string_pretty(&settings).ok(),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read existing settings, starting fresh");
            None
        }
    };

    let prompt = project::configure::build_configure_prompt(
        &project.display().to_string(),
        project_type,
        existing_json.as_deref(),
    );

    let mut cmd =
        project::configure::build_claude_command(project, model, &prompt, skip_permissions);

    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to execute claude command");
            std::process::exit(1);
        }
    };

    std::process::exit(status.code().unwrap_or(1));
}

async fn run_agent() {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "zremote-agent starting"
    );

    let config = match config::AgentConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load configuration");
            std::process::exit(1);
        }
    };

    let backend = config::detect_persistence_backend();
    match backend {
        config::PersistenceBackend::Daemon => {
            tracing::info!("using PTY daemon backend for persistent sessions");
        }
        config::PersistenceBackend::Tmux => {
            tracing::info!("using tmux backend for persistent sessions");
        }
        config::PersistenceBackend::None => {
            tracing::info!("no persistence backend, using standard PTY sessions");
        }
    }

    // Shutdown signal channel: sender sets to `true` on SIGINT/SIGTERM
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Spawn signal handler
    tokio::spawn(async move {
        if let Err(e) = wait_for_termination_signal().await {
            tracing::error!(error = %e, "failed to listen for shutdown signals");
        }
        tracing::info!("shutdown signal received");
        let _ = shutdown_tx.send(true);
    });

    // Persistent state that survives WebSocket reconnects.
    // These are hoisted above the reconnect loop so PTY sessions, agentic loop
    // state, and CC session mappings are preserved across disconnects.
    let (pty_output_tx, mut pty_output_rx) = tokio::sync::mpsc::channel::<session::PtyOutput>(256);
    let mut session_manager = session::SessionManager::new(pty_output_tx, backend);
    let mut agentic_manager = agentic::manager::AgenticLoopManager::new();
    let session_mapper = hooks::mapper::SessionMapper::new();
    let sent_cc_session_ids = std::sync::Arc::new(tokio::sync::RwLock::new(
        std::collections::HashSet::<String>::new(),
    ));

    // Reconnection loop with exponential backoff
    let mut backoff = MIN_BACKOFF;
    let mut attempt_num: u64 = 0;

    loop {
        if *shutdown_rx.borrow() {
            tracing::info!("shutdown requested, exiting reconnect loop");
            break;
        }

        attempt_num += 1;

        match connection::run_connection(
            &config,
            shutdown_rx.clone(),
            &mut session_manager,
            &mut pty_output_rx,
            &mut agentic_manager,
            &session_mapper,
            &sent_cc_session_ids,
        )
        .await
        {
            Ok(()) => {
                // Clean disconnect (e.g. server closed, or we received shutdown)
                tracing::info!("connection closed cleanly");
                backoff = MIN_BACKOFF;
                attempt_num = 0;

                if *shutdown_rx.borrow() {
                    tracing::info!("shutting down after clean disconnect");
                    break;
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "connection failed");
            }
        }

        if *shutdown_rx.borrow() {
            tracing::info!("shutdown requested, not reconnecting");
            break;
        }

        // Apply jitter: add 0-25% random delay on top of the backoff
        let jitter = {
            let mut rng = rand::rng();
            let jitter_max = backoff.as_secs_f64() * JITTER_FRACTION;
            Duration::from_secs_f64(rng.random_range(0.0..=jitter_max))
        };
        let delay = backoff + jitter;

        tracing::info!(
            attempt = attempt_num,
            retry_in = ?delay,
            "Reconnecting to server..."
        );

        // Wait for either the delay or a shutdown signal
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = shutdown_rx_wait(shutdown_rx.clone()) => {
                tracing::info!("shutdown requested during backoff, exiting");
                break;
            }
        }

        // Exponential backoff: double the delay, cap at MAX_BACKOFF
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }

    // Final cleanup: detach persistent sessions, kill plain PTY
    if session_manager.supports_persistence() {
        session_manager.detach_all();
    } else {
        session_manager.close_all();
    }

    // Remove hooks port file so stale file doesn't mislead CC after agent exit
    if let Err(e) = hooks::server::remove_port_file().await {
        tracing::debug!(error = %e, "failed to remove hooks port file on exit");
    }

    tracing::info!("zremote-agent stopped");
}

/// Wait for SIGINT or SIGTERM.
async fn wait_for_termination_signal() -> Result<(), Box<dyn std::error::Error>> {
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    tokio::select! {
        _ = sigint.recv() => {
            tracing::debug!("received SIGINT");
        }
        _ = sigterm.recv() => {
            tracing::debug!("received SIGTERM");
        }
    }
    Ok(())
}

/// Wait until the shutdown watch channel signals `true`.
async fn shutdown_rx_wait(mut rx: tokio::sync::watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn agent_version_is_set() {
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
