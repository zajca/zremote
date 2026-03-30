pub mod format;
pub mod input;
pub mod listener;
pub mod socket;
pub mod types;

use std::io::Read;
use std::process::Command;

/// Get the current git branch name from the working directory.
/// Returns `None` if not in a git repo or command fails.
fn git_branch(cwd: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() {
            None
        } else {
            Some(branch)
        }
    } else {
        None
    }
}

/// Dimmed fallback shown when no meaningful status data is available.
const FALLBACK_STATUS: &str = "\x1b[2m[zremote]\x1b[0m";

/// Entry point for the `ccline` subcommand.
/// Reads Claude Code status line JSON from stdin, formats ANSI output to stdout,
/// and forwards telemetry to the agent via Unix socket.
///
/// Always outputs at least a fallback placeholder so the status line area never
/// disappears in Claude Code.
pub fn run_ccline() {
    // Read all of stdin
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        println!("{FALLBACK_STATUS}");
        return;
    }

    // Parse JSON (show fallback on parse failure so status line stays visible)
    let status_input: input::StatusInput = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            println!("{FALLBACK_STATUS}");
            return;
        }
    };

    // Get git branch from cwd
    let branch = status_input.cwd.as_deref().and_then(git_branch);

    // Format and print status line (always output something so Claude Code
    // keeps showing the status line area).
    let status = format::format_status(&status_input, branch.as_deref());
    if status.is_empty() {
        println!("{FALLBACK_STATUS}");
    } else {
        println!("{status}");
    }

    // Forward raw JSON to ZRemote agent via Unix socket
    socket::send_to_agent(raw.as_bytes());
}
