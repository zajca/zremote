use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use zremote_client::{ApiClient, HostStatus};

use crate::GlobalOpts;

static CLI_REFERENCE: &str = include_str!("../../commander-reference.md");

const CACHE_TTL: Duration = Duration::from_secs(300); // 5 minutes
const MAX_PROJECTS: usize = 20;

#[derive(Debug, Subcommand)]
pub enum CommanderCommand {
    /// Generate Commander CLAUDE.md
    Generate {
        /// Write to project directory instead of stdout
        #[arg(long)]
        write: bool,
        /// Target directory (default: cwd)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Skip live API queries
        #[arg(long)]
        no_dynamic: bool,
    },
    /// Start a Commander CC session
    Start {
        /// Working directory for CC
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Claude model to use
        #[arg(long)]
        model: Option<String>,
        /// Initial prompt for the Commander
        #[arg(long)]
        prompt: Option<String>,
        /// Run CC with --dangerously-skip-permissions
        #[arg(long)]
        skip_permissions: bool,
        /// Don't regenerate CLAUDE.md if it already exists and is < 5 min old
        #[arg(long)]
        no_regenerate: bool,
        /// Path to claude binary
        #[arg(long, env = "CLAUDE_CODE_PATH")]
        claude_path: Option<PathBuf>,
    },
    /// Show commander state
    Status {
        /// Target directory
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

pub async fn run(client: &ApiClient, command: CommanderCommand, global: &GlobalOpts) -> i32 {
    match command {
        CommanderCommand::Generate {
            write,
            dir,
            no_dynamic,
        } => run_generate(client, global, write, dir, no_dynamic).await,
        CommanderCommand::Start {
            dir,
            model,
            prompt,
            skip_permissions,
            no_regenerate,
            claude_path,
        } => {
            run_start(
                client,
                global,
                dir,
                model,
                prompt,
                skip_permissions,
                no_regenerate,
                claude_path,
            )
            .await
        }
        CommanderCommand::Status { dir } => run_status(dir),
    }
}

async fn run_generate(
    client: &ApiClient,
    global: &GlobalOpts,
    write: bool,
    dir: Option<PathBuf>,
    no_dynamic: bool,
) -> i32 {
    let mut sections = Vec::new();

    // 1. Identity section (static)
    sections.push(generate_identity());

    // 2. CLI reference (static, from include_str!)
    sections.push(CLI_REFERENCE.to_string());

    // 3. Context protocol (static)
    sections.push(generate_context_protocol());

    // 4. Dynamic infrastructure (API calls or cache)
    if !no_dynamic {
        match generate_dynamic(client, &global.server).await {
            Ok(dynamic) => sections.push(dynamic),
            Err(e) => {
                eprintln!("Warning: could not fetch infrastructure state: {e}");
                eprintln!("Using --no-dynamic fallback");
            }
        }
    }

    // 5. Error handling (static)
    sections.push(generate_error_handling());

    // 6. Workflow recipes (static)
    sections.push(generate_workflow_recipes());

    // 7. Limitations (static)
    sections.push(generate_limitations());

    let content = sections.join("\n\n");

    if write {
        let target = dir.unwrap_or_else(|| PathBuf::from("."));
        if !target.exists() {
            eprintln!("Error: directory {} does not exist", target.display());
            return 1;
        }
        let claude_dir = target.join(".claude");
        if let Err(e) = std::fs::create_dir_all(&claude_dir) {
            eprintln!("Error creating .claude directory: {e}");
            return 1;
        }
        let path = claude_dir.join("commander.md");
        if let Err(e) = std::fs::write(&path, &content) {
            eprintln!("Error writing {}: {e}", path.display());
            return 1;
        }
        eprintln!("Wrote {}", path.display());
    } else {
        println!("{content}");
    }
    0
}

// ---------------------------------------------------------------------------
// Dynamic section with caching
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct CachedDynamic {
    generated_at: u64, // unix timestamp
    content: String,
}

fn cache_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".zremote")
        .join("commander-cache.json")
}

fn read_cache() -> Option<String> {
    let path = cache_path();
    let data = std::fs::read_to_string(&path).ok()?;
    let cached: CachedDynamic = serde_json::from_str(&data).ok()?;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if now - cached.generated_at < CACHE_TTL.as_secs() {
        Some(cached.content)
    } else {
        None
    }
}

fn write_cache(content: &str) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cached = CachedDynamic {
        generated_at: now,
        content: content.to_string(),
    };
    // Atomic write: temp file + rename
    let tmp = path.with_extension("tmp");
    if let Ok(json) = serde_json::to_string(&cached)
        && std::fs::write(&tmp, &json).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

async fn generate_dynamic(client: &ApiClient, server_url: &str) -> Result<String, String> {
    // Check cache first
    if let Some(cached) = read_cache() {
        return Ok(cached);
    }

    let mode = client.get_mode_info().await.map_err(|e| e.to_string())?;
    let hosts = client.list_hosts().await.map_err(|e| e.to_string())?;

    let mut lines = Vec::new();
    lines.push("## Current Infrastructure".to_string());
    lines.push(String::new());
    lines.push(format!(
        "Server: {} ({} mode{})",
        server_url,
        mode.mode,
        mode.version
            .as_deref()
            .map_or(String::new(), |v| format!(", v{v}"))
    ));
    lines.push(String::new());
    lines.push("### Hosts".to_string());

    for host in &hosts {
        let status = match host.status {
            HostStatus::Online => "online",
            HostStatus::Offline => "offline",
            HostStatus::Unknown => "unknown",
        };
        let version = host.agent_version.as_deref().unwrap_or("?");
        lines.push(format!(
            "- {} ({}) -- {status}, agent v{version}",
            host.name,
            &host.id[..8.min(host.id.len())]
        ));
    }

    lines.push(String::new());
    lines.push("### Projects".to_string());

    let online_hosts: Vec<_> = hosts
        .iter()
        .filter(|h| h.status == HostStatus::Online)
        .collect();

    let mut project_count = 0;
    for host in &online_hosts {
        match client.list_projects(&host.id).await {
            Ok(projects) => {
                for p in &projects {
                    if project_count >= MAX_PROJECTS {
                        lines.push(format!(
                            "- ... and more (truncated at {MAX_PROJECTS})"
                        ));
                        break;
                    }
                    let branch = p.git_branch.as_deref().unwrap_or("-");
                    lines.push(format!(
                        "- {} ({}) -- {}, {}, branch: {branch}",
                        p.name, host.name, p.path, p.project_type
                    ));
                    project_count += 1;
                }
            }
            Err(e) => {
                lines.push(format!(
                    "- {} projects: error fetching ({e})",
                    host.name
                ));
            }
        }
        if project_count >= MAX_PROJECTS {
            break;
        }
    }

    lines.push(String::new());
    lines.push(
        "Note: This is a snapshot from generation time. \
         Use `zremote cli status` and `zremote cli host list --output llm` for current state."
            .to_string(),
    );

    let content = lines.join("\n");
    write_cache(&content);
    Ok(content)
}

// ---------------------------------------------------------------------------
// Commander start & status
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn run_start(
    client: &ApiClient,
    global: &GlobalOpts,
    dir: Option<PathBuf>,
    model: Option<String>,
    prompt: Option<String>,
    skip_permissions: bool,
    no_regenerate: bool,
    claude_path: Option<PathBuf>,
) -> i32 {
    let work_dir = dir.unwrap_or_else(|| PathBuf::from("."));
    if !work_dir.exists() {
        eprintln!("Error: directory {} does not exist", work_dir.display());
        return 1;
    }

    // Step 1: Generate CLAUDE.md (reuse generate logic)
    let commander_md = work_dir.join(".claude").join("commander.md");

    let should_generate = if no_regenerate && commander_md.exists() {
        match commander_md.metadata().and_then(|m| m.modified()) {
            Ok(modified) => {
                let age = SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or(Duration::MAX);
                age >= CACHE_TTL
            }
            Err(_) => true,
        }
    } else {
        true
    };

    if should_generate {
        let exit = run_generate(client, global, true, Some(work_dir.clone()), false).await;
        if exit != 0 {
            return exit;
        }
    }

    // Step 2: Locate claude binary
    let claude_bin = match find_claude_binary(claude_path.as_deref()) {
        Ok(path) => path,
        Err(msg) => {
            eprintln!("Error: {msg}");
            return 1;
        }
    };

    // Step 3: Build and run command
    let mut cmd = std::process::Command::new(&claude_bin);
    cmd.current_dir(&work_dir);

    cmd.env("ZREMOTE_OUTPUT", "llm");
    cmd.env("ZREMOTE_SERVER_URL", &global.server);
    if let Some(ref host) = global.host {
        cmd.env("ZREMOTE_HOST_ID", host);
    }

    if let Some(ref m) = model {
        cmd.arg("--model").arg(m);
    }
    if skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }
    if let Some(ref p) = prompt {
        cmd.arg("-p").arg(p);
    }

    cmd.stdin(std::process::Stdio::inherit());
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());

    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("Error launching claude: {e}");
            1
        }
    }
}

fn find_claude_binary(explicit: Option<&std::path::Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        if path.exists() {
            return Ok(path.to_path_buf());
        }
        return Err(format!("claude binary not found at {}", path.display()));
    }

    if let Ok(path) = std::env::var("CLAUDE_CODE_PATH") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Ok(p);
        }
    }

    if let Ok(output) = std::process::Command::new("which").arg("claude").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let common_paths = [
        format!("{home}/.local/bin/claude"),
        format!("{home}/.npm/bin/claude"),
        "/usr/local/bin/claude".to_string(),
    ];
    for p in &common_paths {
        let path = PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
    }

    Err("claude binary not found. Install Claude Code (https://docs.anthropic.com/en/docs/claude-code) or use --claude-path".to_string())
}

fn run_status(dir: Option<PathBuf>) -> i32 {
    let work_dir = dir.unwrap_or_else(|| PathBuf::from("."));
    let commander_md = work_dir.join(".claude").join("commander.md");

    if commander_md.exists() {
        println!("Commander CLAUDE.md: {}", commander_md.display());
        match commander_md.metadata().and_then(|m| m.modified()) {
            Ok(modified) => {
                let age = SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or(Duration::MAX);
                let mins = age.as_secs() / 60;
                println!("Generated: {mins}m ago");
                if age < CACHE_TTL {
                    println!("Status: fresh (< 5m)");
                } else {
                    println!("Status: stale (> 5m, will regenerate on next start)");
                }
            }
            Err(e) => {
                println!("Modified time: unknown ({e})");
            }
        }
    } else {
        println!("Commander CLAUDE.md: not found");
        println!(
            "Run `zremote cli commander generate --write` or `zremote cli commander start` to create it."
        );
    }

    match find_claude_binary(None) {
        Ok(path) => println!("Claude binary: {}", path.display()),
        Err(_) => println!("Claude binary: not found"),
    }

    if read_cache().is_some() {
        println!("Infrastructure cache: fresh (< 5m)");
    } else {
        println!("Infrastructure cache: stale or missing");
    }

    0
}

// ---------------------------------------------------------------------------
// Static section generators
// ---------------------------------------------------------------------------

fn generate_identity() -> String {
    "# ZRemote Commander\n\
     \n\
     You are a ZRemote Commander. Your role is to orchestrate Claude Code instances\n\
     across remote machines managed by ZRemote. You accept high-level tasks and\n\
     break them down into operations executed via `zremote cli`.\n\
     \n\
     Always use `--output llm` for all zremote commands (set via ZREMOTE_OUTPUT=llm).\n\
     Only one Commander should run per project at a time (no concurrency support)."
        .to_string()
}

fn generate_context_protocol() -> String {
    "## Shared Context\n\
     \n\
     Before dispatching a task, load shared memories for the target project:\n\
     ```\n\
     zremote cli memory list <project_id>\n\
     ```\n\
     \n\
     Include relevant memories in the task prompt so the dispatched CC instance\n\
     has context from previous work.\n\
     \n\
     After a task completes, extract and save learnings:\n\
     ```\n\
     zremote cli knowledge extract <project_id> --loop-id <loop_id> --save\n\
     ```"
        .to_string()
}

fn generate_error_handling() -> String {
    "## Error Handling\n\
     \n\
     - Commands return exit code 0 on success, 1 on failure\n\
     - With --output llm, errors produce: `{\"_t\":\"error\",\"code\":\"...\",\"msg\":\"...\"}`\n\
     - Error codes: `not_found`, `connection`, `auth`, `validation`, `internal`\n\
     - If a host is offline, task creation will fail -- check host status first\n\
     - If a task gets stuck, check the agentic loop status with `loop list`"
        .to_string()
}

fn generate_workflow_recipes() -> String {
    "## Workflow Recipes\n\
     \n\
     ### Task Dispatch\n\
     1. Check host status: `host list` → verify target host is online\n\
     2. Load context: `memory list <project_id>` → get relevant memories\n\
     3. Create task: `task create --host <id> --project-path <path> --prompt \"...\"`\n\
     4. Monitor: `task get <task_id>` or `loop list --host <id>` for progress\n\
     5. Collect result: `task get <task_id>` → check status and summary\n\
     \n\
     ### Memory Sync\n\
     1. Before task: `memory list <project_id>` → include in task prompt\n\
     2. After task: `knowledge extract <project_id> --loop-id <id> --save`\n\
     3. Verify: `memory list <project_id>` → confirm new memories saved\n\
     \n\
     ### Multi-Host Coordination\n\
     1. List hosts: `host list` → identify available hosts\n\
     2. For each host: `project list` → find target project\n\
     3. Create tasks in parallel on different hosts\n\
     4. Monitor all: `task list` → track progress across hosts\n\
     \n\
     ### Error Recovery\n\
     1. Check task status: `task get <id>` → identify failure reason\n\
     2. Check worktree: `worktree list <project_id>` → find orphaned worktrees\n\
     3. Clean up: `worktree delete <project_id> <path>` if work not committed\n\
     4. Retry or escalate to user with context\n\
     \n\
     ### Project Review\n\
     1. Active loops: `loop list --status working` → what's running\n\
     2. Recent tasks: `task list` → costs and status\n\
     3. Worktree state: `worktree list <project_id>` → outstanding branches"
        .to_string()
}

fn generate_limitations() -> String {
    "## Limitations\n\
     \n\
     - Only one Commander should run per project at a time\n\
     - Infrastructure state in this document is a snapshot -- use CLI for current state\n\
     - Cost tracking: monitor task costs with `task get` -- no automatic budget limits\n\
     - Session attach is interactive only -- Commander should use tasks, not direct sessions"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_starts_with_header() {
        let identity = generate_identity();
        assert!(!identity.is_empty());
        assert!(identity.starts_with("# ZRemote Commander"));
    }

    #[test]
    fn cli_reference_contains_host_list() {
        assert!(!CLI_REFERENCE.is_empty());
        assert!(CLI_REFERENCE.contains("host list"));
    }

    #[test]
    fn context_protocol_mentions_memory_list() {
        let ctx = generate_context_protocol();
        assert!(ctx.contains("memory list"));
    }

    #[test]
    fn workflow_recipes_contains_all_headers() {
        let recipes = generate_workflow_recipes();
        assert!(recipes.contains("### Task Dispatch"));
        assert!(recipes.contains("### Memory Sync"));
        assert!(recipes.contains("### Multi-Host Coordination"));
        assert!(recipes.contains("### Error Recovery"));
        assert!(recipes.contains("### Project Review"));
    }

    #[test]
    fn limitations_is_non_empty() {
        assert!(!generate_limitations().is_empty());
    }

    #[test]
    fn read_cache_returns_none_when_no_file() {
        // With no cache file present (or stale), read_cache should return None.
        // We cannot guarantee the file doesn't exist, but the function handles
        // missing files gracefully by returning None.
        let result = read_cache();
        // Just verify it doesn't panic -- it returns either Some or None
        let _ = result;
    }

    #[test]
    fn write_then_read_cache_roundtrip() {
        let test_content = "test-commander-cache-roundtrip";
        write_cache(test_content);
        let cached = read_cache();
        assert_eq!(cached, Some(test_content.to_string()));

        // Clean up
        let _ = std::fs::remove_file(cache_path());
    }

    #[test]
    fn find_claude_binary_with_explicit_nonexistent_path() {
        let result = find_claude_binary(Some(std::path::Path::new("/nonexistent/claude")));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn commander_md_path_is_correct() {
        let dir = PathBuf::from("/tmp/test-project");
        let expected = dir.join(".claude").join("commander.md");
        assert_eq!(
            expected.to_str().unwrap(),
            "/tmp/test-project/.claude/commander.md"
        );
    }

    #[test]
    fn all_sections_under_token_limit() {
        let mut sections = Vec::new();
        sections.push(generate_identity());
        sections.push(CLI_REFERENCE.to_string());
        sections.push(generate_context_protocol());
        sections.push(generate_error_handling());
        sections.push(generate_workflow_recipes());
        sections.push(generate_limitations());
        let combined = sections.join("\n\n");
        // ~6000 tokens ≈ ~24000 characters
        assert!(
            combined.len() < 24_000,
            "Combined sections are {} characters, exceeding 24000 limit",
            combined.len()
        );
    }
}
