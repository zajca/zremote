mod agentic;
mod claude;
mod config;
mod connection;
mod hooks;
mod knowledge;
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
#[command(name = "myremote-agent", version, about = "MyRemote agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Default, Subcommand)]
enum Commands {
    /// Connect to remote server (default)
    #[default]
    Run,
    /// Run in local mode with embedded web server
    #[cfg(feature = "local")]
    Local {
        /// HTTP/WS listen port
        #[arg(long, default_value = "3000")]
        port: u16,
        /// SQLite database path
        #[arg(long, default_value = "~/.myremote/local.db")]
        db: String,
        /// Serve web UI from filesystem (for development)
        #[arg(long)]
        web_dir: Option<String>,
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
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .json()
        .init();

    // Parse CLI args; default to Run if no subcommand given
    let cli = Cli::try_parse().unwrap_or_default();

    match cli.command.unwrap_or_default() {
        Commands::Run => run_agent().await,
        #[cfg(feature = "local")]
        Commands::Local {
            port,
            db,
            web_dir,
            bind,
        } => {
            if let Err(e) = local::run_local(port, &db, web_dir.as_deref(), &bind).await {
                tracing::error!(error = %e, "local mode failed");
                std::process::exit(1);
            }
        }
        Commands::McpServe { project, ov_port } => {
            mcp::run_mcp_server(project, ov_port).await;
        }
    }
}

async fn run_agent() {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "myremote-agent starting"
    );

    let config = match config::AgentConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to load configuration");
            std::process::exit(1);
        }
    };

    let use_tmux = config::detect_tmux();
    if use_tmux {
        tracing::info!("tmux detected, persistent sessions enabled");
    } else {
        tracing::info!("tmux not found, using standard PTY sessions");
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

    // Reconnection loop with exponential backoff
    let mut backoff = MIN_BACKOFF;
    let mut attempt_num: u64 = 0;

    loop {
        if *shutdown_rx.borrow() {
            tracing::info!("shutdown requested, exiting reconnect loop");
            break;
        }

        attempt_num += 1;

        match connection::run_connection(&config, shutdown_rx.clone(), use_tmux).await {
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

    tracing::info!("myremote-agent stopped");
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
