use std::path::{Path, PathBuf};
use std::process::Command;

use zremote_protocol::project::{Branch, BranchList, GitInfo, GitRemote, WorktreeInfo};

/// Maximum wall time for any individual git subprocess. Kills the child on
/// expiry so a hung command (network, file-lock, misconfigured credential
/// helper) cannot wedge the scanner/refresh loop.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Run a git command in the given directory with a 5-second wall-clock
/// timeout. Returns stdout as a trimmed String on success, or an error
/// message. Disables every interactive credential prompt path (terminal +
/// GUI askpass) so a repo with a broken remote can't block the caller
/// waiting for human input.
fn run_git(path: &Path, args: &[&str]) -> Result<String, String> {
    // `Command::spawn` fails with ENOENT both when the binary is missing AND
    // when `current_dir` points at a nonexistent directory — the two are
    // indistinguishable from the caller's perspective. Pre-check the path so
    // the error message tells users the real problem.
    if !path.exists() {
        return Err(format!("path does not exist: {}", path.display()));
    }
    let mut child = Command::new("git")
        .args(args)
        .current_dir(path)
        // Prevent parent repo or env vars from interfering
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        // Prevent git from discovering repos above the target path
        .env("GIT_CEILING_DIRECTORIES", path)
        // Disable every interactive credential prompt path. Without these
        // git happily blocks on a TTY prompt (or pops a desktop askpass)
        // when a remote needs auth — which would deadlock the scanner.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn git: {e}"))?;

    // Poll until the child exits or the deadline passes. On timeout we kill
    // the child and report the failure up. `wait_with_output` is called
    // after the poll confirms the child is gone, so the OS has buffered
    // stdout/stderr for us to drain.
    let deadline = std::time::Instant::now() + GIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    // Collect the process so we don't leak a zombie. Ignore
                    // any read errors — we only care that it's reaped.
                    let _ = child.wait_with_output();
                    return Err(format!(
                        "git {args:?} timed out after {}s",
                        GIT_TIMEOUT.as_secs()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => return Err(format!("git wait failed: {e}")),
        }
    }

    match child.wait_with_output() {
        Ok(output) => {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                Err(stderr)
            }
        }
        Err(e) => Err(format!("git command failed: {e}")),
    }
}

/// Strip credentials from a remote URL.
/// `https://user:token@github.com/repo` -> `https://github.com/repo`
/// SSH URLs (git@...) are returned as-is.
pub fn sanitize_remote_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://")
        && let Some(at_pos) = rest.find('@')
    {
        return format!("https://{}", &rest[at_pos + 1..]);
    }
    if let Some(rest) = url.strip_prefix("http://")
        && let Some(at_pos) = rest.find('@')
    {
        return format!("http://{}", &rest[at_pos + 1..]);
    }
    url.to_string()
}

/// Parse `git worktree list --porcelain` output into `Vec<WorktreeInfo>`.
pub fn parse_worktree_list(output: &str) -> Vec<WorktreeInfo> {
    let mut worktrees = Vec::new();
    let mut path = None;
    let mut commit_hash = None;
    let mut branch = None;
    let mut is_detached = false;
    let mut is_locked = false;

    for line in output.lines() {
        if line.is_empty() {
            // End of a worktree block
            if let Some(p) = path.take() {
                worktrees.push(WorktreeInfo {
                    path: p,
                    branch: branch.take(),
                    commit_hash: commit_hash.take(),
                    is_detached,
                    is_locked,
                    is_dirty: false,
                    commit_message: None,
                });
            }
            is_detached = false;
            is_locked = false;
        } else if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(p.to_string());
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            // Store short hash (first 7 chars)
            commit_hash = Some(h.chars().take(7).collect());
        } else if let Some(b) = line.strip_prefix("branch ") {
            // Strip refs/heads/ prefix
            branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        } else if line == "detached" {
            is_detached = true;
        } else if line == "locked" || line.starts_with("locked ") {
            is_locked = true;
        }
    }

    // Handle last block if no trailing newline
    if let Some(p) = path {
        worktrees.push(WorktreeInfo {
            path: p,
            branch,
            commit_hash,
            is_detached,
            is_locked,
            is_dirty: false,
            commit_message: None,
        });
    }

    worktrees
}

/// Parse `git remote -v` output into `Vec<GitRemote>` (fetch URLs only, deduped).
pub fn parse_remotes(output: &str) -> Vec<GitRemote> {
    let mut remotes = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in output.lines() {
        // Format: "origin\thttps://github.com/user/repo.git (fetch)"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let name = parts[0];
            let url = parts[1];
            // Only include fetch entries (or if no fetch/push marker)
            let is_fetch = parts.len() < 3 || parts[2] == "(fetch)";
            if is_fetch && seen.insert(name.to_string()) {
                remotes.push(GitRemote {
                    name: name.to_string(),
                    url: sanitize_remote_url(url),
                });
            }
        }
    }

    remotes
}

/// Enrich parsed worktrees with dirty state and commit message.
pub fn enrich_worktrees(worktrees: &mut [WorktreeInfo]) {
    for wt in worktrees.iter_mut() {
        let path = std::path::Path::new(&wt.path);
        if path.is_dir() {
            wt.is_dirty = run_git(path, &["status", "--porcelain"])
                .map(|output| !output.is_empty())
                .unwrap_or(false);
            wt.commit_message = run_git(path, &["log", "-1", "--format=%s"])
                .ok()
                .filter(|s| !s.is_empty());
        }
    }
}

/// Compute (ahead, behind) of `target` relative to `base`. Both are branch
/// names (short form — `main`, `origin/main`, etc). Returns `None` when git
/// refuses the comparison (missing ref, no common ancestor, empty repo).
fn ahead_behind(path: &Path, base: &str, target: &str) -> Option<(u32, u32)> {
    let spec = format!("{base}...{target}");
    let output = run_git(path, &["rev-list", "--left-right", "--count", &spec]).ok()?;
    let parts: Vec<&str> = output.split_whitespace().collect();
    if parts.len() != 2 {
        return None;
    }
    // `git rev-list --left-right --count A...B` prints "<left>\t<right>":
    //   * left  = commits reachable from A but not B = how far B is *behind* A
    //   * right = commits reachable from B but not A = how far B is *ahead*  of A
    // We invoke this with A = `base` and B = `target`, so parts[0] is the
    // behind count and parts[1] is the ahead count from target's perspective.
    let behind = parts[0].parse().ok()?;
    let ahead = parts[1].parse().ok()?;
    Some((ahead, behind))
}

/// Git inspector for collecting repository metadata and managing worktrees.
pub struct GitInspector;

impl GitInspector {
    /// Collect full git metadata for a path. Returns None if not a git repo
    /// or git is unavailable.
    pub fn inspect(path: &Path) -> Option<(GitInfo, Vec<WorktreeInfo>)> {
        // Verify this is a git repo
        if run_git(path, &["rev-parse", "--is-inside-work-tree"]).is_err() {
            return None;
        }

        let branch = run_git(path, &["branch", "--show-current"])
            .ok()
            .filter(|s| !s.is_empty());

        let commit_hash = run_git(path, &["rev-parse", "--short", "HEAD"]).ok();

        let commit_message = run_git(path, &["log", "-1", "--format=%s"])
            .ok()
            .filter(|s| !s.is_empty());

        let is_dirty = run_git(path, &["status", "--porcelain"])
            .map(|s| !s.is_empty())
            .unwrap_or(false);

        let (ahead, behind) = run_git(
            path,
            &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
        )
        .ok()
        .and_then(|s| {
            let parts: Vec<&str> = s.split('\t').collect();
            if parts.len() == 2 {
                let behind = parts[0].parse().unwrap_or(0);
                let ahead = parts[1].parse().unwrap_or(0);
                Some((ahead, behind))
            } else {
                None
            }
        })
        .unwrap_or((0, 0));

        let remotes_output = run_git(path, &["remote", "-v"]).unwrap_or_default();
        let remotes = parse_remotes(&remotes_output);

        let worktree_output =
            run_git(path, &["worktree", "list", "--porcelain"]).unwrap_or_default();
        let mut worktrees = parse_worktree_list(&worktree_output);

        // Remove the main worktree (first entry is always the main repo itself)
        if !worktrees.is_empty() {
            worktrees.remove(0);
        }

        enrich_worktrees(&mut worktrees);

        let git_info = GitInfo {
            branch,
            commit_hash,
            commit_message,
            is_dirty,
            ahead,
            behind,
            remotes,
        };

        Some((git_info, worktrees))
    }

    /// Fast-path git inspection used by the periodic refresh loop.
    ///
    /// Collects only the fields that change frequently during normal work —
    /// branch, dirty flag, ahead/behind — and skips the slower bits that the
    /// full `inspect` path does (remote resolution, worktree listing,
    /// commit-message lookup). Returns `None` when the path is not a git
    /// repo or git itself is unavailable; a repo without an upstream still
    /// yields `Some` with `ahead = 0`, `behind = 0`.
    ///
    /// Leaves `commit_hash`, `commit_message`, and `remotes` unset — callers
    /// that need those fields must use the full `inspect` path.
    pub fn inspect_fast(path: &Path) -> Option<GitInfo> {
        if run_git(path, &["rev-parse", "--is-inside-work-tree"]).is_err() {
            return None;
        }

        let branch = run_git(path, &["branch", "--show-current"])
            .ok()
            .filter(|s| !s.is_empty());

        let is_dirty = run_git(path, &["status", "--porcelain"])
            .map(|s| !s.is_empty())
            .unwrap_or(false);

        let (ahead, behind) = run_git(
            path,
            &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
        )
        .ok()
        .and_then(|s| {
            let parts: Vec<&str> = s.split('\t').collect();
            if parts.len() == 2 {
                let behind = parts[0].parse().unwrap_or(0);
                let ahead = parts[1].parse().unwrap_or(0);
                Some((ahead, behind))
            } else {
                None
            }
        })
        .unwrap_or((0, 0));

        Some(GitInfo {
            branch,
            commit_hash: None,
            commit_message: None,
            is_dirty,
            ahead,
            behind,
            remotes: Vec::new(),
        })
    }

    /// Create a new worktree.
    ///
    /// When `new_branch` is true, `base_ref` (if provided) is used as the
    /// starting point for the new branch. For existing branches, `base_ref`
    /// is ignored — git uses the branch itself.
    pub fn create_worktree(
        repo_path: &Path,
        branch: &str,
        worktree_path: Option<&Path>,
        new_branch: bool,
        base_ref: Option<&str>,
    ) -> Result<WorktreeInfo, String> {
        let default_path = repo_path.parent().unwrap_or(repo_path).join(format!(
            "{}-{}",
            repo_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("worktree"),
            branch.replace('/', "-")
        ));
        let wt_path = worktree_path.unwrap_or(&default_path);

        let wt_path_str = wt_path.to_str().ok_or("invalid worktree path")?;

        // Defence in depth against argument injection (CWE-88): insert a `--`
        // separator before the first user-controlled positional so git treats
        // every subsequent token as a positional, not a flag. The API
        // boundary already rejects leading-dash values; this is belt-and-braces
        // in case a future caller reaches this helper through another path.
        let mut args = vec!["worktree", "add"];
        if new_branch {
            args.push("-b");
            args.push(branch);
        }
        args.push("--");
        args.push(wt_path_str);
        if new_branch {
            if let Some(base) = base_ref {
                args.push(base);
            }
        } else {
            args.push(branch);
        }

        run_git(repo_path, &args)?;

        // Read the resulting worktree info
        let wt_branch = run_git(wt_path, &["branch", "--show-current"])
            .ok()
            .filter(|s| !s.is_empty());
        let wt_hash = run_git(wt_path, &["rev-parse", "--short", "HEAD"]).ok();

        let wt_path_str = wt_path.to_string_lossy().to_string();
        let is_dirty = run_git(wt_path, &["status", "--porcelain"])
            .map(|output| !output.is_empty())
            .unwrap_or(false);
        let commit_message = run_git(wt_path, &["log", "-1", "--format=%s"])
            .ok()
            .filter(|s| !s.is_empty());

        Ok(WorktreeInfo {
            path: wt_path_str,
            branch: wt_branch,
            commit_hash: wt_hash,
            is_detached: false,
            is_locked: false,
            is_dirty,
            commit_message,
        })
    }

    /// List local and remote branches for a repo, with ahead/behind counts
    /// against the current branch. Returns empty lists (and empty `current`)
    /// for empty or brand-new repos without any commits.
    ///
    /// Both the local and remote ref listings are capped at
    /// `BRANCH_LIST_CAP` entries so a pathological repo cannot pin the
    /// calling thread forever. When the remote set is larger than
    /// `REMOTE_AHEAD_BEHIND_CAP` we skip the per-branch `rev-list` call —
    /// each remote branch still appears in the output (name only, zeroed
    /// counts) but `BranchList.remote_truncated` is set so clients can
    /// surface the degraded state.
    pub fn list_branches(path: &Path) -> Result<BranchList, String> {
        /// Hard ceiling on refs emitted by `for-each-ref`. Chosen to match
        /// the largest realistic monorepo while still bounding memory.
        const BRANCH_LIST_CAP: &str = "--count=500";
        /// Above this many remote branches we skip per-branch ahead/behind
        /// to avoid running hundreds of `git rev-list` processes on every
        /// request.
        const REMOTE_AHEAD_BEHIND_CAP: usize = 50;

        // Verify this is a git repo. Consistent with inspect() we treat this
        // as a hard error (caller should check) rather than silently empty.
        run_git(path, &["rev-parse", "--is-inside-work-tree"])?;

        // The "current" short name — empty string when HEAD is detached or the
        // repo has no commits yet.
        let current = run_git(path, &["branch", "--show-current"]).unwrap_or_default();

        // Local branches. `%(refname:short)` gives "main" not "refs/heads/main".
        let local_output = run_git(
            path,
            &[
                "for-each-ref",
                BRANCH_LIST_CAP,
                "--format=%(refname:short)",
                "refs/heads",
            ],
        )
        .unwrap_or_default();

        let local = local_output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|name| {
                let is_current = name == current;
                // Ahead/behind only make sense relative to a different branch.
                // For the current branch itself both are zero.
                let (ahead, behind) = if is_current || current.is_empty() {
                    (0, 0)
                } else {
                    ahead_behind(path, &current, name).unwrap_or((0, 0))
                };
                Branch {
                    name: name.to_string(),
                    is_current,
                    ahead,
                    behind,
                }
            })
            .collect();

        // Remote branches (e.g. "origin/main"). Filter HEAD symrefs which
        // for-each-ref emits as "origin/HEAD -> origin/main" artefacts.
        let remote_output = run_git(
            path,
            &[
                "for-each-ref",
                BRANCH_LIST_CAP,
                "--format=%(refname:short)",
                "refs/remotes",
            ],
        )
        .unwrap_or_default();

        let remote_names: Vec<&str> = remote_output
            .lines()
            .filter(|l| !l.is_empty() && !l.ends_with("/HEAD"))
            .collect();

        // If the remote set is large, skip the O(N) rev-list calls and just
        // return names with zeroed counts. Clients observe the degraded mode
        // via `BranchList.remote_truncated`.
        let remote_truncated = remote_names.len() > REMOTE_AHEAD_BEHIND_CAP;

        let remote = remote_names
            .into_iter()
            .map(|name| {
                let (ahead, behind) = if remote_truncated || current.is_empty() {
                    (0, 0)
                } else {
                    ahead_behind(path, &current, name).unwrap_or((0, 0))
                };
                Branch {
                    name: name.to_string(),
                    is_current: false,
                    ahead,
                    behind,
                }
            })
            .collect();

        Ok(BranchList {
            local,
            remote,
            current,
            remote_truncated,
        })
    }

    /// Remove an existing worktree.
    pub fn remove_worktree(
        repo_path: &Path,
        worktree_path: &Path,
        force: bool,
    ) -> Result<(), String> {
        let wt_str = worktree_path.to_str().ok_or("invalid worktree path")?;
        if force {
            run_git(repo_path, &["worktree", "remove", "--force", wt_str])?;
        } else {
            run_git(repo_path, &["worktree", "remove", wt_str])?;
        }
        Ok(())
    }
}

/// If `path` is a linked git worktree, return the absolute path of its main
/// repository (top-level of the main worktree). Returns None if not a linked
/// worktree, if it's a submodule, or if git inspection fails.
pub fn main_repo_path_for_worktree(path: &Path) -> Option<PathBuf> {
    let git_entry = path.join(".git");
    // A main repo has `.git` as a directory; a linked worktree has `.git` as a file.
    if !git_entry.is_file() {
        return None;
    }

    // Cap the read to 4 KiB — legitimate `.git` pointer files are ~70 bytes.
    // Prevents a malicious large regular file from driving memory pressure.
    let mut buf = [0u8; 4096];
    let n = {
        use std::io::Read;
        let mut f = std::fs::File::open(&git_entry).ok()?;
        f.read(&mut buf).ok()?
    };
    let contents = std::str::from_utf8(&buf[..n]).ok()?;
    let gitdir_line = contents
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:").map(str::trim))?;

    // Submodules: gitdir points into `.git/modules/<name>`. Not a worktree.
    if gitdir_line.contains("/modules/") {
        return None;
    }

    // Linked worktrees: gitdir ends with `/.git/worktrees/<name>`.
    if !gitdir_line.contains("/worktrees/") {
        return None;
    }

    // Resolve the gitdir path (may be relative to the worktree dir).
    let gitdir_path = Path::new(gitdir_line);
    let absolute_gitdir = if gitdir_path.is_absolute() {
        gitdir_path.to_path_buf()
    } else {
        path.join(gitdir_path)
    };

    // Try canonicalizing first; fall back to lexical cleanup if that fails
    // (e.g., the worktree metadata moved but the main repo still exists).
    let gitdir_resolved = std::fs::canonicalize(&absolute_gitdir).unwrap_or(absolute_gitdir);

    // Strip trailing `/worktrees/<name>` → `<something>/.git`.
    let dot_git = gitdir_resolved
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)?;

    // Defense in depth: `gitdir:` content is user-controlled (any file the
    // scanner walks). A crafted `gitdir: /etc/worktrees/x` passes the
    // `/worktrees/` filter above; verify that the stripped path actually
    // ends in a `.git` component before treating it as a real repo root.
    if dot_git.file_name().and_then(|n| n.to_str()) != Some(".git") {
        return None;
    }

    // Strip trailing `.git` → main repo working tree.
    let main_repo = dot_git.parent().map(Path::to_path_buf)?;

    let canonical_main = std::fs::canonicalize(&main_repo).unwrap_or(main_repo);

    if !canonical_main.is_dir() {
        return None;
    }

    // Make sure the main repo isn't the same directory as `path`.
    let canonical_self = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if canonical_main == canonical_self {
        return None;
    }

    Some(canonical_main)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Initialize a git repo with an initial commit.
    fn init_git_repo(dir: &Path) {
        run_git(dir, &["init"]).expect("git init");
        run_git(dir, &["config", "user.email", "test@test.com"]).expect("git config email");
        run_git(dir, &["config", "user.name", "Test"]).expect("git config name");
        run_git(dir, &["config", "commit.gpgsign", "false"]).expect("git config gpgsign");
        fs::write(dir.join("README.md"), "# Test").expect("write README");
        run_git(dir, &["add", "."]).expect("git add");
        // --no-verify prevents the parent repo's pre-commit hook from running
        run_git(dir, &["commit", "--no-verify", "-m", "initial commit"]).expect("git commit");
    }

    #[test]
    fn sanitize_remote_url_strips_https_credentials() {
        assert_eq!(
            sanitize_remote_url("https://user:token123@github.com/user/repo.git"),
            "https://github.com/user/repo.git"
        );
    }

    #[test]
    fn sanitize_remote_url_strips_http_credentials() {
        assert_eq!(
            sanitize_remote_url("http://user:pass@example.com/repo.git"),
            "http://example.com/repo.git"
        );
    }

    #[test]
    fn sanitize_remote_url_preserves_ssh() {
        let url = "git@github.com:user/repo.git";
        assert_eq!(sanitize_remote_url(url), url);
    }

    #[test]
    fn sanitize_remote_url_preserves_clean_https() {
        let url = "https://github.com/user/repo.git";
        assert_eq!(sanitize_remote_url(url), url);
    }

    #[test]
    fn parse_worktree_list_basic() {
        let output = "\
worktree /home/user/repo
HEAD abc1234567890abcdef1234567890abcdef1234567
branch refs/heads/main

worktree /home/user/repo-feature
HEAD def5678901234567890abcdef1234567890abcdef
branch refs/heads/feature/new

";
        let wts = parse_worktree_list(output);
        assert_eq!(wts.len(), 2);
        assert_eq!(wts[0].path, "/home/user/repo");
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert_eq!(wts[0].commit_hash.as_deref(), Some("abc1234"));
        assert!(!wts[0].is_detached);

        assert_eq!(wts[1].path, "/home/user/repo-feature");
        assert_eq!(wts[1].branch.as_deref(), Some("feature/new"));
        assert_eq!(wts[1].commit_hash.as_deref(), Some("def5678"));
    }

    #[test]
    fn parse_worktree_list_detached_and_locked() {
        let output = "\
worktree /home/user/repo
HEAD abc1234567890abcdef1234567890abcdef1234567
branch refs/heads/main

worktree /home/user/repo-detached
HEAD def5678901234567890abcdef1234567890abcdef
detached
locked

";
        let wts = parse_worktree_list(output);
        assert_eq!(wts.len(), 2);
        assert!(wts[1].is_detached);
        assert!(wts[1].is_locked);
        assert!(wts[1].branch.is_none());
    }

    #[test]
    fn parse_worktree_list_empty() {
        assert!(parse_worktree_list("").is_empty());
    }

    #[test]
    fn parse_remotes_basic() {
        let output = "\
origin\thttps://github.com/user/repo.git (fetch)
origin\thttps://github.com/user/repo.git (push)
upstream\tgit@github.com:org/repo.git (fetch)
upstream\tgit@github.com:org/repo.git (push)
";
        let remotes = parse_remotes(output);
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[0].name, "origin");
        assert_eq!(remotes[0].url, "https://github.com/user/repo.git");
        assert_eq!(remotes[1].name, "upstream");
        assert_eq!(remotes[1].url, "git@github.com:org/repo.git");
    }

    #[test]
    fn parse_remotes_with_credentials() {
        let output = "origin\thttps://user:token@github.com/user/repo.git (fetch)\n\
                       origin\thttps://user:token@github.com/user/repo.git (push)\n";
        let remotes = parse_remotes(output);
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].url, "https://github.com/user/repo.git");
    }

    #[test]
    fn parse_remotes_empty() {
        assert!(parse_remotes("").is_empty());
    }

    #[test]
    fn inspect_non_git_dir_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(GitInspector::inspect(tmp.path()).is_none());
    }

    #[test]
    fn inspect_git_repo() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let result = GitInspector::inspect(tmp.path());
        assert!(result.is_some());

        let (info, worktrees) = result.unwrap();
        // Should have a branch (default branch created by git init)
        assert!(info.branch.is_some());
        assert!(info.commit_hash.is_some());
        assert_eq!(info.commit_message.as_deref(), Some("initial commit"));
        assert!(!info.is_dirty);
        assert_eq!(info.ahead, 0);
        assert_eq!(info.behind, 0);
        assert!(worktrees.is_empty());
    }

    #[test]
    fn inspect_dirty_repo() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("new_file.txt"), "dirty").unwrap();

        let (info, _) = GitInspector::inspect(tmp.path()).unwrap();
        assert!(info.is_dirty);
    }

    #[test]
    fn create_and_remove_worktree() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        fs::create_dir_all(&repo_path).unwrap();
        init_git_repo(&repo_path);

        let wt_path = tmp.path().join("test-worktree");
        let wt =
            GitInspector::create_worktree(&repo_path, "test-branch", Some(&wt_path), true, None)
                .expect("create worktree");

        assert_eq!(wt.path, wt_path.to_string_lossy());
        assert_eq!(wt.branch.as_deref(), Some("test-branch"));
        assert!(!wt.is_detached);
        assert!(!wt.is_locked);

        // Verify worktree shows up in inspect
        let (_, worktrees) = GitInspector::inspect(&repo_path).unwrap();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].branch.as_deref(), Some("test-branch"));

        // Remove the worktree
        GitInspector::remove_worktree(&repo_path, &wt_path, false).expect("remove worktree");

        let (_, worktrees) = GitInspector::inspect(&repo_path).unwrap();
        assert!(worktrees.is_empty());
    }

    #[test]
    fn create_worktree_existing_branch() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        fs::create_dir_all(&repo_path).unwrap();
        init_git_repo(&repo_path);

        // Create a branch first
        run_git(&repo_path, &["branch", "existing-branch"]).unwrap();

        let wt_path = tmp.path().join("existing-wt");
        let wt = GitInspector::create_worktree(
            &repo_path,
            "existing-branch",
            Some(&wt_path),
            false,
            None,
        )
        .expect("create worktree from existing branch");

        assert_eq!(wt.branch.as_deref(), Some("existing-branch"));
    }

    #[test]
    fn create_worktree_auto_path() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("myrepo");
        fs::create_dir_all(&repo_path).unwrap();
        init_git_repo(&repo_path);

        let wt = GitInspector::create_worktree(&repo_path, "auto-branch", None, true, None)
            .expect("create worktree with auto path");

        assert!(wt.path.contains("myrepo-auto-branch"));

        // Clean up
        GitInspector::remove_worktree(&repo_path, Path::new(&wt.path), true).ok();
    }

    #[test]
    fn run_git_nonexistent_path() {
        let result = run_git(Path::new("/nonexistent/path/xyz"), &["status"]);
        assert!(result.is_err());
    }

    #[test]
    fn main_repo_path_for_worktree_returns_main_for_linked_worktree() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        fs::create_dir_all(&main).unwrap();
        init_git_repo(&main);

        let wt = tmp.path().join("wt");
        run_git(
            &main,
            &["worktree", "add", "-b", "feat", wt.to_str().unwrap()],
        )
        .expect("git worktree add");

        let result = main_repo_path_for_worktree(&wt).expect("should detect main repo");
        let expected = fs::canonicalize(&main).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn main_repo_path_for_worktree_returns_none_for_main_repo() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        assert!(main_repo_path_for_worktree(tmp.path()).is_none());
    }

    #[test]
    fn main_repo_path_for_worktree_returns_none_for_non_git_dir() {
        let tmp = TempDir::new().unwrap();
        assert!(main_repo_path_for_worktree(tmp.path()).is_none());
    }

    #[test]
    fn main_repo_path_for_worktree_returns_none_for_submodule() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".git"), "gitdir: ../../.git/modules/foo\n").unwrap();
        assert!(main_repo_path_for_worktree(tmp.path()).is_none());
    }

    #[test]
    fn main_repo_path_for_worktree_returns_none_for_missing_git_entry() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        assert!(main_repo_path_for_worktree(&sub).is_none());
    }

    #[test]
    fn inspect_fast_returns_none_for_non_git_dir() {
        let tmp = TempDir::new().unwrap();
        assert!(GitInspector::inspect_fast(tmp.path()).is_none());
    }

    #[test]
    fn inspect_fast_returns_none_for_missing_path() {
        let missing = Path::new("/nonexistent/path/zremote-inspect-fast-test");
        assert!(GitInspector::inspect_fast(missing).is_none());
    }

    #[test]
    fn inspect_fast_clean_repo_has_zero_ahead_behind() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let info = GitInspector::inspect_fast(tmp.path()).expect("should detect git repo");
        assert!(info.branch.is_some());
        assert!(!info.is_dirty);
        assert_eq!(info.ahead, 0);
        assert_eq!(info.behind, 0);
        // Fast path intentionally skips these fields.
        assert!(info.commit_hash.is_none());
        assert!(info.commit_message.is_none());
        assert!(info.remotes.is_empty());
    }

    #[test]
    fn inspect_fast_detects_dirty_working_tree() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("dirty.txt"), "edit").unwrap();

        let info = GitInspector::inspect_fast(tmp.path()).expect("should detect dirty repo");
        assert!(info.is_dirty);
    }

    #[test]
    fn list_branches_returns_local_remote_and_current() {
        let tmp = TempDir::new().unwrap();
        // upstream plays the role of "origin".
        let upstream = tmp.path().join("upstream");
        fs::create_dir_all(&upstream).unwrap();
        init_git_repo(&upstream);

        // Clone so the clone has remote-tracking refs ("origin/...").
        let clone = tmp.path().join("clone");
        let status = std::process::Command::new("git")
            .args(["clone", "--quiet"])
            .arg(&upstream)
            .arg(&clone)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CEILING_DIRECTORIES", tmp.path())
            .status()
            .expect("git clone");
        assert!(status.success());

        run_git(&clone, &["config", "user.email", "c@c.com"]).unwrap();
        run_git(&clone, &["config", "user.name", "c"]).unwrap();
        run_git(&clone, &["config", "commit.gpgsign", "false"]).unwrap();

        // Create a second local branch so local list has >1 entry.
        run_git(&clone, &["branch", "feature"]).unwrap();

        let list = GitInspector::list_branches(&clone).expect("list branches");
        assert!(!list.current.is_empty(), "current should be set");
        let local_names: Vec<&str> = list.local.iter().map(|b| b.name.as_str()).collect();
        assert!(local_names.contains(&list.current.as_str()));
        assert!(local_names.contains(&"feature"));

        // Current branch reports itself as is_current=true with 0/0.
        let cur = list.local.iter().find(|b| b.is_current).unwrap();
        assert_eq!(cur.ahead, 0);
        assert_eq!(cur.behind, 0);

        // Remote list has at least one origin/* entry, none marked is_current.
        assert!(!list.remote.is_empty());
        assert!(list.remote.iter().all(|b| !b.is_current));
        assert!(list.remote.iter().any(|b| b.name.starts_with("origin/")));
        // HEAD symref should be filtered out.
        assert!(list.remote.iter().all(|b| !b.name.ends_with("/HEAD")));
    }

    #[test]
    fn list_branches_empty_repo_returns_empty_lists() {
        let tmp = TempDir::new().unwrap();
        // Bare init, no commits. list_branches should succeed but return
        // empty local/remote and empty current.
        run_git(tmp.path(), &["init"]).expect("git init");
        run_git(tmp.path(), &["config", "user.email", "x@x.com"]).unwrap();
        run_git(tmp.path(), &["config", "user.name", "x"]).unwrap();

        let list = GitInspector::list_branches(tmp.path()).expect("list branches");
        // Empty repo: no refs at all, so both local and remote lists must be
        // empty. `current` may be the init-default branch name (git prints
        // "main"/"master" even when no commit has been made) — so we don't
        // assert on it, only that the listing succeeds and returns no refs.
        assert!(list.local.is_empty());
        assert!(list.remote.is_empty());
    }

    #[test]
    fn list_branches_non_git_returns_error() {
        let tmp = TempDir::new().unwrap();
        assert!(GitInspector::list_branches(tmp.path()).is_err());
    }

    #[test]
    fn run_git_reports_missing_path_clearly() {
        // Previously `current_dir` on a nonexistent path produced ENOENT
        // from spawn, which surfaced as "failed to spawn git: No such file
        // or directory" — indistinguishable from git being missing from
        // PATH. The pre-check must now report the real cause.
        let missing = Path::new("/nonexistent-zremote-test-path-a29fbb11");
        let err = run_git(missing, &["--version"]).expect_err("must fail");
        assert!(
            err.contains("path does not exist"),
            "expected path-missing error, got: {err}"
        );
    }

    #[test]
    fn list_branches_caps_at_500_branches() {
        // Create a repo with more than 500 local branches and confirm we
        // never return more than the cap. We verify the cap is honoured by
        // asserting the returned length is exactly BRANCH_LIST_CAP value.
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // Create 520 lightweight branches by writing them directly to
        // `.git/packed-refs`. Spawning 520 `git branch` calls is slow and
        // flakes under parallel test load; a single file write avoids any
        // subprocess work.
        let head = run_git(tmp.path(), &["rev-parse", "HEAD"]).unwrap();
        let head = head.trim();
        let mut packed = String::from("# pack-refs with: peeled fully-peeled sorted \n");
        for i in 0..520 {
            packed.push_str(&format!("{head} refs/heads/b{i:04}\n"));
        }
        fs::write(tmp.path().join(".git").join("packed-refs"), packed).expect("write packed-refs");

        let list = GitInspector::list_branches(tmp.path()).expect("list branches");
        // Default branch (main) + 520 created - but cap is 500.
        assert_eq!(
            list.local.len(),
            500,
            "expected --count=500 cap to be honoured, got {} entries",
            list.local.len()
        );
        assert!(!list.remote_truncated, "no remotes configured in this repo");
    }

    #[test]
    fn inspect_fast_reports_ahead_commits() {
        let tmp = TempDir::new().unwrap();
        let upstream = tmp.path().join("upstream");
        fs::create_dir_all(&upstream).unwrap();
        init_git_repo(&upstream);

        // Clone so the clone has a proper upstream tracking ref.
        let clone = tmp.path().join("clone");
        let status = std::process::Command::new("git")
            .args(["clone", "--quiet"])
            .arg(&upstream)
            .arg(&clone)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CEILING_DIRECTORIES", tmp.path())
            .status()
            .expect("spawn git clone");
        assert!(status.success(), "git clone failed");

        // Set identity on the clone so commits work.
        run_git(&clone, &["config", "user.email", "c@c.com"]).unwrap();
        run_git(&clone, &["config", "user.name", "c"]).unwrap();
        run_git(&clone, &["config", "commit.gpgsign", "false"]).unwrap();

        // Create two local commits so we are ahead of upstream.
        fs::write(clone.join("one.txt"), "1").unwrap();
        run_git(&clone, &["add", "."]).unwrap();
        run_git(&clone, &["commit", "--no-verify", "-m", "one"]).unwrap();
        fs::write(clone.join("two.txt"), "2").unwrap();
        run_git(&clone, &["add", "."]).unwrap();
        run_git(&clone, &["commit", "--no-verify", "-m", "two"]).unwrap();

        let info = GitInspector::inspect_fast(&clone).expect("should detect clone");
        assert_eq!(info.ahead, 2);
        assert_eq!(info.behind, 0);
        assert!(!info.is_dirty);
    }
}
