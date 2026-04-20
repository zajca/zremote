//! Shell-out git diff runner + source options lister.
//!
//! RFC §6.1. Wraps `git diff` / `git diff --cached` / `git show` with:
//!
//! - per-file 512 KiB cap (trip `too_large=true` with empty hunks)
//! - global 2000-file cap (reject the whole request)
//! - strict validation of user-supplied refs + paths (reject flag injection,
//!   range syntax, control chars, `..`)
//! - 30 s wall-clock timeout on the shell-out
//! - untracked-file synthesis for WorkingTree / WorkingTreeVsHead sources
//! - streaming `DiffEvent` delivery to a sink the caller provides
//!
//! We reuse `super::git::run_git` for the non-streaming helpers (ref list,
//! rev-parse, ls-files) so every git invocation goes through the hardened
//! spawn path (credentials disabled, timeouts, GIT_CEILING_DIRECTORIES).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use zremote_protocol::project::{
    BranchList, DiffError, DiffErrorCode, DiffFile, DiffFileStatus, DiffFileSummary, DiffRequest,
    DiffSource, DiffSourceOptions,
};

use super::diff_parser::parse_unified_diff;
use super::git::{GitInspector, list_recent_commits, run_git, validate_git_ref};

/// Per-file cap for the streamed diff text. A file that exceeds this is sent
/// with `too_large=true` and empty hunks (client renders a placeholder).
pub const MAX_FILE_BYTES: usize = 512 * 1024;
/// Global cap on number of files emitted per request. Protects the agent +
/// the WS frame budget (§11 risks table).
pub const MAX_TOTAL_FILES: usize = 2000;
/// Cap on requested `context_lines`. Above this we clamp.
pub const MAX_CONTEXT_LINES: u32 = 20;
/// Wall-clock timeout for any single `git diff` subprocess.
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap on `file_paths` entries in a DiffRequest.
pub const MAX_FILE_PATHS: usize = 1000;
/// Default number of recent commits returned by `list_diff_sources`.
pub const DEFAULT_RECENT_COMMITS: usize = 20;
/// Upper bound enforced on the `max_commits` query parameter. Prevents a
/// caller from requesting an unbounded `git log -n <N>` and pinning CPU /
/// memory on a huge repo (CWE-400).
pub const MAX_COMMITS_QUERY: usize = 200;

// TODO(P2): wire CancellationToken through `run_diff_streaming` so an
// upstream cancel (client disconnect, request abort) kills the spawned
// `git diff` child. Today we rely on the sink returning BrokenPipe between
// files, which stops further emissions but does not kill the in-flight git
// process. Incremental-streaming parse + kill-on-cancel is owned by P2's
// connection/dispatch refactor (see CWE-404).

/// Events emitted during a streaming diff run. Shape matches the NDJSON
/// payload the REST handler forwards to the client.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiffEvent {
    /// Summary manifest. Emitted first so the GUI populates its file list.
    Started { files: Vec<DiffFileSummary> },
    /// A single file's full DiffFile. `file_index` aligns with `Started.files`.
    File { file_index: u32, file: DiffFile },
    /// Terminal event. `error: Some` after `Started` means partial result.
    Finished {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<DiffError>,
    },
}

/// Shell out to `git diff` / `git show` for the given source, parse the
/// output into `DiffFile`s, and feed them to `sink` one event at a time.
///
/// If `sink` returns `Err` (typically BrokenPipe from a dropped HTTP client),
/// we abort immediately — no further git invocations, no further events.
///
/// Never panics; all errors turn into a `DiffEvent::Finished { error: Some(_) }`
/// delivered through the sink unless the sink itself has failed.
pub fn run_diff_streaming<F>(
    project_path: &Path,
    req: &DiffRequest,
    mut sink: F,
) -> Result<(), DiffError>
where
    F: FnMut(&DiffEvent) -> std::io::Result<()>,
{
    let validated = match validate_request(req) {
        Ok(v) => v,
        Err(err) => {
            // Try to notify the client; ignore secondary sink failures.
            let _ = sink(&DiffEvent::Finished {
                error: Some(err.clone()),
            });
            return Err(err);
        }
    };

    // Build the git arg list. `build_source_args` returns either a
    // `diff`-style invocation (is_show=false) or a `show`-style one
    // (is_show=true); we append `-U<N>` + rename/copy flags uniformly.
    let (is_show, mut args) = match build_source_args(&req.source) {
        Ok(v) => v,
        Err(err) => {
            let _ = sink(&DiffEvent::Finished {
                error: Some(err.clone()),
            });
            return Err(err);
        }
    };
    // Insert context + rename/copy flags right after the subcommand.
    // For `diff` the subcommand is `args[0]`; for `show` likewise.
    let insert_at = 1;
    let uniform_flags = [
        "--no-color".to_string(),
        "--no-ext-diff".to_string(),
        "--no-textconv".to_string(),
        // Force canonical `a/` / `b/` prefixes regardless of the user's
        // `diff.mnemonicPrefix` setting. The parser relies on this to tell
        // which side a ---/+++ line refers to.
        "--src-prefix=a/".to_string(),
        "--dst-prefix=b/".to_string(),
        format!("-U{}", validated.context_lines),
        "-M".to_string(),
        "-C".to_string(),
    ];
    for (i, flag) in uniform_flags.iter().enumerate() {
        args.insert(insert_at + i, flag.clone());
    }

    if !validated.file_paths.is_empty() {
        // For `diff`, pathspec goes after a `--` separator. For `show` the
        // same applies: `git show <sha> -- <paths>`.
        args.push("--".to_string());
        args.extend(validated.file_paths.iter().cloned());
    }

    let _ = is_show; // reserved for future logging / metrics

    // Run `git diff` streaming. Parse the full output once we have it.
    // Note: we buffer the output rather than incrementally parsing the state
    // machine on a byte stream — `git diff` on a 2000-file repo still fits
    // within a few MiB and buffering dramatically simplifies the parser
    // interaction. The per-file cap is enforced after parse.
    let raw = run_git_capped(project_path, &args, MAX_FILE_BYTES * MAX_TOTAL_FILES)?;
    let mut files = parse_unified_diff(&raw);

    // Enforce caps.
    if files.len() > MAX_TOTAL_FILES {
        let err = DiffError {
            code: DiffErrorCode::LimitExceeded,
            message: format!(
                "diff produced {} files (> {} cap)",
                files.len(),
                MAX_TOTAL_FILES
            ),
            hint: Some("narrow the diff with `file_paths` or a smaller range".to_string()),
        };
        let _ = sink(&DiffEvent::Finished {
            error: Some(err.clone()),
        });
        return Err(err);
    }

    // Trip too_large for any file whose raw hunk bytes exceed the per-file
    // cap. We compute by reserialising hunk content — cheap and avoids holding
    // the original byte spans through the parser.
    for f in &mut files {
        let approx_bytes: usize = f
            .hunks
            .iter()
            .map(|h| h.lines.iter().map(|l| l.content.len()).sum::<usize>())
            .sum();
        if approx_bytes > MAX_FILE_BYTES {
            f.summary.too_large = true;
            f.hunks.clear();
        }
    }

    // For WorkingTree / WorkingTreeVsHead, append untracked files as synthetic
    // Added entries (Okena pattern).
    if matches!(
        req.source,
        DiffSource::WorkingTree | DiffSource::WorkingTreeVsHead
    ) {
        append_untracked(project_path, &mut files, &validated.file_paths)?;
    }

    // Emit `Started` with summaries.
    let summaries: Vec<DiffFileSummary> = files.iter().map(|f| f.summary.clone()).collect();
    if let Err(e) = sink(&DiffEvent::Started {
        files: summaries.clone(),
    }) {
        return Err(DiffError {
            code: DiffErrorCode::Other,
            message: format!("sink failed before first chunk: {e}"),
            hint: None,
        });
    }

    // Emit one File per summary. Abort on sink failure between files.
    for (idx, file) in files.into_iter().enumerate() {
        let evt = DiffEvent::File {
            file_index: idx as u32,
            file,
        };
        if let Err(e) = sink(&evt) {
            tracing::debug!(error = %e, "diff sink closed — aborting worker");
            return Ok(()); // Client went away; not our error to raise.
        }
    }

    let _ = sink(&DiffEvent::Finished { error: None });
    Ok(())
}

struct ValidatedRequest {
    context_lines: u32,
    file_paths: Vec<String>,
}

fn validate_request(req: &DiffRequest) -> Result<ValidatedRequest, DiffError> {
    let context_lines = req.context_lines.min(MAX_CONTEXT_LINES);

    let file_paths: Vec<String> = match &req.file_paths {
        None => Vec::new(),
        Some(paths) => {
            if paths.len() > MAX_FILE_PATHS {
                return Err(DiffError {
                    code: DiffErrorCode::LimitExceeded,
                    message: format!(
                        "file_paths contains {} entries (> {} cap)",
                        paths.len(),
                        MAX_FILE_PATHS
                    ),
                    hint: None,
                });
            }
            for p in paths {
                zremote_core::validation::validate_path_no_traversal(p).map_err(|e| DiffError {
                    code: DiffErrorCode::InvalidInput,
                    message: format!("invalid file path: {e}"),
                    hint: None,
                })?;
                if p.starts_with('-') {
                    return Err(DiffError {
                        code: DiffErrorCode::InvalidInput,
                        message: format!("file path must not start with '-': {p}"),
                        hint: None,
                    });
                }
                if p.contains('\0') || p.contains('\n') {
                    return Err(DiffError {
                        code: DiffErrorCode::InvalidInput,
                        message: "file path contains control characters".to_string(),
                        hint: None,
                    });
                }
            }
            paths.clone()
        }
    };

    Ok(ValidatedRequest {
        context_lines,
        file_paths,
    })
}

/// Build the subcommand + source-specific portion of the git arg list.
/// Returns `(is_show, args)` where `args[0]` is the subcommand ("diff" or
/// "show") and the remainder is source-specific; the caller splices in the
/// common flags (`-U`, `--no-color`, rename/copy) and trailing `-- <paths>`.
fn build_source_args(source: &DiffSource) -> Result<(bool, Vec<String>), DiffError> {
    match source {
        DiffSource::WorkingTree => Ok((false, vec!["diff".to_string()])),
        DiffSource::Staged => Ok((false, vec!["diff".to_string(), "--cached".to_string()])),
        DiffSource::WorkingTreeVsHead => Ok((false, vec!["diff".to_string(), "HEAD".to_string()])),
        DiffSource::HeadVs { reference } => {
            validate_git_ref(reference)?;
            // `git diff <reference>..HEAD` — shows what HEAD brings in vs base.
            Ok((
                false,
                vec!["diff".to_string(), format!("{reference}..HEAD")],
            ))
        }
        DiffSource::Range {
            from,
            to,
            symmetric,
        } => {
            validate_git_ref(from)?;
            validate_git_ref(to)?;
            let sep = if *symmetric { "..." } else { ".." };
            Ok((false, vec!["diff".to_string(), format!("{from}{sep}{to}")]))
        }
        DiffSource::Commit { sha } => {
            validate_git_ref(sha)?;
            // `git show --format= <sha>` — always works, even on root
            // commits (which `git diff <sha>^ <sha>` cannot handle).
            Ok((
                true,
                vec!["show".to_string(), "--format=".to_string(), sha.clone()],
            ))
        }
    }
}

/// Like `run_git` but with a larger output ceiling and DIFF_TIMEOUT. Returns
/// the stdout text (may be empty, e.g. no changes). Failures turn into a
/// structured `DiffError` — not a `String`.
fn run_git_capped(path: &Path, args: &[String], max_bytes: usize) -> Result<String, DiffError> {
    if !path.exists() {
        return Err(DiffError {
            code: DiffErrorCode::PathMissing,
            message: format!("path does not exist: {}", path.display()),
            hint: None,
        });
    }
    let mut child = Command::new("git")
        .args(args)
        .current_dir(path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CEILING_DIRECTORIES", path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DiffError {
            code: DiffErrorCode::Other,
            message: format!("failed to spawn git: {e}"),
            hint: None,
        })?;

    // Drain stdout up to `max_bytes`, concurrently polling the process for
    // timeout. Reading on a separate thread keeps the pipe from blocking the
    // child when stdout fills the OS buffer.
    let stdout = child.stdout.take().ok_or_else(|| DiffError {
        code: DiffErrorCode::Other,
        message: "failed to take git stdout".to_string(),
        hint: None,
    })?;
    let stderr = child.stderr.take();

    let reader_handle = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let mut buf = Vec::with_capacity(64 * 1024);
        let mut reader = BufReader::new(stdout);
        let mut tmp = [0u8; 8192];
        loop {
            let n = reader.read(&mut tmp)?;
            if n == 0 {
                break;
            }
            if buf.len() + n > max_bytes {
                // Truncate silently — better a partial diff than a crash.
                let remaining = max_bytes.saturating_sub(buf.len());
                buf.extend_from_slice(&tmp[..remaining]);
                // Drain the rest into the void so the child doesn't block.
                let mut sink = [0u8; 8192];
                loop {
                    match reader.read(&mut sink) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        Ok(buf)
    });

    let deadline = Instant::now() + DIFF_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(DiffError {
                        code: DiffErrorCode::Timeout,
                        message: format!(
                            "git {args:?} timed out after {}s",
                            DIFF_TIMEOUT.as_secs()
                        ),
                        hint: None,
                    });
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                return Err(DiffError {
                    code: DiffErrorCode::Other,
                    message: format!("git wait failed: {e}"),
                    hint: None,
                });
            }
        }
    };

    let stdout_bytes = match reader_handle.join() {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            return Err(DiffError {
                code: DiffErrorCode::Other,
                message: format!("git stdout read failed: {e}"),
                hint: None,
            });
        }
        Err(_) => {
            return Err(DiffError {
                code: DiffErrorCode::Other,
                message: "git stdout reader thread panicked".to_string(),
                hint: None,
            });
        }
    };

    if !status.success() {
        let mut stderr_text = String::new();
        if let Some(mut s) = stderr {
            use std::io::Read;
            let _ = s.read_to_string(&mut stderr_text);
        }
        let stderr_trim = stderr_text.trim();
        let lowered = stderr_trim.to_lowercase();
        let code = if lowered.contains("unknown revision") || lowered.contains("bad revision") {
            DiffErrorCode::RefNotFound
        } else if lowered.contains("not a git repository") {
            DiffErrorCode::NotGitRepo
        } else {
            DiffErrorCode::Other
        };
        // Log the full stderr agent-side (CWE-532). Client-visible message is
        // a fixed category string so we never leak absolute paths, remote
        // URLs, credentials, or git internals to the caller (CWE-209).
        tracing::warn!(
            exit_status = %status,
            stderr = %stderr_trim,
            "git subprocess failed"
        );
        let safe_message = match code {
            DiffErrorCode::RefNotFound => "git ref not found",
            DiffErrorCode::NotGitRepo => "not a git repository",
            _ => "git command failed",
        }
        .to_string();
        return Err(DiffError {
            code,
            message: safe_message,
            hint: None,
        });
    }

    // Git diff output is UTF-8-ish; invalid sequences become replacement
    // chars. Consistent with the rest of the crate.
    Ok(String::from_utf8_lossy(&stdout_bytes).into_owned())
}

/// Append synthetic entries for untracked files. Each untracked file is
/// treated as "all lines Added" so the GUI shows it alongside real diffs.
fn append_untracked(
    path: &Path,
    files: &mut Vec<DiffFile>,
    allow_paths: &[String],
) -> Result<(), DiffError> {
    let raw = match run_git(path, &["ls-files", "--others", "--exclude-standard", "-z"]) {
        Ok(s) => s,
        Err(stderr) => {
            // Can't list untracked — not fatal. Diff itself may still be
            // useful. Log and continue.
            tracing::warn!(error = %stderr, "ls-files untracked failed — skipping");
            return Ok(());
        }
    };
    // -z uses NUL separators, but `run_git` trims and returns a String. NUL
    // may still survive inside the string — split on '\0'. If it got stripped
    // by the trim path, fall back to newlines.
    let mut rel_paths: Vec<&str> = if raw.contains('\0') {
        raw.split('\0').filter(|s| !s.is_empty()).collect()
    } else {
        raw.lines().filter(|s| !s.is_empty()).collect()
    };

    if !allow_paths.is_empty() {
        rel_paths.retain(|p| allow_paths.iter().any(|allowed| *p == allowed));
    }

    for rel in rel_paths {
        if files.len() >= MAX_TOTAL_FILES {
            break;
        }
        let abs = path.join(rel);
        let Ok(meta) = std::fs::metadata(&abs) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let is_too_large = meta.len() as usize > MAX_FILE_BYTES;
        let (binary, lines_count, hunks) = if is_too_large {
            (false, 0u32, Vec::new())
        } else {
            let Ok(bytes) = std::fs::read(&abs) else {
                continue;
            };
            let binary = looks_binary(&bytes);
            if binary {
                (true, 0u32, Vec::new())
            } else {
                let text = String::from_utf8_lossy(&bytes);
                let mut lines: Vec<zremote_protocol::project::DiffLine> = Vec::new();
                let mut n: u32 = 0;
                for (i, ln) in text.split('\n').enumerate() {
                    // Skip the trailing empty element produced by a final '\n'.
                    if i > 0
                        && ln.is_empty()
                        && text.ends_with('\n')
                        && i + 1 == text.split('\n').count()
                    {
                        continue;
                    }
                    n += 1;
                    lines.push(zremote_protocol::project::DiffLine {
                        kind: zremote_protocol::project::DiffLineKind::Added,
                        old_lineno: None,
                        new_lineno: Some(n),
                        content: ln.to_string(),
                    });
                }
                let hunk = zremote_protocol::project::DiffHunk {
                    old_start: 0,
                    old_lines: 0,
                    new_start: 1,
                    new_lines: n,
                    header: format!("@@ -0,0 +1,{n} @@"),
                    lines,
                };
                (false, n, vec![hunk])
            }
        };

        files.push(DiffFile {
            summary: DiffFileSummary {
                path: rel.to_string(),
                old_path: None,
                status: DiffFileStatus::Added,
                binary,
                submodule: false,
                too_large: is_too_large,
                additions: lines_count,
                deletions: 0,
                old_sha: None,
                new_sha: None,
                old_mode: None,
                new_mode: None,
            },
            hunks,
        });
    }
    Ok(())
}

fn looks_binary(bytes: &[u8]) -> bool {
    // Mimics git's heuristic: NUL in the first 8000 bytes → binary.
    bytes.iter().take(8000).any(|b| *b == 0)
}

/// Gather everything the diff source picker needs in a single call. Runs
/// several cheap git invocations: status checks, branch list, recent commits,
/// HEAD SHA.
pub fn list_diff_sources(
    project_path: &Path,
    max_commits: usize,
) -> Result<DiffSourceOptions, DiffError> {
    if !project_path.exists() {
        return Err(DiffError {
            code: DiffErrorCode::PathMissing,
            message: format!("path does not exist: {}", project_path.display()),
            hint: None,
        });
    }
    // Confirm it's a git repo. Falls back to empty options if not.
    if run_git(project_path, &["rev-parse", "--is-inside-work-tree"]).is_err() {
        return Err(DiffError {
            code: DiffErrorCode::NotGitRepo,
            message: "not a git repository".to_string(),
            hint: None,
        });
    }

    let has_working_tree_changes = run_git(project_path, &["diff", "--quiet"]).err().is_some();
    let has_staged_changes = run_git(project_path, &["diff", "--cached", "--quiet"])
        .err()
        .is_some();

    let branches = match GitInspector::list_branches(project_path) {
        Ok(b) => b,
        Err(_) => BranchList {
            local: Vec::new(),
            remote: Vec::new(),
            current: String::new(),
            remote_truncated: false,
        },
    };

    let n = if max_commits == 0 {
        DEFAULT_RECENT_COMMITS
    } else {
        max_commits
    };
    let recent_commits = list_recent_commits(project_path, n)?;

    let head_sha = run_git(project_path, &["rev-parse", "HEAD"]).ok();
    let head_short_sha = head_sha
        .as_ref()
        .map(|s| s.chars().take(7).collect::<String>());

    Ok(DiffSourceOptions {
        has_working_tree_changes,
        has_staged_changes,
        branches,
        recent_commits,
        head_sha,
        head_short_sha,
    })
}

/// Helper used by tests and callers that want the absolute canonical path
/// to a project's working tree. Not public outside the crate.
#[allow(dead_code)]
pub(crate) fn canonical_path(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Directly invoke the system git without the 5-second `run_git` timeout.
    /// Under parallel test load the short timeout trips purely from scheduler
    /// contention; diff tests need a timeout-free helper for setup work.
    fn raw_git_out(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("failed to spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed:\nstderr: {}\nstdout: {}",
            args,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn raw_git(dir: &Path, args: &[&str]) {
        let _ = raw_git_out(dir, args);
    }

    /// Initialise a git repo at `path` with an initial commit. Uses
    /// `raw_git` to bypass the 5s `run_git` timeout (test scheduler
    /// contention under parallel load trips it spuriously).
    fn init_git_repo(path: &Path) {
        raw_git(path, &["init", "--initial-branch=main"]);
        raw_git(path, &["config", "user.email", "t@t.com"]);
        raw_git(path, &["config", "user.name", "Test"]);
        raw_git(path, &["config", "commit.gpgsign", "false"]);
        fs::write(path.join("README.md"), "# Test\n").unwrap();
        raw_git(path, &["add", "."]);
        raw_git(path, &["commit", "--no-verify", "-m", "initial"]);
    }

    fn collect<F>(
        project_path: &Path,
        source: DiffSource,
        file_paths: Option<Vec<String>>,
    ) -> Vec<DiffEvent>
    where
        F: Sized,
    {
        let _ = std::marker::PhantomData::<F>;
        let mut events = Vec::new();
        let req = DiffRequest {
            project_id: "test".to_string(),
            source,
            file_paths,
            context_lines: 3,
        };
        let _ = run_diff_streaming(project_path, &req, |e| {
            events.push(e.clone());
            Ok(())
        });
        events
    }

    fn run(project_path: &Path, source: DiffSource) -> Vec<DiffEvent> {
        collect::<()>(project_path, source, None)
    }

    #[test]
    fn working_tree_shows_unstaged_modification() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("README.md"), "# Test\n# Modified\n").unwrap();

        let events = run(tmp.path(), DiffSource::WorkingTree);
        // Expect Started + 1 File + Finished.
        assert!(matches!(events.first(), Some(DiffEvent::Started { files }) if files.len() == 1));
        match &events[1] {
            DiffEvent::File { file, .. } => {
                assert_eq!(file.summary.path, "README.md");
                assert_eq!(file.summary.status, DiffFileStatus::Modified);
                assert!(file.summary.additions >= 1);
            }
            other => panic!("expected File event, got {other:?}"),
        }
        assert!(matches!(
            events.last(),
            Some(DiffEvent::Finished { error: None })
        ));
    }

    #[test]
    fn working_tree_shows_untracked_as_added() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("new.txt"), "hello\nworld\n").unwrap();

        let events = run(tmp.path(), DiffSource::WorkingTree);
        let started = match &events[0] {
            DiffEvent::Started { files } => files.clone(),
            _ => panic!("expected Started"),
        };
        let new_summary = started.iter().find(|s| s.path == "new.txt");
        assert!(
            new_summary.is_some(),
            "untracked file must appear in Started"
        );
        assert_eq!(new_summary.unwrap().status, DiffFileStatus::Added);
    }

    #[test]
    fn staged_shows_added_file() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("new.txt"), "hi\n").unwrap();
        raw_git(tmp.path(), &["add", "new.txt"]);

        let events = run(tmp.path(), DiffSource::Staged);
        let files = match &events[0] {
            DiffEvent::Started { files } => files.clone(),
            _ => panic!("expected Started"),
        };
        let f = files.iter().find(|f| f.path == "new.txt").expect("new.txt");
        assert_eq!(f.status, DiffFileStatus::Added);
    }

    #[test]
    fn working_tree_vs_head_shows_combined() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        // Staged + unstaged modification.
        fs::write(tmp.path().join("staged.txt"), "s\n").unwrap();
        raw_git(tmp.path(), &["add", "staged.txt"]);
        fs::write(tmp.path().join("README.md"), "# Test\n# X\n").unwrap();

        let events = run(tmp.path(), DiffSource::WorkingTreeVsHead);
        let files = match &events[0] {
            DiffEvent::Started { files } => files.clone(),
            _ => panic!("expected Started"),
        };
        let paths: Vec<&String> = files.iter().map(|s| &s.path).collect();
        assert!(paths.iter().any(|p| *p == "staged.txt"));
        assert!(paths.iter().any(|p| *p == "README.md"));
    }

    #[test]
    fn head_vs_ref_diff() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        // Create a feature branch, add a commit on it, then diff feature vs main.
        raw_git(tmp.path(), &["checkout", "-b", "feat"]);
        fs::write(tmp.path().join("f.txt"), "feat\n").unwrap();
        raw_git(tmp.path(), &["add", "."]);
        raw_git(tmp.path(), &["commit", "--no-verify", "-m", "feat commit"]);

        // HEAD (feat) vs base=main → expect f.txt added.
        let events = run(
            tmp.path(),
            DiffSource::HeadVs {
                reference: "main".to_string(),
            },
        );
        let files = match &events[0] {
            DiffEvent::Started { files } => files.clone(),
            _ => panic!("expected Started"),
        };
        assert!(
            files.iter().any(|f| f.path == "f.txt"),
            "expected f.txt in HEAD vs main diff, got {files:?}"
        );
    }

    #[test]
    fn range_diff_reports_all_changes() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        // Capture the initial commit SHA as the base of the range. We deliberately
        // resolve to a concrete SHA rather than relying on `HEAD~1` — the strict
        // `validate_git_ref` allowlist (CWE-88) rejects `~` and `^`, so callers
        // must pass pre-resolved revisions here.
        let initial = raw_git_out(tmp.path(), &["rev-parse", "HEAD"]);
        // Build a second commit.
        fs::write(tmp.path().join("a.txt"), "a\n").unwrap();
        raw_git(tmp.path(), &["add", "."]);
        raw_git(tmp.path(), &["commit", "--no-verify", "-m", "second"]);

        let events = run(
            tmp.path(),
            DiffSource::Range {
                from: initial,
                to: "HEAD".to_string(),
                symmetric: false,
            },
        );
        let files = match &events[0] {
            DiffEvent::Started { files } => files.clone(),
            _ => panic!("expected Started"),
        };
        assert!(files.iter().any(|f| f.path == "a.txt"));
    }

    #[test]
    fn commit_diff_against_first_parent() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("c.txt"), "c\n").unwrap();
        raw_git(tmp.path(), &["add", "."]);
        raw_git(tmp.path(), &["commit", "--no-verify", "-m", "c commit"]);

        let head = raw_git_out(tmp.path(), &["rev-parse", "HEAD"]);
        let events = run(tmp.path(), DiffSource::Commit { sha: head });
        let files = match &events[0] {
            DiffEvent::Started { files } => files.clone(),
            _ => panic!("expected Started"),
        };
        assert!(files.iter().any(|f| f.path == "c.txt"));
    }

    #[test]
    fn commit_diff_works_for_root_commit() {
        // `git show` handles root commits; `git diff <sha>^ <sha>` does not.
        // Using the initial commit SHA must still succeed.
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let head = raw_git_out(tmp.path(), &["rev-parse", "HEAD"]);

        let events = run(tmp.path(), DiffSource::Commit { sha: head });
        let files = match &events[0] {
            DiffEvent::Started { files } => files.clone(),
            _ => panic!("expected Started"),
        };
        assert!(files.iter().any(|f| f.path == "README.md"));
    }

    #[test]
    fn deleted_file_appears_as_deleted() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        fs::remove_file(tmp.path().join("README.md")).unwrap();

        let events = run(tmp.path(), DiffSource::WorkingTree);
        let files = match &events[0] {
            DiffEvent::Started { files } => files.clone(),
            _ => panic!("expected Started"),
        };
        let f = files
            .iter()
            .find(|f| f.path == "README.md")
            .expect("README.md");
        assert_eq!(f.status, DiffFileStatus::Deleted);
    }

    #[test]
    fn renamed_file_tracks_old_path() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        // Write then commit a second file so a rename is stable.
        fs::write(
            tmp.path().join("before.txt"),
            "same content\nline 2\nline 3\n",
        )
        .unwrap();
        raw_git(tmp.path(), &["add", "."]);
        raw_git(tmp.path(), &["commit", "--no-verify", "-m", "add"]);

        // Rename it.
        std::fs::rename(tmp.path().join("before.txt"), tmp.path().join("after.txt")).unwrap();
        raw_git(tmp.path(), &["add", "-A"]);

        let events = run(tmp.path(), DiffSource::Staged);
        let files = match &events[0] {
            DiffEvent::Started { files } => files.clone(),
            _ => panic!("expected Started"),
        };
        let f = files
            .iter()
            .find(|f| f.path == "after.txt")
            .expect("after.txt must be in diff");
        assert_eq!(f.status, DiffFileStatus::Renamed);
        assert_eq!(f.old_path.as_deref(), Some("before.txt"));
    }

    #[test]
    fn empty_repo_diff_returns_empty_started() {
        let tmp = TempDir::new().unwrap();
        raw_git(tmp.path(), &["init"]);
        raw_git(tmp.path(), &["config", "user.email", "x@x.com"]);
        raw_git(tmp.path(), &["config", "user.name", "x"]);

        let events = run(tmp.path(), DiffSource::WorkingTree);
        match &events[0] {
            DiffEvent::Started { files } => assert!(files.is_empty()),
            other => panic!("expected empty Started, got {other:?}"),
        }
    }

    #[test]
    fn invalid_ref_is_rejected_with_invalid_input() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let events = run(
            tmp.path(),
            DiffSource::HeadVs {
                reference: "-ignorecase".to_string(),
            },
        );
        match events.last() {
            Some(DiffEvent::Finished { error: Some(e) }) => {
                assert_eq!(e.code, DiffErrorCode::InvalidInput);
            }
            other => panic!("expected InvalidInput Finished, got {other:?}"),
        }
    }

    #[test]
    fn sink_broken_pipe_aborts_worker_gracefully() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        // Make at least one file dirty so we have something to stream.
        fs::write(tmp.path().join("README.md"), "x\n").unwrap();
        fs::write(tmp.path().join("a.txt"), "a\n").unwrap();
        fs::write(tmp.path().join("b.txt"), "b\n").unwrap();

        let req = DiffRequest {
            project_id: "t".to_string(),
            source: DiffSource::WorkingTree,
            file_paths: None,
            context_lines: 3,
        };

        let mut seen_started = false;
        let mut file_count = 0;
        let result = run_diff_streaming(tmp.path(), &req, |e| {
            match e {
                DiffEvent::Started { .. } => {
                    seen_started = true;
                }
                DiffEvent::File { .. } => {
                    file_count += 1;
                    if file_count == 1 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "client gone",
                        ));
                    }
                }
                DiffEvent::Finished { .. } => {}
            }
            Ok(())
        });
        // Ok — graceful abort; not an error the caller needs to handle.
        assert!(result.is_ok(), "BrokenPipe must not bubble as DiffError");
        assert!(seen_started);
        assert_eq!(file_count, 1, "worker must stop after first sink failure");
    }

    #[test]
    fn file_paths_cap_is_enforced() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let paths: Vec<String> = (0..=MAX_FILE_PATHS).map(|i| format!("f{i}.txt")).collect();
        let req = DiffRequest {
            project_id: "t".to_string(),
            source: DiffSource::WorkingTree,
            file_paths: Some(paths),
            context_lines: 3,
        };
        let mut events = Vec::new();
        let _ = run_diff_streaming(tmp.path(), &req, |e| {
            events.push(e.clone());
            Ok(())
        });
        match events.last() {
            Some(DiffEvent::Finished { error: Some(err) }) => {
                assert_eq!(err.code, DiffErrorCode::LimitExceeded);
            }
            other => panic!("expected LimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn file_path_with_traversal_is_rejected() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let req = DiffRequest {
            project_id: "t".to_string(),
            source: DiffSource::WorkingTree,
            file_paths: Some(vec!["../etc/passwd".to_string()]),
            context_lines: 3,
        };
        let mut events = Vec::new();
        let _ = run_diff_streaming(tmp.path(), &req, |e| {
            events.push(e.clone());
            Ok(())
        });
        match events.last() {
            Some(DiffEvent::Finished { error: Some(err) }) => {
                assert_eq!(err.code, DiffErrorCode::InvalidInput);
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn list_diff_sources_returns_current_branch_and_head_sha() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let opts = list_diff_sources(tmp.path(), 10).expect("list sources");
        assert!(!opts.branches.current.is_empty());
        assert!(opts.head_sha.is_some());
        assert_eq!(opts.head_short_sha.as_ref().map(|s| s.len()), Some(7));
        assert!(!opts.has_working_tree_changes);
        assert!(!opts.has_staged_changes);
    }

    #[test]
    fn list_diff_sources_flags_dirty_tree() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        fs::write(tmp.path().join("README.md"), "dirty\n").unwrap();

        let opts = list_diff_sources(tmp.path(), 5).expect("list sources");
        assert!(opts.has_working_tree_changes);
    }

    #[test]
    fn list_diff_sources_rejects_non_git() {
        let tmp = TempDir::new().unwrap();
        let err = list_diff_sources(tmp.path(), 5).expect_err("must fail");
        assert_eq!(err.code, DiffErrorCode::NotGitRepo);
    }

    /// CWE-532 / CWE-209: on a nonzero git exit the client-visible message
    /// must be a fixed category string — never the raw stderr which can
    /// leak absolute paths, remote URLs, or git internals. Full stderr is
    /// still logged agent-side via `tracing::warn!`.
    #[test]
    fn git_error_messages_never_leak_stderr_paths() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // A syntactically valid ref that doesn't exist triggers the "unknown
        // revision" error path in `run_git_capped`. We call
        // `run_diff_streaming` directly to capture its return value — a
        // run_git_capped failure short-circuits with `?` before emitting
        // any sink events.
        let req = DiffRequest {
            project_id: "t".to_string(),
            source: DiffSource::Commit {
                sha: "nonexistent-ref-xyz".to_string(),
            },
            file_paths: None,
            context_lines: 3,
        };
        let err = run_diff_streaming(tmp.path(), &req, |_| Ok(()))
            .expect_err("nonexistent ref must produce a DiffError");
        assert_eq!(err.code, DiffErrorCode::RefNotFound);
        // Must be the safe category string — not the raw git stderr.
        assert_eq!(err.message, "git ref not found");
        // Extra defence: must not leak the absolute test path.
        let tmp_path_str = tmp.path().to_string_lossy().to_string();
        assert!(
            !err.message.contains(&tmp_path_str),
            "error message must not contain agent-side paths: {}",
            err.message
        );
        // Sanitized messages are drawn from a fixed allowlist — no paths.
        let allowed = [
            "git ref not found",
            "not a git repository",
            "git command failed",
        ];
        assert!(
            allowed.contains(&err.message.as_str()),
            "leaked message outside allowlist: {}",
            err.message
        );
    }

    /// CWE-209: whichever git error category we hit, the client-visible
    /// message must come from the fixed allowlist — never the raw stderr.
    #[test]
    fn git_unknown_error_maps_to_generic_message() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        // `deadbeef` passes the allowlist regex; git will reject it with
        // "unknown revision". Category is RefNotFound, message is "git ref
        // not found" — no stderr leakage.
        let req = DiffRequest {
            project_id: "t".to_string(),
            source: DiffSource::Commit {
                sha: "deadbeef".to_string(),
            },
            file_paths: None,
            context_lines: 3,
        };
        let err = run_diff_streaming(tmp.path(), &req, |_| Ok(()))
            .expect_err("deadbeef must produce a DiffError");
        let allowed = [
            "git ref not found",
            "not a git repository",
            "git command failed",
        ];
        assert!(
            allowed.contains(&err.message.as_str()),
            "unexpected leaked message: {}",
            err.message
        );
    }
}
