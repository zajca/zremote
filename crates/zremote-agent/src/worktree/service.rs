//! Shared "default flow" of worktree creation.
//!
//! This is the slice of `create_worktree` that does NOT depend on the local
//! HTTP app state (DB, session manager, broadcast channel):
//!
//! 1. Reject leading-dash inputs (CWE-88).
//! 2. Emit `Init` progress.
//! 3. `git worktree add` on a blocking thread, bounded by a wall-time timeout.
//! 4. Emit `Finalizing` progress.
//! 5. Run the `post_create` hook in captured mode.
//! 6. Emit `Done` progress.
//!
//! Callers translate the `emit_progress` callback into their transport
//! (HTTP broadcasts a `ServerEvent::WorktreeCreationProgress`; WS dispatcher
//! sends an `AgentMessage::WorktreeCreationProgress` on the outbound channel).
//!
//! DB insert + `ProjectsUpdated` broadcast remain in the HTTP handler because
//! the WS dispatcher does not own a DB — the server performs its own upsert
//! after it receives `AgentMessage::WorktreeCreateResponse`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use zremote_protocol::events::WorktreeCreationStage;
use zremote_protocol::project::{WorktreeError, WorktreeErrorCode};
use zremote_protocol::{HookResultInfo, ProjectSettings};

use crate::project::action_runner::ActionRunContext;
use crate::project::git::GitInspector;
use crate::project::hook_dispatcher::{WorktreeSlot, run_worktree_hook};

/// Maximum wall time for the blocking git worktree add call. Chosen to
/// tolerate large-repo worktree creation (where git's staged checkout can
/// legitimately take tens of seconds) while still putting a hard ceiling on
/// the request so the client isn't left hanging forever.
///
/// Kept in sync with `local::routes::projects::worktree::WORKTREE_CREATE_TIMEOUT`
/// and the server-mode dispatch's inline constant.
pub const WORKTREE_CREATE_TIMEOUT: Duration = Duration::from_secs(60);

/// Inputs to `run_worktree_create`. Mirrors the fields callers already have
/// in hand from either the HTTP body or the WS message.
#[derive(Debug, Clone)]
pub struct WorktreeCreateInput {
    pub project_path: PathBuf,
    pub branch: String,
    pub path: Option<PathBuf>,
    pub new_branch: bool,
    pub base_ref: Option<String>,
}

/// Success payload returned by `run_worktree_create`. The `hook_result` is
/// populated when a `post_create` hook ran; `None` means no hook configured
/// (or resolution failed — that path is non-fatal and logged).
#[derive(Debug, Clone)]
pub struct WorktreeCreateOutput {
    pub path: String,
    pub branch: Option<String>,
    pub commit_hash: Option<String>,
    pub hook_result: Option<HookResultInfo>,
}

/// Failure cases surfaced to the caller. `Structured` is a git/validation/
/// hook-resolution failure with a typed `WorktreeError` ready to forward to
/// the client; `Timeout` lets the caller emit a transport-specific message
/// (HTTP → 500; WS → `WorktreeError { code: Internal, .. }`).
#[derive(Debug)]
pub enum WorktreeCreateFailure {
    Structured(WorktreeError),
    Timeout { seconds: u64 },
}

impl WorktreeCreateFailure {
    /// Convert to a `WorktreeError` payload — handy for the WS caller which
    /// needs to put a `WorktreeError` on the wire regardless of variant.
    #[must_use]
    pub fn into_worktree_error(self) -> WorktreeError {
        match self {
            Self::Structured(err) => err,
            Self::Timeout { seconds } => WorktreeError::new(
                WorktreeErrorCode::Internal,
                "Worktree creation timed out — the repository may be very large or git may be stuck.",
                format!("timed out after {seconds}s"),
            ),
        }
    }
}

/// Reject user-controlled git inputs that start with `-`. Without this guard
/// a caller could smuggle additional git options through the worktree
/// endpoint (CWE-88) — for example, passing `--upload-pack=evil` as a branch
/// name. Enforced *inside* the service so no caller can bypass it by calling
/// the helper without first validating, even if the outer HTTP/WS layer
/// forgets to.
fn reject_leading_dash(field: &str, value: &str) -> Result<(), WorktreeError> {
    if value.starts_with('-') {
        return Err(WorktreeError::new(
            WorktreeErrorCode::InvalidRef,
            format!("{field} must not start with '-'"),
            format!("rejected {field}: leading dash not allowed"),
        ));
    }
    Ok(())
}

/// Read full project settings via `spawn_blocking`. `None` means "no settings
/// file or unreadable" which downstream treats as "no hooks configured".
async fn read_settings_off_thread(project_path: &Path) -> Option<ProjectSettings> {
    let pp = project_path.to_path_buf();
    tokio::task::spawn_blocking(move || crate::project::settings::read_settings(&pp))
        .await
        .ok()?
        .ok()
        .flatten()
}

/// Run the default worktree-create flow.
///
/// `emit_progress` is called synchronously on each lifecycle stage. Callers
/// use it to turn the stage into a transport-specific message.
///
/// # Errors
/// See [`WorktreeCreateFailure`] — validation, git, and hook-resolution
/// failures flow through `Structured`; wall-time exceeded flows through
/// `Timeout`.
pub async fn run_worktree_create(
    input: WorktreeCreateInput,
    emit_progress: impl Fn(WorktreeCreationStage, u8, Option<String>) + Send + Sync,
) -> Result<WorktreeCreateOutput, WorktreeCreateFailure> {
    // 1. CWE-88 guard. Runs before any I/O so a malicious caller cannot
    //    observe timing differences between validated and unvalidated paths.
    reject_leading_dash("branch", &input.branch).map_err(WorktreeCreateFailure::Structured)?;
    if let Some(ref p) = input.path {
        let s = p.to_string_lossy();
        reject_leading_dash("path", &s).map_err(WorktreeCreateFailure::Structured)?;
    }
    if let Some(ref b) = input.base_ref {
        reject_leading_dash("base_ref", b).map_err(WorktreeCreateFailure::Structured)?;
    }

    // 2. Init progress before any I/O.
    emit_progress(WorktreeCreationStage::Init, 0, None);

    // Read settings once up front — used for the post_create hook below.
    let settings = read_settings_off_thread(&input.project_path).await;

    // 3. Creating progress: emit right before spawning the git subprocess so
    //    the GUI sees a signal reflecting when git actually starts rather
    //    than when we *scheduled* the blocking task.
    emit_progress(
        WorktreeCreationStage::Creating,
        25,
        Some("running git worktree add".to_string()),
    );

    // git worktree add on a blocking thread with a hard wall-time ceiling.
    let repo_path = input.project_path.clone();
    let branch = input.branch.clone();
    let wt_path: Option<PathBuf> = input.path.clone();
    let new_branch = input.new_branch;
    let base_ref = input.base_ref.clone();

    let mut handle = tokio::task::spawn_blocking(move || {
        GitInspector::create_worktree(
            repo_path.as_path(),
            &branch,
            wt_path.as_deref(),
            new_branch,
            base_ref.as_deref(),
        )
    });

    // Pass `&mut handle` so we keep ownership and can abort on timeout.
    let Ok(join_result) = tokio::time::timeout(WORKTREE_CREATE_TIMEOUT, &mut handle).await else {
        handle.abort();
        let seconds = WORKTREE_CREATE_TIMEOUT.as_secs();
        tracing::warn!(timeout_secs = seconds, "worktree create timed out");
        emit_progress(
            WorktreeCreationStage::Failed,
            100,
            Some(format!("timed out after {seconds}s")),
        );
        return Err(WorktreeCreateFailure::Timeout { seconds });
    };

    let git_result = match join_result {
        Ok(r) => r,
        Err(join_err) => {
            tracing::error!(error = %join_err, "spawn_blocking join failed");
            emit_progress(
                WorktreeCreationStage::Failed,
                100,
                Some("worktree create task failed".to_string()),
            );
            return Err(WorktreeCreateFailure::Structured(WorktreeError::new(
                WorktreeErrorCode::Internal,
                "Internal error while creating worktree.",
                "worktree create task failed",
            )));
        }
    };

    let worktree_info = match git_result {
        Ok(info) => info,
        Err(stderr) => {
            tracing::warn!(error = %stderr, "worktree create failed");
            let err = WorktreeError::from_git_stderr(&stderr);
            emit_progress(
                WorktreeCreationStage::Failed,
                100,
                Some(err.message.clone()),
            );
            return Err(WorktreeCreateFailure::Structured(err));
        }
    };

    // 4. Finalizing: git is done; hook still ahead of us.
    emit_progress(WorktreeCreationStage::Finalizing, 75, None);

    // 5. Post-create hook. Captured mode is the only mode that works without
    //    a PTY — both callers are in contexts where spawning a PTY would
    //    either be out of scope (server dispatch) or already serviced by the
    //    `create`-slot override (local HTTP override branch is handled in the
    //    caller, not here).
    //
    //    Resolution errors (missing action) are non-fatal: the worktree was
    //    created successfully, so withholding the success response because a
    //    misconfigured secondary hook is worse than surfacing the
    //    misconfiguration via a tracing warn. Command failures inside the
    //    hook are captured in `HookResultInfo.success` and forwarded to the
    //    client.
    let hook_result = if let Some(ref sett) = settings {
        let worktree_name = Path::new(&worktree_info.path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from);
        let ctx = ActionRunContext {
            project_path: input.project_path.to_string_lossy().into_owned(),
            worktree_path: Some(worktree_info.path.clone()),
            branch: worktree_info.branch.clone(),
            worktree_name,
            inputs: std::collections::HashMap::new(),
        };
        match run_worktree_hook(sett, WorktreeSlot::PostCreate, ctx, None).await {
            Ok(hook) => hook,
            Err(e) => {
                // Downgrade to warn so the successful worktree still returns.
                tracing::warn!(
                    worktree = %worktree_info.path,
                    error = %e,
                    "post_create hook resolution failed"
                );
                None
            }
        }
    } else {
        None
    };

    // 6. Done.
    emit_progress(WorktreeCreationStage::Done, 100, None);

    Ok(WorktreeCreateOutput {
        path: worktree_info.path,
        branch: worktree_info.branch,
        commit_hash: worktree_info.commit_hash,
        hook_result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal on-disk git repo with one commit and a secondary
    /// branch so callers can test `base_ref` threading.
    fn init_test_repo(dir: &Path) {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env_clear()
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("HOME", dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "--initial-branch=main", "."]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        git(&["add", "."]);
        git(&["commit", "--no-verify", "-m", "init"]);
        git(&["branch", "base-branch"]);
    }

    #[tokio::test]
    async fn run_worktree_create_golden_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);

        let wt_path = tmp.path().join("wt-golden");
        let stages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stages_clone = stages.clone();
        let input = WorktreeCreateInput {
            project_path: repo.clone(),
            branch: "feature-x".to_string(),
            path: Some(wt_path.clone()),
            new_branch: true,
            base_ref: Some("base-branch".to_string()),
        };
        let output = run_worktree_create(input, move |stage, _pct, _msg| {
            stages_clone.lock().unwrap().push(stage);
        })
        .await
        .expect("golden path must succeed");

        assert!(
            output.path.ends_with("wt-golden"),
            "unexpected worktree path: {}",
            output.path
        );
        assert_eq!(output.branch.as_deref(), Some("feature-x"));
        assert!(output.commit_hash.is_some(), "missing commit hash");
        // No hook configured → no hook result.
        assert!(output.hook_result.is_none());

        let seen = stages.lock().unwrap().clone();
        assert!(
            seen.contains(&WorktreeCreationStage::Init),
            "missing Init: {seen:?}"
        );
        assert!(
            seen.contains(&WorktreeCreationStage::Finalizing),
            "missing Finalizing: {seen:?}"
        );
        assert!(
            seen.contains(&WorktreeCreationStage::Done),
            "missing Done: {seen:?}"
        );
    }

    #[tokio::test]
    async fn run_worktree_create_branch_exists_is_structured() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);

        // First create a worktree with branch `dup`.
        let wt1 = tmp.path().join("wt1");
        let _ = run_worktree_create(
            WorktreeCreateInput {
                project_path: repo.clone(),
                branch: "dup".to_string(),
                path: Some(wt1),
                new_branch: true,
                base_ref: None,
            },
            |_, _, _| {},
        )
        .await
        .expect("first create must succeed");

        // Second attempt with the same `new_branch=true` + same name must hit
        // git's "A branch named 'dup' already exists" error and surface as
        // BranchExists.
        let wt2 = tmp.path().join("wt2");
        let err = run_worktree_create(
            WorktreeCreateInput {
                project_path: repo.clone(),
                branch: "dup".to_string(),
                path: Some(wt2),
                new_branch: true,
                base_ref: None,
            },
            |_, _, _| {},
        )
        .await
        .expect_err("second create must fail");

        match err {
            WorktreeCreateFailure::Structured(e) => {
                assert_eq!(
                    e.code,
                    WorktreeErrorCode::BranchExists,
                    "expected BranchExists, got {:?}; message={}",
                    e.code,
                    e.message,
                );
            }
            WorktreeCreateFailure::Timeout { .. } => panic!("unexpected Timeout failure"),
        }
    }

    #[tokio::test]
    async fn run_worktree_create_rejects_leading_dash_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);

        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();
        let err = run_worktree_create(
            WorktreeCreateInput {
                project_path: repo,
                branch: "-x".to_string(),
                path: None,
                new_branch: true,
                base_ref: None,
            },
            move |_, _, _| {
                called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            },
        )
        .await
        .expect_err("leading-dash branch must be rejected");

        // Validation must fire before any progress callback (and before any
        // git call).
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "progress callback fired despite validation rejection"
        );
        match err {
            WorktreeCreateFailure::Structured(e) => {
                assert_eq!(e.code, WorktreeErrorCode::InvalidRef);
                assert!(
                    e.hint.contains("branch"),
                    "hint should mention branch: {}",
                    e.hint
                );
            }
            WorktreeCreateFailure::Timeout { .. } => panic!("unexpected Timeout failure"),
        }
    }

    #[tokio::test]
    async fn run_worktree_create_rejects_leading_dash_base_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_test_repo(&repo);

        let err = run_worktree_create(
            WorktreeCreateInput {
                project_path: repo,
                branch: "ok".to_string(),
                path: None,
                new_branch: true,
                base_ref: Some("--upload-pack=bad".to_string()),
            },
            |_, _, _| {},
        )
        .await
        .expect_err("leading-dash base_ref must be rejected");
        match err {
            WorktreeCreateFailure::Structured(e) => {
                assert_eq!(e.code, WorktreeErrorCode::InvalidRef);
                assert!(
                    e.hint.contains("base_ref"),
                    "hint should mention base_ref: {}",
                    e.hint
                );
            }
            WorktreeCreateFailure::Timeout { .. } => panic!("unexpected Timeout failure"),
        }
    }
}
