use std::path::Path;
use std::process::Command;

use zremote_protocol::project::{GitInfo, GitRemote, WorktreeInfo};

/// Run a git command in the given directory with a 5-second timeout.
/// Returns stdout as a trimmed String on success, or an error message.
fn run_git(path: &Path, args: &[&str]) -> Result<String, String> {
    let child = Command::new("git")
        .args(args)
        .current_dir(path)
        // Prevent parent repo or env vars from interfering
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        // Prevent git from discovering repos above the target path
        .env("GIT_CEILING_DIRECTORIES", path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn git: {e}"))?;

    let result = child.wait_with_output();
    match result {
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

    /// Create a new worktree.
    pub fn create_worktree(
        repo_path: &Path,
        branch: &str,
        worktree_path: Option<&Path>,
        new_branch: bool,
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

        let mut args = vec!["worktree", "add"];
        if new_branch {
            args.push("-b");
            args.push(branch);
            args.push(wt_path.to_str().ok_or("invalid worktree path")?);
        } else {
            args.push(wt_path.to_str().ok_or("invalid worktree path")?);
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
        let wt = GitInspector::create_worktree(&repo_path, "test-branch", Some(&wt_path), true)
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
        let wt =
            GitInspector::create_worktree(&repo_path, "existing-branch", Some(&wt_path), false)
                .expect("create worktree from existing branch");

        assert_eq!(wt.branch.as_deref(), Some("existing-branch"));
    }

    #[test]
    fn create_worktree_auto_path() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("myrepo");
        fs::create_dir_all(&repo_path).unwrap();
        init_git_repo(&repo_path);

        let wt = GitInspector::create_worktree(&repo_path, "auto-branch", None, true)
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
}
