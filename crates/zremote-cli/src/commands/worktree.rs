//! `zremote worktree` (and `wt` alias) subcommand.
//!
//! Implements the Phase 2 CLI surface from RFC-007:
//! - positional `<branch>` for `create`
//! - `--base`, `--path`, `--json`, `--interactive`, `--open`, `--dry-run`
//! - structured error output that mirrors `zremote-protocol`'s `WorktreeError`
//!
//! See `docs/rfc/rfc-007-worktree-ux.md` Phase 2 for the full design.

use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

use clap::Subcommand;
use zremote_client::ApiClient;
use zremote_client::types::CreateWorktreeRequest;
use zremote_protocol::project::{BranchList, WorktreeError, WorktreeErrorCode};

use crate::connection::ConnectionResolver;
use crate::format::Formatter;

/// Exit code conventions for worktree commands.
/// 0 — success, 1 — transport/unexpected error, 2 — structured worktree error,
/// 3 — user-input error (missing required arg, missing guard flag, etc.).
const EXIT_SUCCESS: i32 = 0;
const EXIT_TRANSPORT_ERROR: i32 = 1;
const EXIT_STRUCTURED_ERROR: i32 = 2;
const EXIT_USER_ERROR: i32 = 3;

/// Maximum bytes accepted for a single interactive prompt line, to bound
/// memory if stdin is an unbounded stream (piped file, adversarial input).
const PROMPT_MAX_LEN: u64 = 4096;

#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// List worktrees for a project
    #[command(alias = "ls")]
    List {
        /// Project ID
        project_id: String,
    },
    /// Create a new worktree
    Create {
        /// Project ID
        project_id: String,
        /// Branch name (positional)
        ///
        /// Optional only when `--interactive` is set.
        branch: Option<String>,
        /// Base ref (commit, branch, or tag) for new branch; defaults to HEAD.
        #[arg(long)]
        base: Option<String>,
        /// Custom worktree path (default: auto-suggested)
        #[arg(long)]
        path: Option<String>,
        /// Create as a new branch
        #[arg(long)]
        new_branch: bool,
        /// Emit machine-readable JSON result on stdout
        #[arg(long)]
        json: bool,
        /// Prompt for branch / base / path interactively
        #[arg(long)]
        interactive: bool,
        /// Print a shell command to enter the new worktree on stderr
        #[arg(long)]
        open: bool,
        /// Validate locally without creating (no agent call)
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete a worktree
    Delete {
        /// Project ID
        project_id: String,
        /// Worktree ID
        worktree_id: String,
        /// Force deletion
        #[arg(long)]
        force: bool,
    },
}

pub async fn run(
    client: &ApiClient,
    _resolver: &ConnectionResolver,
    fmt: &dyn Formatter,
    command: WorktreeCommand,
) -> i32 {
    match command {
        WorktreeCommand::List { project_id } => run_list(client, fmt, &project_id).await,
        WorktreeCommand::Create {
            project_id,
            branch,
            base,
            path,
            new_branch,
            json,
            interactive,
            open,
            dry_run,
        } => {
            run_create(
                client,
                &project_id,
                CreateArgs {
                    branch,
                    base,
                    path,
                    new_branch,
                    json,
                    interactive,
                    open,
                    dry_run,
                },
            )
            .await
        }
        WorktreeCommand::Delete {
            project_id,
            worktree_id,
            force,
        } => run_delete(client, &project_id, &worktree_id, force).await,
    }
}

async fn run_list(client: &ApiClient, fmt: &dyn Formatter, project_id: &str) -> i32 {
    match client.list_worktrees(project_id).await {
        Ok(worktrees) => {
            println!("{}", fmt.worktrees(&worktrees));
            EXIT_SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e}");
            EXIT_TRANSPORT_ERROR
        }
    }
}

async fn run_delete(client: &ApiClient, project_id: &str, worktree_id: &str, force: bool) -> i32 {
    if !force {
        eprintln!("Use --force to delete worktree {worktree_id}");
        return EXIT_USER_ERROR;
    }
    match client.delete_worktree(project_id, worktree_id).await {
        Ok(()) => {
            println!("Worktree {worktree_id} deleted.");
            EXIT_SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e}");
            EXIT_TRANSPORT_ERROR
        }
    }
}

/// All the user-supplied inputs to `zremote wt create`, bundled to keep the
/// `run_create` signature readable.
#[allow(clippy::struct_excessive_bools)] // mirror clap flags directly
struct CreateArgs {
    branch: Option<String>,
    base: Option<String>,
    path: Option<String>,
    new_branch: bool,
    json: bool,
    interactive: bool,
    open: bool,
    dry_run: bool,
}

async fn run_create(client: &ApiClient, project_id: &str, args: CreateArgs) -> i32 {
    let CreateArgs {
        branch,
        base,
        path,
        new_branch,
        json,
        interactive,
        open,
        dry_run,
    } = args;

    // Resolve final values; interactive prompts override CLI defaults when
    // empty. Returns early with exit code if resolution fails.
    let resolved = match resolve_inputs(branch, base, path, interactive) {
        Ok(r) => r,
        Err(code) => return code,
    };

    if dry_run {
        return run_dry_run(client, project_id, &resolved, new_branch, json).await;
    }

    let req = CreateWorktreeRequest {
        branch: resolved.branch.clone(),
        path: resolved.path.clone(),
        new_branch,
        base_ref: resolved.base.clone(),
    };

    match client.create_worktree(project_id, &req).await {
        Ok(resp) => {
            let created_path = extract_path(&resp).unwrap_or_else(|| "<unknown>".to_string());
            if json {
                let out = serde_json::json!({
                    "status": "ok",
                    "project_id": project_id,
                    "path": created_path,
                    "branch": resolved.branch,
                });
                println!("{out}");
            } else {
                println!("Created worktree '{}' at {created_path}", resolved.branch);
            }
            if open {
                print_open_hint(&created_path);
            }
            EXIT_SUCCESS
        }
        Err(e) => handle_create_error(&e, json),
    }
}

/// Inputs after interactive prompts and default handling.
struct ResolvedInputs {
    branch: String,
    base: Option<String>,
    path: Option<String>,
}

/// Resolve user inputs: if `--interactive`, prompt for missing values. Error
/// code is returned via `Err` so the caller can exit cleanly.
fn resolve_inputs(
    branch: Option<String>,
    base: Option<String>,
    path: Option<String>,
    interactive: bool,
) -> Result<ResolvedInputs, i32> {
    if interactive {
        let mut stdin = io::stdin();
        let mut stderr = io::stderr();
        let branch = match branch {
            Some(b) if !b.is_empty() => b,
            _ => match prompt(&mut stderr, &mut stdin, "Branch: ", None) {
                Ok(Some(v)) => v,
                Ok(None) => {
                    eprintln!("Error: branch name is required.");
                    return Err(EXIT_USER_ERROR);
                }
                Err(e) => {
                    eprintln!("Error reading branch: {e}");
                    return Err(EXIT_USER_ERROR);
                }
            },
        };
        let base = match base {
            Some(v) if !v.is_empty() => Some(v),
            _ => match prompt(&mut stderr, &mut stdin, "Base ref", Some("HEAD")) {
                Ok(v) => v.filter(|s| s != "HEAD" && !s.is_empty()),
                Err(e) => {
                    eprintln!("Error reading base ref: {e}");
                    return Err(EXIT_USER_ERROR);
                }
            },
        };
        let path = match path {
            Some(v) if !v.is_empty() => Some(v),
            _ => match prompt(&mut stderr, &mut stdin, "Path (blank for auto)", None) {
                Ok(v) => v.filter(|s| !s.is_empty()),
                Err(e) => {
                    eprintln!("Error reading path: {e}");
                    return Err(EXIT_USER_ERROR);
                }
            },
        };
        Ok(ResolvedInputs { branch, base, path })
    } else {
        let branch = match branch {
            Some(b) if !b.is_empty() => b,
            _ => {
                eprintln!("Error: <branch> is required (or use --interactive).");
                return Err(EXIT_USER_ERROR);
            }
        };
        Ok(ResolvedInputs { branch, base, path })
    }
}

/// Write a prompt to `stderr` (keeps stdout JSON-clean) and read one line from
/// `stdin`. `default` is echoed in brackets. Returns `Ok(None)` on EOF.
fn prompt<R: io::Read, W: Write>(
    out: &mut W,
    input: &mut R,
    label: &str,
    default: Option<&str>,
) -> io::Result<Option<String>> {
    if let Some(d) = default {
        write!(out, "{label} [{d}]: ")?;
    } else {
        write!(out, "{label}: ")?;
    }
    out.flush()?;
    let mut line = String::new();
    // Cap at PROMPT_MAX_LEN to bound memory if stdin is piped / adversarial.
    let mut reader = io::BufReader::new(input).take(PROMPT_MAX_LEN);
    let n = io::BufRead::read_line(&mut reader, &mut line)?;
    if n == 0 {
        return Ok(None);
    }
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        Ok(default.map(str::to_string))
    } else {
        Ok(Some(trimmed))
    }
}

/// Extract `path` from a create-worktree response. The agent currently returns
/// either a full `Project` JSON or `{ status: "started", ... }` (command mode).
fn extract_path(value: &serde_json::Value) -> Option<String> {
    value
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn print_open_hint(path: &str) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "$SHELL".to_string());
    // The path originates from the agent response. Strip ASCII control
    // characters (including ESC) so a hostile or buggy server cannot smuggle
    // terminal escapes through our stderr hint. Printable non-ASCII stays
    // intact so UTF-8 paths still render correctly.
    let sanitized: String = path.chars().filter(|c| !c.is_ascii_control()).collect();
    eprintln!("To enter the new worktree: cd {sanitized} && {shell}");
}

/// Dry-run: fetch branches, cross-check the requested branch against local
/// collisions and filesystem, print the plan. Never calls `create_worktree`.
async fn run_dry_run(
    client: &ApiClient,
    project_id: &str,
    resolved: &ResolvedInputs,
    new_branch: bool,
    json: bool,
) -> i32 {
    let branches = match client.list_branches(project_id).await {
        Ok(b) => b,
        Err(e) => {
            // Log the raw error for operator diagnosis; emit a fixed message
            // to stdout/stderr so transport internals never leak to scripted
            // consumers of --json.
            tracing::warn!(error = %e, "worktree list_branches transport error");
            if json {
                let out = serde_json::json!({
                    "status": "error",
                    "code": "transport",
                    "hint": "Could not reach the agent to list branches.",
                    "message": "Transport error — check the server URL, token, and network.",
                });
                println!("{out}");
            } else {
                eprintln!("Error: could not reach the agent to list branches.");
            }
            return EXIT_TRANSPORT_ERROR;
        }
    };

    if let Some(err) = detect_dry_run_collision(&branches, resolved, new_branch) {
        emit_structured_error(&err, json);
        return EXIT_STRUCTURED_ERROR;
    }

    emit_dry_run_plan(resolved, new_branch, &branches.current, json);
    EXIT_SUCCESS
}

/// Scan `branches` + the filesystem for conflicts. First hit wins. `None` when
/// clear to proceed.
fn detect_dry_run_collision(
    branches: &BranchList,
    resolved: &ResolvedInputs,
    new_branch: bool,
) -> Option<WorktreeError> {
    if new_branch && branches.local.iter().any(|b| b.name == resolved.branch) {
        return Some(WorktreeError::new(
            WorktreeErrorCode::BranchExists,
            "Pick a different branch name or drop --new-branch to reuse the existing one.",
            format!("Local branch '{}' already exists.", resolved.branch),
        ));
    }
    if !new_branch && !branches.local.iter().any(|b| b.name == resolved.branch) {
        return Some(WorktreeError::new(
            WorktreeErrorCode::InvalidRef,
            "Pass --new-branch to create it, or choose an existing branch.",
            format!("Local branch '{}' does not exist.", resolved.branch),
        ));
    }
    if let Some(base_ref) = resolved.base.as_deref()
        && !base_ref.is_empty()
        && !base_ref_resolvable(branches, base_ref)
    {
        return Some(WorktreeError::new(
            WorktreeErrorCode::InvalidRef,
            "Check that the base branch, tag, or commit exists.",
            format!("Base ref '{base_ref}' could not be resolved from local/remote branches."),
        ));
    }
    if let Some(path) = resolved.path.as_deref()
        && Path::new(path).exists()
    {
        return Some(WorktreeError::new(
            WorktreeErrorCode::PathCollision,
            "Choose a different target path — the current one is already in use.",
            format!("Target path '{path}' already exists."),
        ));
    }
    None
}

/// A base-ref is resolvable for the dry-run if it matches any local or remote
/// branch name we know about. We intentionally do NOT treat bare SHAs as
/// unresolvable here — the agent is the source of truth and the dry-run only
/// flags likely problems without the server round-trip.
fn base_ref_resolvable(branches: &BranchList, base_ref: &str) -> bool {
    // SHA-ish (hex). Git's minimum abbreviated commit length is 7 chars; lower
    // thresholds risk misclassifying short branch names like "fix" as SHAs.
    if base_ref.len() >= 7 && base_ref.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    branches.local.iter().any(|b| b.name == base_ref)
        || branches.remote.iter().any(|b| b.name == base_ref)
}

fn emit_dry_run_plan(
    resolved: &ResolvedInputs,
    new_branch: bool,
    current_branch: &str,
    json: bool,
) {
    let base_display = resolved.base.as_deref().unwrap_or("HEAD");
    let path_display = resolved.path.as_deref().unwrap_or("<auto>");
    if json {
        let out = serde_json::json!({
            "status": "ok",
            "dry_run": true,
            "branch": resolved.branch,
            "new_branch": new_branch,
            "base": base_display,
            "path": path_display,
            "current_branch": current_branch,
        });
        println!("{out}");
    } else {
        println!("Dry run — no worktree created.");
        println!("  Branch:        {}", resolved.branch);
        println!("  New branch:    {new_branch}");
        println!("  Base ref:      {base_display}");
        println!("  Path:          {path_display}");
        println!("  Current HEAD:  {current_branch}");
    }
}

fn handle_create_error(e: &zremote_client::ApiError, json: bool) -> i32 {
    if let Some(err) = try_parse_worktree_error(e) {
        emit_structured_error(&err, json);
        EXIT_STRUCTURED_ERROR
    } else {
        // Log raw transport error for operator diagnosis; never emit it to
        // stdout/stderr so scripted consumers of --json get a stable shape
        // and no transport internals leak.
        tracing::warn!(error = %e, "worktree create_worktree transport error");
        if json {
            let out = serde_json::json!({
                "status": "error",
                "code": "transport",
                "hint": "Check the server URL, token, and network.",
                "message": "Transport error — check the server URL, token, and network.",
            });
            println!("{out}");
        } else {
            eprintln!("Error: transport error — check the server URL, token, and network.");
        }
        EXIT_TRANSPORT_ERROR
    }
}

/// If `ApiError` carries a server-side JSON body, try to deserialize it into a
/// `WorktreeError`. `None` when the body is absent or not a structured error.
fn try_parse_worktree_error(e: &zremote_client::ApiError) -> Option<WorktreeError> {
    match e {
        zremote_client::ApiError::ServerError { message, .. } => {
            serde_json::from_str::<WorktreeError>(message).ok()
        }
        _ => None,
    }
}

fn emit_structured_error(err: &WorktreeError, json: bool) {
    if json {
        let out = serde_json::json!({
            "status": "error",
            "code": err.code,
            "hint": err.hint,
            "message": err.message,
        });
        // JSON goes to stdout; stderr stays quiet in --json mode.
        println!("{out}");
    } else {
        let code_str = code_as_str(&err.code);
        let tty = io::stderr().is_terminal();
        if tty {
            // Red + bold for the code, reset for hint.
            // ANSI: \x1b[31;1m … \x1b[0m
            eprintln!("\x1b[31;1m{code_str}:\x1b[0m {}", err.hint);
        } else {
            eprintln!("{code_str}: {}", err.hint);
        }
        if !err.message.is_empty() {
            eprintln!("  {}", err.message);
        }
    }
}

fn code_as_str(code: &WorktreeErrorCode) -> &'static str {
    match code {
        WorktreeErrorCode::BranchExists => "branch_exists",
        WorktreeErrorCode::PathCollision => "path_collision",
        WorktreeErrorCode::DetachedHead => "detached_head",
        WorktreeErrorCode::Locked => "locked",
        WorktreeErrorCode::Unmerged => "unmerged",
        WorktreeErrorCode::InvalidRef => "invalid_ref",
        WorktreeErrorCode::Internal => "internal",
        WorktreeErrorCode::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use zremote_protocol::project::Branch;

    fn branch(name: &str) -> Branch {
        Branch {
            name: name.to_string(),
            is_current: false,
            ahead: 0,
            behind: 0,
        }
    }

    fn branchlist(local: Vec<&str>, remote: Vec<&str>, current: &str) -> BranchList {
        BranchList {
            local: local.into_iter().map(branch).collect(),
            remote: remote.into_iter().map(branch).collect(),
            current: current.to_string(),
            remote_truncated: false,
        }
    }

    // ------------------------------------------------------------------
    // Collision detection
    // ------------------------------------------------------------------

    #[test]
    fn wt_dry_run_collision_detects_branch_exists() {
        let branches = branchlist(vec!["main", "feature-x"], vec![], "main");
        let resolved = ResolvedInputs {
            branch: "feature-x".to_string(),
            base: None,
            path: None,
        };
        let err = detect_dry_run_collision(&branches, &resolved, true).expect("collision");
        assert_eq!(err.code, WorktreeErrorCode::BranchExists);
        assert!(err.message.contains("feature-x"));
    }

    #[test]
    fn wt_dry_run_collision_detects_invalid_existing_branch() {
        // new_branch=false but the branch doesn't exist locally
        let branches = branchlist(vec!["main"], vec![], "main");
        let resolved = ResolvedInputs {
            branch: "missing".to_string(),
            base: None,
            path: None,
        };
        let err = detect_dry_run_collision(&branches, &resolved, false).expect("collision");
        assert_eq!(err.code, WorktreeErrorCode::InvalidRef);
    }

    #[test]
    fn wt_dry_run_collision_detects_unknown_base_ref() {
        let branches = branchlist(vec!["main"], vec!["origin/main"], "main");
        let resolved = ResolvedInputs {
            branch: "new-feature".to_string(),
            base: Some("origin/missing".to_string()),
            path: None,
        };
        let err = detect_dry_run_collision(&branches, &resolved, true).expect("collision");
        assert_eq!(err.code, WorktreeErrorCode::InvalidRef);
        assert!(err.message.contains("origin/missing"));
    }

    #[test]
    fn wt_dry_run_collision_accepts_sha_base_ref() {
        let branches = branchlist(vec!["main"], vec![], "main");
        let resolved = ResolvedInputs {
            branch: "new-feature".to_string(),
            base: Some("deadbeef".to_string()),
            path: None,
        };
        assert!(detect_dry_run_collision(&branches, &resolved, true).is_none());
    }

    #[test]
    fn wt_dry_run_collision_rejects_short_hex_like_ref() {
        // Short (<7 char) hex-looking strings are NOT treated as SHAs — e.g.
        // a 4-char branch name like "beef" or "abc1" must surface as an
        // InvalidRef collision rather than silently passing through.
        let branches = branchlist(vec!["main"], vec![], "main");
        let resolved = ResolvedInputs {
            branch: "new-feature".to_string(),
            base: Some("beef".to_string()),
            path: None,
        };
        let err = detect_dry_run_collision(&branches, &resolved, true).expect("collision");
        assert_eq!(err.code, WorktreeErrorCode::InvalidRef);
    }

    #[test]
    fn wt_dry_run_collision_detects_path_collision() {
        let branches = branchlist(vec!["main"], vec![], "main");
        // /tmp always exists on Unix test runners.
        let resolved = ResolvedInputs {
            branch: "new-feature".to_string(),
            base: None,
            path: Some("/tmp".to_string()),
        };
        let err = detect_dry_run_collision(&branches, &resolved, true).expect("collision");
        assert_eq!(err.code, WorktreeErrorCode::PathCollision);
    }

    #[test]
    fn wt_dry_run_ok_when_clear() {
        let branches = branchlist(vec!["main"], vec!["origin/main"], "main");
        let resolved = ResolvedInputs {
            branch: "brand-new".to_string(),
            base: Some("origin/main".to_string()),
            path: None, // let agent auto-suggest
        };
        assert!(detect_dry_run_collision(&branches, &resolved, true).is_none());
    }

    // ------------------------------------------------------------------
    // Structured error output
    // ------------------------------------------------------------------

    #[test]
    fn wt_json_output_on_error_uses_structured_fields() {
        let err = WorktreeError::new(
            WorktreeErrorCode::BranchExists,
            "pick another",
            "branch already exists",
        );
        let api_err = zremote_client::ApiError::ServerError {
            status: reqwest::StatusCode::CONFLICT,
            message: serde_json::to_string(&err).unwrap(),
        };
        let parsed = try_parse_worktree_error(&api_err).expect("parse");
        assert_eq!(parsed.code, WorktreeErrorCode::BranchExists);
        assert_eq!(parsed.hint, "pick another");
    }

    #[test]
    fn wt_transport_error_does_not_parse_as_worktree_error() {
        let api_err = zremote_client::ApiError::ServerError {
            status: reqwest::StatusCode::BAD_GATEWAY,
            message: "upstream offline".to_string(),
        };
        assert!(try_parse_worktree_error(&api_err).is_none());
    }

    #[test]
    fn wt_code_as_str_matches_serde_name() {
        assert_eq!(
            code_as_str(&WorktreeErrorCode::BranchExists),
            "branch_exists"
        );
        assert_eq!(code_as_str(&WorktreeErrorCode::InvalidRef), "invalid_ref");
        assert_eq!(code_as_str(&WorktreeErrorCode::Internal), "internal");
    }

    // ------------------------------------------------------------------
    // Clap parsing (alias + flags)
    // ------------------------------------------------------------------

    /// A minimal clap harness that exposes just `worktree` and `wt` so the
    /// alias test doesn't have to boot the full top-level CLI.
    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: TestCommands,
    }

    #[derive(Debug, clap::Subcommand)]
    enum TestCommands {
        #[command(visible_alias = "wt")]
        Worktree {
            #[command(subcommand)]
            command: WorktreeCommand,
        },
    }

    #[test]
    fn wt_alias_delegates_to_worktree_subcommand() {
        let parsed = TestCli::try_parse_from([
            "zremote",
            "wt",
            "create",
            "proj-123",
            "feature-x",
            "--new-branch",
        ])
        .expect("wt alias should parse");

        let TestCommands::Worktree { command } = parsed.command;
        match command {
            WorktreeCommand::Create {
                project_id,
                branch,
                new_branch,
                ..
            } => {
                assert_eq!(project_id, "proj-123");
                assert_eq!(branch.as_deref(), Some("feature-x"));
                assert!(new_branch);
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn wt_create_supports_all_new_flags() {
        let parsed = TestCli::try_parse_from([
            "zremote",
            "worktree",
            "create",
            "pid",
            "br",
            "--base",
            "main",
            "--path",
            "/tmp/wt-test-xyz-does-not-exist",
            "--json",
            "--dry-run",
            "--open",
            "--new-branch",
        ])
        .expect("flags should parse");
        let TestCommands::Worktree { command } = parsed.command;
        match command {
            WorktreeCommand::Create {
                base,
                path,
                json,
                dry_run,
                open,
                new_branch,
                ..
            } => {
                assert_eq!(base.as_deref(), Some("main"));
                assert_eq!(path.as_deref(), Some("/tmp/wt-test-xyz-does-not-exist"));
                assert!(json);
                assert!(dry_run);
                assert!(open);
                assert!(new_branch);
            }
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn wt_interactive_allows_missing_branch() {
        // When --interactive is set, branch positional is optional.
        let parsed = TestCli::try_parse_from(["zremote", "wt", "create", "pid", "--interactive"])
            .expect("interactive without branch should parse");
        let TestCommands::Worktree { command } = parsed.command;
        if let WorktreeCommand::Create {
            interactive,
            branch,
            ..
        } = command
        {
            assert!(interactive);
            assert!(branch.is_none());
        } else {
            panic!("expected Create");
        }
    }

    // ------------------------------------------------------------------
    // Interactive prompt helper
    // ------------------------------------------------------------------

    #[test]
    fn prompt_returns_default_on_empty_line() {
        let mut out: Vec<u8> = Vec::new();
        let mut input: &[u8] = b"\n";
        let v = prompt(&mut out, &mut input, "Base", Some("HEAD")).unwrap();
        assert_eq!(v.as_deref(), Some("HEAD"));
    }

    #[test]
    fn prompt_returns_trimmed_value() {
        let mut out: Vec<u8> = Vec::new();
        let mut input: &[u8] = b"  main  \n";
        let v = prompt(&mut out, &mut input, "Base", Some("HEAD")).unwrap();
        assert_eq!(v.as_deref(), Some("main"));
    }

    #[test]
    fn prompt_returns_none_on_eof() {
        let mut out: Vec<u8> = Vec::new();
        let mut input: &[u8] = b"";
        let v = prompt(&mut out, &mut input, "Branch", None).unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn prompt_caps_input_at_prompt_max_len() {
        // A malicious / buggy stdin providing a very long line without a
        // newline must not blow up the process: PROMPT_MAX_LEN stops the
        // read at the cap. The resulting String's byte length must be
        // <= PROMPT_MAX_LEN.
        let mut out: Vec<u8> = Vec::new();
        let long = vec![b'x'; (PROMPT_MAX_LEN as usize) * 4];
        let mut input: &[u8] = &long;
        let v = prompt(&mut out, &mut input, "Branch", None).unwrap();
        let got = v.expect("some value");
        assert!(
            got.len() <= PROMPT_MAX_LEN as usize,
            "expected <= {PROMPT_MAX_LEN} bytes, got {}",
            got.len()
        );
    }
}
