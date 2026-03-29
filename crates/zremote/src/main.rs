use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "zremote",
    version,
    about = "ZRemote — remote machine management"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the desktop GUI
    #[cfg(feature = "gui")]
    Gui {
        /// Server URL (http/ws, path auto-stripped)
        #[arg(
            long,
            env = "ZREMOTE_SERVER_URL",
            default_value = "http://localhost:3000"
        )]
        server: String,

        /// Start a local agent automatically (standalone mode)
        #[arg(long)]
        local: bool,

        /// Port for the local agent (only with --local)
        #[arg(long, default_value = "3000")]
        port: u16,

        /// Auto-exit after N seconds (for headless screenshot capture)
        #[arg(long)]
        exit_after: Option<u64>,
    },

    /// Run the agent (connect to server, local mode, MCP, etc.)
    #[cfg(feature = "agent")]
    Agent {
        #[command(subcommand)]
        command: Option<zremote_agent::Commands>,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        #[cfg(feature = "gui")]
        Commands::Gui {
            server,
            local,
            port,
            exit_after,
        } => run_gui(server, local, port, exit_after),

        #[cfg(feature = "agent")]
        Commands::Agent { command } => {
            zremote_agent::run(command);
        }
    }
}

#[cfg(feature = "gui")]
fn run_gui(server: String, local: bool, port: u16, exit_after: Option<u64>) {
    // Initialize tracing (before anything else)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let server_url = if local {
        // In local mode, spawn the agent as a child process if not already running
        let url = format!("http://127.0.0.1:{port}");
        if let Err(e) = ensure_local_agent(port) {
            tracing::error!(error = %e, "failed to start local agent");
            std::process::exit(1);
        }
        url
    } else {
        zremote_gui::extract_base_url(&server)
    };

    zremote_gui::run(zremote_gui::GuiConfig {
        server_url,
        exit_after,
    });
}

/// Ensure a local agent is running on the given port.
/// If the health endpoint responds, we assume it's already running.
/// Otherwise, spawn `zremote agent local --port <port>` as a detached child process.
#[cfg(feature = "gui")]
fn ensure_local_agent(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::{Duration, Instant};

    let health_url = format!("http://127.0.0.1:{port}/health");

    // Check if agent is already running
    if check_health(&health_url) {
        tracing::info!(port, "local agent already running");
        return Ok(());
    }

    tracing::info!(port, "starting local agent");

    // Spawn the agent as a child process using the same binary
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .args(["agent", "local", "--port", &port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    // Wait for the agent to become healthy (up to 5 seconds)
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if check_health(&health_url) {
            tracing::info!(port, "local agent is ready");
            return Ok(());
        }
    }

    Err("local agent did not become healthy within 5 seconds".into())
}

/// Quick synchronous health check via a blocking TCP connect + HTTP GET.
#[cfg(feature = "gui")]
fn check_health(url: &str) -> bool {
    // Use a simple TCP connect to check if the port is open.
    // We avoid pulling in reqwest/blocking here to keep the facade lightweight.
    let addr = url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/health"))
        .unwrap_or("127.0.0.1:3000");

    std::net::TcpStream::connect_timeout(
        &addr
            .parse()
            .unwrap_or_else(|_| std::net::SocketAddr::from(([127, 0, 0, 1], 3000))),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
}
