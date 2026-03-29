use clap::Parser;
use zremote_gui::{GuiConfig, extract_base_url, run};

#[derive(Parser)]
#[command(name = "zremote-gui", version, about = "ZRemote native desktop client")]
struct Cli {
    /// Server URL (same ZREMOTE_SERVER_URL as agent uses, e.g. ws://host:3000/ws/agent
    /// or just http://host:3000). Path is stripped automatically.
    #[arg(
        long,
        env = "ZREMOTE_SERVER_URL",
        default_value = "http://localhost:3000"
    )]
    server: String,

    /// Auto-exit after N seconds (for headless screenshot capture).
    #[arg(long)]
    exit_after: Option<u64>,
}

fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let server_url = extract_base_url(&cli.server);

    run(GuiConfig {
        server_url,
        exit_after: cli.exit_after,
    });
}
