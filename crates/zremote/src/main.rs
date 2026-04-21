use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "zremote",
    version,
    about = "ZRemote — remote machine management"
)]
struct Cli {
    /// Override config directory (default: ~/.config/zremote)
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,

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

    /// Command-line interface for managing hosts, sessions, projects, and more
    #[cfg(feature = "cli")]
    Cli {
        #[command(flatten)]
        global: zremote_cli::GlobalOpts,
        #[command(subcommand)]
        command: zremote_cli::Commands,
    },
}

fn main() {
    load_dotenv();
    let cli = Cli::parse();

    match cli.command {
        #[cfg(feature = "gui")]
        Commands::Gui {
            server,
            local,
            port,
            exit_after,
        } => run_gui(&server, local, port, exit_after),

        #[cfg(feature = "agent")]
        Commands::Agent { command } => {
            zremote_agent::run(command);
        }

        #[cfg(feature = "cli")]
        Commands::Cli { global, command } => {
            zremote_cli::run(global, command);
        }
    }
}

/// Pre-scan argv for `--config-dir` before clap parsing.
fn parse_config_dir_from_args() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--config-dir" {
            return args.get(i + 1).map(PathBuf::from);
        }
        if let Some(val) = args[i].strip_prefix("--config-dir=") {
            return Some(PathBuf::from(val));
        }
        i += 1;
    }
    None
}

/// Load environment variables from `<config_dir>/.env` before clap parsing.
///
/// Existing process env vars are NOT overwritten (they take precedence).
/// Silent if .env file does not exist.
fn load_dotenv() {
    let config_dir = parse_config_dir_from_args().unwrap_or_else(|| {
        dirs::config_dir()
            .expect("could not determine config directory")
            .join("zremote")
    });

    let env_path = config_dir.join(".env");

    match dotenvy::from_path(&env_path) {
        Ok(()) => {}
        Err(dotenvy::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            eprintln!("warning: failed to load {}: {e}", env_path.display());
        }
    }
}

#[cfg(feature = "gui")]
fn run_gui(server: &str, local: bool, port: u16, exit_after: Option<u64>) {
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
        zremote_gui::extract_base_url(server)
    };

    // Local mode: read the agent's bearer with a short retry. The agent
    // writes the token BEFORE its /health listener binds, so by the time we
    // get here the file should already exist. The retry loop is defence in
    // depth against filesystem races on slow CI / network home dirs.
    let local_token = if local {
        (0..3).find_map(|i| {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            zremote_gui::local::read_local_token()
        })
    } else {
        None
    };

    zremote_gui::run(zremote_gui::GuiConfig {
        server_url,
        exit_after,
        is_local: local,
        local_token,
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

    // Redirect the spawned agent's stdout+stderr to a log file so users hitting
    // 500s or crashes can actually see what went wrong. Previously both streams
    // went to /dev/null, which made diagnosing local-mode issues impossible
    // without re-launching the agent manually.
    let log_path = agent_log_path();
    let log_file = open_agent_log(&log_path);

    // Spawn the agent as a child process using the same binary. Inherit
    // RUST_LOG if the user set one; otherwise default to info for our crates
    // so the log file is actually useful when something goes wrong.
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["agent", "local", "--port", &port.to_string()])
        .stdin(std::process::Stdio::null());
    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env("RUST_LOG", "zremote_agent=info,zremote_core=info");
    }
    match log_file {
        Some(file) => {
            let stdout = file
                .try_clone()
                .unwrap_or_else(|_| std::fs::File::create("/dev/null").expect("open /dev/null"));
            cmd.stdout(std::process::Stdio::from(stdout))
                .stderr(std::process::Stdio::from(file));
            tracing::info!(log = %log_path.display(), "agent logs streaming to file");
        }
        None => {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
    }

    // Create a new process group so the agent is isolated from the GUI's
    // process group. Prevents process-group signals from the agent reaching
    // the GUI or the desktop session manager.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }

    cmd.spawn()?;

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

/// Path to the agent log file written when the GUI spawns a local agent. Kept
/// next to the local DB so `~/.zremote/` is the single place a user has to
/// look when something goes wrong.
#[cfg(feature = "gui")]
fn agent_log_path() -> PathBuf {
    let base = dirs::home_dir().map_or_else(|| PathBuf::from("."), |h| h.join(".zremote"));
    base.join("agent.log")
}

/// Open the agent log file for append. Creates the parent directory if it
/// doesn't exist. Returns `None` on any IO failure — the caller falls back to
/// /dev/null so a bad permissions / read-only home never blocks startup.
#[cfg(feature = "gui")]
fn open_agent_log(path: &std::path::Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            tracing::warn!(error = %e, path = %path.display(), "failed to open agent log");
            e
        })
        .ok()
}

/// Quick synchronous health check: TCP connect + HTTP GET `/health`, then
/// verify the JSON body contains `"service": "zremote-agent"`.
#[cfg(feature = "gui")]
fn check_health(url: &str) -> bool {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    let Some(addr_str) = url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/health"))
    else {
        return false;
    };

    let Ok(addr) = addr_str.parse::<SocketAddr>() else {
        return false;
    };

    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(200)) else {
        return false;
    };

    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));

    let request = format!("GET /health HTTP/1.1\r\nHost: {addr_str}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut buf = vec![0u8; 4096];
    let mut total = 0;
    loop {
        match stream.read(&mut buf[total..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                total += n;
                if total >= buf.len() {
                    break;
                }
            }
        }
    }

    parse_health_response(&buf[..total])
}

/// Parse an HTTP response and check that the JSON body contains
/// `"service": "zremote-agent"`.
#[cfg(feature = "gui")]
fn parse_health_response(response: &[u8]) -> bool {
    let Ok(response_str) = std::str::from_utf8(response) else {
        return false;
    };

    // Verify HTTP 200 status
    if !response_str.starts_with("HTTP/1.1 200") && !response_str.starts_with("HTTP/1.0 200") {
        return false;
    }

    // Find end of headers
    let Some(pos) = response_str.find("\r\n\r\n") else {
        return false;
    };
    let body = &response_str[pos + 4..];

    // Parse JSON and check service field
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };

    json.get("service").and_then(|v| v.as_str()) == Some("zremote-agent")
}

#[cfg(all(test, feature = "gui"))]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_zremote_response() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
            {\"status\":\"ok\",\"mode\":\"local\",\"hostname\":\"box\",\"service\":\"zremote-agent\",\"version\":\"0.12.6\"}";
        assert!(parse_health_response(response));
    }

    #[test]
    fn parse_missing_service_field() {
        let response = b"HTTP/1.1 200 OK\r\n\r\n{\"status\":\"ok\",\"mode\":\"local\"}";
        assert!(!parse_health_response(response));
    }

    #[test]
    fn parse_wrong_service_value() {
        let response = b"HTTP/1.1 200 OK\r\n\r\n{\"service\":\"something-else\"}";
        assert!(!parse_health_response(response));
    }

    #[test]
    fn parse_non_json_body() {
        let response = b"HTTP/1.1 200 OK\r\n\r\nHello World";
        assert!(!parse_health_response(response));
    }

    #[test]
    fn parse_empty_response() {
        assert!(!parse_health_response(b""));
    }

    #[test]
    fn parse_no_header_separator() {
        let response = b"HTTP/1.1 200 OK";
        assert!(!parse_health_response(response));
    }

    #[test]
    fn parse_non_200_status() {
        let response = b"HTTP/1.1 503 Service Unavailable\r\n\r\n{\"service\":\"zremote-agent\"}";
        assert!(!parse_health_response(response));
    }
}
