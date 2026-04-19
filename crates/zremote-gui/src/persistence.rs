//! Batched async persistence for GUI state (see `docs/rfc/rfc-004-batched-async-persistence.md`).
//!
//! State file: `~/.config/zremote/gui-state.json`
//!
//! # Model
//!
//! The public [`Persistence`] handle is held by `AppState` and mutated from the
//! GPUI main thread. Mutations are synchronous and non-blocking: `update` only
//! touches in-memory fields, clones the current `GuiState`, drops the snapshot
//! into a shared "pending" slot, and signals a background worker thread. The
//! worker is the only entity that touches disk. It waits for work, debounces
//! bursts, then performs a single atomic-with-backup write per burst.
//!
//! # Invariants
//!
//! - Exactly one background thread performs I/O. `update` is wait-free with
//!   respect to file I/O.
//! - `last_saved_version <= pending_version` at all times; `last_saved_version`
//!   is monotonic.
//! - Only one mutex (`SharedSaver::mu`) protects the saver state. The worker
//!   never holds the mutex across a `PersistenceWriter::write` call — all disk
//!   I/O happens outside the critical section.
//! - Coalescing: N rapid `update` calls during a debounce window produce at
//!   most 2 writes (one for whatever is in `pending` when the current write
//!   started, plus at most one follow-up for everything after).
//! - Retry on failure: if `PersistenceWriter::write` returns `Err`, the worker
//!   re-queues the failed snapshot into `pending` (unless a newer update has
//!   already superseded it) and retries after the next debounce window.
//!   Callers waiting in `flush_blocking` either see a later retry succeed or
//!   observe the deadline elapse.
//! - `Drop::drop` flushes pending writes with a 2 s deadline, then signals
//!   shutdown and joins the worker before returning.
//!
//! # Default-state guard
//!
//! Matches the legacy behaviour: if the snapshot about to be written compares
//! equal to [`GuiState::default`] (via [`GuiState::is_default`]), the worker
//! skips the disk write **but still advances `last_saved_version`** so that
//! [`Persistence::flush_blocking`] does not spin waiting for a write that will
//! never happen.
//!
//! # Testability
//!
//! The writer is injectable via [`PersistenceWriter`]. Tests install counting,
//! blocking, or failing writers through [`Persistence::with_writer`] without
//! touching the real filesystem.

use std::collections::HashSet;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentSession {
    pub session_id: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentAction {
    pub action_key: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
}

/// Current format version. Informational only — written on save but not checked
/// on load. New fields use `#[serde(default)]` for backward compatibility.
const FORMAT_VERSION: u32 = 2;

/// Default debounce window for the production worker.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(250);

/// Maximum entries retained in [`GuiState::recent_add_paths`].
pub const RECENT_ADD_PATHS_CAP: usize = 20;

/// Default deadline used by `Drop` to flush pending writes before shutdown.
const DROP_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GuiState {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub server_url: Option<String>,
    #[serde(default)]
    pub active_session_id: Option<String>,
    #[serde(default)]
    pub window_width: Option<f32>,
    #[serde(default)]
    pub window_height: Option<f32>,
    #[serde(default)]
    pub recent_sessions: Vec<RecentSession>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_actions: Vec<RecentAction>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub activity_panel_visible: bool,
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub expanded_projects: HashSet<String>,
    /// Projects the user has explicitly force-collapsed. Wins over
    /// `expanded_projects` and the default heuristic so a user who wants
    /// a <4-worktree parent hidden can make it stay hidden across restarts.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub collapsed_projects: HashSet<String>,
    /// Most-recently-used paths from the "Add project" path autocomplete.
    /// Front is newest; deduped on push; capped at [`RECENT_ADD_PATHS_CAP`].
    /// Stored as-given by the caller (no canonicalization).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_add_paths: Vec<String>,
}

impl GuiState {
    fn is_default(&self) -> bool {
        self.server_url.is_none()
            && self.active_session_id.is_none()
            && self.window_width.is_none()
            && self.window_height.is_none()
            && self.recent_sessions.is_empty()
            && self.recent_actions.is_empty()
            && !self.activity_panel_visible
            && self.expanded_projects.is_empty()
            && self.collapsed_projects.is_empty()
            && self.recent_add_paths.is_empty()
    }

    /// Record a path entered in the "Add project" autocomplete: dedupe against
    /// any existing equal entry, push to the front, and cap the list at
    /// [`RECENT_ADD_PATHS_CAP`]. The path is stored as-given — callers that
    /// want canonicalization must do it themselves.
    pub fn push_recent_add_path(&mut self, path: String) {
        self.recent_add_paths.retain(|p| p != &path);
        self.recent_add_paths.insert(0, path);
        self.recent_add_paths.truncate(RECENT_ADD_PATHS_CAP);
    }

    #[must_use]
    pub fn recent_add_paths(&self) -> &[String] {
        &self.recent_add_paths
    }
}

/// Error returned by [`Persistence::flush_blocking`] / [`FlushWaiter::wait`]
/// when the deadline elapses before all pending writes reach disk, or when
/// the worker has already shut down with pending state.
#[derive(Debug)]
pub struct FlushTimeout;

impl fmt::Display for FlushTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("timed out waiting for persistence flush")
    }
}

impl std::error::Error for FlushTimeout {}

/// Opaque handle that lets the caller release an outer mutex around
/// `Persistence` before blocking on a flush. Obtained via
/// [`Persistence::flush_waiter`].
pub struct FlushWaiter {
    shared: Arc<SharedSaver>,
    target: u64,
}

impl FlushWaiter {
    /// Block until the target version is on disk or `timeout` elapses.
    pub fn wait(self, timeout: Duration) -> Result<(), FlushTimeout> {
        flush_shared_blocking(&self.shared, self.target, timeout)
    }
}

/// Blocking wait on the worker's shared state. Used by
/// [`Persistence::flush_blocking`] and [`FlushWaiter::wait`] so both paths
/// share the same condvar logic.
fn flush_shared_blocking(
    shared: &Arc<SharedSaver>,
    target: u64,
    timeout: Duration,
) -> Result<(), FlushTimeout> {
    let deadline = Instant::now() + timeout;
    // Mutex poisoning is treated as a process-fatal event — the worker is
    // the only critical section and a poisoned lock means we already
    // crashed somewhere upstream. Matches pre-RFC-004 behaviour.
    let mut inner = shared.mu.lock().unwrap();
    loop {
        if inner.last_saved_version >= target {
            return Ok(());
        }
        if inner.shutdown {
            // Worker has exited and can no longer advance last_saved_version.
            // Treat this as a timeout — the caller's pending data will not
            // reach disk via this handle.
            return Err(FlushTimeout);
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(FlushTimeout);
        };
        // Mutex poisoning: fatal by design.
        let (new_inner, result) = shared.cv.wait_timeout(inner, remaining).unwrap();
        inner = new_inner;
        if result.timed_out() && inner.last_saved_version < target {
            return Err(FlushTimeout);
        }
    }
}

/// Pluggable writer so tests can observe / inject failures without real I/O.
pub trait PersistenceWriter: Send + Sync {
    fn write(&self, path: &Path, state: &GuiState) -> std::io::Result<()>;
}

/// Production writer: atomic write with rolling `.bak` backup. Produces byte
/// output identical to the pre-RFC-004 synchronous implementation.
pub struct FileWriter;

impl PersistenceWriter for FileWriter {
    fn write(&self, path: &Path, state: &GuiState) -> std::io::Result<()> {
        atomic_write_with_backup(path, state)
    }
}

/// Atomic-write-with-rolling-backup routine — must stay byte-compatible with
/// the legacy synchronous implementation.
fn atomic_write_with_backup(path: &Path, state: &GuiState) -> std::io::Result<()> {
    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Rolling backup: rename existing to .bak. Tolerate `NotFound` for the
    // first-write case; any other error is surfaced so the caller can retry.
    // No `path.exists()` TOCTOU pre-check — `fs::rename` is the authoritative
    // existence test.
    let bak = path.with_extension("json.bak");
    match std::fs::rename(path, &bak) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    // Write to .tmp first, then atomically rename over the real path.
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;

    // Write + fsync inside a block so the file handle is closed before rename.
    let write_result: std::io::Result<()> = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        // Clean up the half-written tmp so we do not leak disk on repeated
        // failures.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        // Rename failed after a successful write — the tmp still holds the
        // serialized state but will never land at the real path. Delete it
        // so `.tmp` does not accumulate.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    tracing::debug!(path = %path.display(), "saved GUI state");
    Ok(())
}

/// Shared state between the caller and the background worker.
struct SharedSaver {
    mu: Mutex<SaverInner>,
    cv: Condvar,
    debounce: Duration,
    writer: Box<dyn PersistenceWriter>,
    path: PathBuf,
}

struct SaverInner {
    /// Newest snapshot queued for writing. Overwritten on every update —
    /// only the latest survives, which is the coalescing invariant.
    pending: Option<GuiState>,
    pending_version: u64,
    last_saved_version: u64,
    shutdown: bool,
}

/// Caller-side handle. Held inside `AppState` behind a `Mutex`.
pub struct Persistence {
    state: GuiState,
    data_version: u64,
    shared: Arc<SharedSaver>,
    worker: Option<JoinHandle<()>>,
}

impl Persistence {
    /// Load state from disk and spawn the background writer. Returns default
    /// state on any parse/IO error.
    pub fn load() -> Self {
        let path = state_file_path();
        let state = load_state_from_disk(&path);
        Self::new_with_state(path, state, Box::new(FileWriter), DEFAULT_DEBOUNCE)
    }

    /// Construct a persistence handle with an injected writer and debounce.
    /// Used by tests to observe coalescing and failure behaviour without
    /// touching the real filesystem. The file at `path` is NOT loaded — the
    /// initial state is [`GuiState::default`].
    pub fn with_writer(
        path: PathBuf,
        writer: Box<dyn PersistenceWriter>,
        debounce: Duration,
    ) -> Self {
        Self::new_with_state(path, GuiState::default(), writer, debounce)
    }

    fn new_with_state(
        path: PathBuf,
        state: GuiState,
        writer: Box<dyn PersistenceWriter>,
        debounce: Duration,
    ) -> Self {
        let shared = Arc::new(SharedSaver {
            mu: Mutex::new(SaverInner {
                pending: None,
                pending_version: 0,
                last_saved_version: 0,
                shutdown: false,
            }),
            cv: Condvar::new(),
            debounce,
            writer,
            path,
        });

        let worker_shared = Arc::clone(&shared);
        // Thread spawn failure is unrecoverable: without the worker, no disk
        // I/O can ever happen, so there is no graceful degradation path.
        // `expect` here surfaces the failure immediately rather than returning
        // a half-initialized handle.
        let worker = thread::Builder::new()
            .name("zremote-persistence".to_string())
            .spawn(move || run_worker(&worker_shared))
            .expect("failed to spawn persistence worker thread");

        Self {
            state,
            data_version: 0,
            shared,
            worker: Some(worker),
        }
    }

    pub fn state(&self) -> &GuiState {
        &self.state
    }

    /// Mutate state, bump the data version, and queue a save. Non-blocking:
    /// returns after touching only in-memory fields and the saver mutex.
    pub fn update(&mut self, f: impl FnOnce(&mut GuiState)) {
        f(&mut self.state);
        self.data_version += 1;
        self.queue_save();
    }

    /// Record a session access, moving it to the front of the recent list.
    /// Keeps at most 10 entries. Save is queued automatically via `update`.
    pub fn record_session_access(&mut self, session_id: &str) {
        self.update(|state| {
            state.recent_sessions.retain(|r| r.session_id != session_id);
            state.recent_sessions.insert(
                0,
                RecentSession {
                    session_id: session_id.to_string(),
                    timestamp: i64::try_from(
                        SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_secs()),
                    )
                    .unwrap_or(0),
                },
            );
            state.recent_sessions.truncate(10);
        });
    }

    /// Record a palette action usage. Deduplicates by key, moves to front,
    /// caps at 20. Save is queued automatically via `update`.
    pub fn record_action_usage(&mut self, action_key: &str) {
        self.update(|state| {
            state.recent_actions.retain(|r| r.action_key != action_key);
            state.recent_actions.insert(
                0,
                RecentAction {
                    action_key: action_key.to_string(),
                    timestamp: i64::try_from(
                        SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_secs()),
                    )
                    .unwrap_or(0),
                },
            );
            if state.recent_actions.len() > 20 {
                state.recent_actions.truncate(20);
            }
        });
    }

    /// Clear all recent palette actions (used from settings UI).
    pub fn clear_recent_actions(&mut self) {
        self.update(|state| {
            state.recent_actions.clear();
        });
    }

    /// Record an explicit user override for whether a parent project's
    /// worktree children are shown in the sidebar. `default_expanded` is the
    /// value computed from the auto-heuristic before the click — we only
    /// persist the *opposite* of the default so that future changes to the
    /// heuristic (e.g. lowering the collapse threshold) are respected unless
    /// the user has actively pushed back. The two sets are mutually exclusive:
    /// writing to one always clears the matching id from the other.
    ///
    /// Rotation when the user keeps clicking:
    ///   default → explicit-opposite → default (no override) → explicit-opposite → …
    pub fn toggle_project_expanded(&mut self, project_id: &str, default_expanded: bool) {
        self.update(|state| {
            let had_explicit = state.expanded_projects.remove(project_id)
                || state.collapsed_projects.remove(project_id);
            if had_explicit {
                // Returning to default heuristic on this click.
                return;
            }
            // First click on a project in default state: store the opposite.
            if default_expanded {
                state.collapsed_projects.insert(project_id.to_string());
            } else {
                state.expanded_projects.insert(project_id.to_string());
            }
        });
    }

    /// Mark a project as explicitly expanded (clears any force-collapse
    /// override for it). Used by auto-expand-on-activity paths where we want
    /// to persist the new expanded state regardless of prior user choice.
    pub fn set_project_expanded(&mut self, project_id: &str, expanded: bool) {
        self.update(|state| {
            if expanded {
                state.collapsed_projects.remove(project_id);
                state.expanded_projects.insert(project_id.to_string());
            } else {
                state.expanded_projects.remove(project_id);
                state.collapsed_projects.insert(project_id.to_string());
            }
        });
    }

    /// Block the current thread until every mutation up to `self.data_version`
    /// is on disk, or `timeout` elapses. Returns [`FlushTimeout`] on timeout
    /// or if the worker has already shut down with unpersisted state.
    ///
    /// Note: the worker advances `last_saved_version` even when it skips a
    /// default-state snapshot, so this method will not spin in that case.
    ///
    /// Holds the internal `SharedSaver::mu` lock only while waiting on the
    /// condition variable — the worker's critical section is always short,
    /// so the wait does not starve the I/O thread.
    pub fn flush_blocking(&mut self, timeout: Duration) -> Result<(), FlushTimeout> {
        flush_shared_blocking(&self.shared, self.data_version, timeout)
    }

    /// Cheap snapshot of the flush target plus a shareable handle to the
    /// worker state, so the caller can release any outer mutex around
    /// `Persistence` before blocking on the flush. Use when `Persistence`
    /// is itself held inside another mutex (e.g. `AppState::persistence`)
    /// and you do not want to stall other access to that mutex for up to
    /// `timeout`.
    pub fn flush_waiter(&self) -> FlushWaiter {
        FlushWaiter {
            shared: Arc::clone(&self.shared),
            target: self.data_version,
        }
    }

    #[cfg(test)]
    fn last_saved_version(&self) -> u64 {
        // Mutex poisoning: fatal by design.
        self.shared.mu.lock().unwrap().last_saved_version
    }

    fn queue_save(&mut self) {
        self.state.version = FORMAT_VERSION;
        let snapshot = self.state.clone();
        let version = self.data_version;
        // Mutex poisoning: fatal by design.
        let mut inner = self.shared.mu.lock().unwrap();
        inner.pending = Some(snapshot);
        inner.pending_version = version;
        drop(inner);
        self.shared.cv.notify_all();
    }
}

impl Drop for Persistence {
    fn drop(&mut self) {
        // Best-effort flush with a bounded deadline. Skips the wait entirely
        // when `last_saved_version >= data_version` (cheap first-iteration
        // check in `flush_shared_blocking`) so a caller that already flushed
        // explicitly does not pay a second 2 s stall here.
        let _ = self.flush_blocking(DROP_FLUSH_TIMEOUT);
        // Signal shutdown and wake the worker.
        {
            // Mutex poisoning: fatal by design.
            let mut inner = self.shared.mu.lock().unwrap();
            inner.shutdown = true;
        }
        self.shared.cv.notify_all();
        if let Some(handle) = self.worker.take()
            && let Err(panic_payload) = handle.join()
        {
            // A worker panic means subsequent `update` calls are silently
            // dropped from the retry-queue perspective. Log loudly so the
            // failure is visible in tracing output rather than disappearing.
            tracing::error!(
                ?panic_payload,
                "persistence worker thread panicked before shutdown",
            );
        }
    }
}

/// Worker loop: see RFC-004 Architecture § Worker loop.
fn run_worker(shared: &Arc<SharedSaver>) {
    loop {
        // Phase 1: wait for pending work or shutdown.
        {
            // Mutex poisoning: fatal by design.
            let mut inner = shared.mu.lock().unwrap();
            while inner.pending.is_none() && !inner.shutdown {
                inner = shared.cv.wait(inner).unwrap();
            }
            if inner.shutdown && inner.pending.is_none() {
                break;
            }
        }

        // Phase 2: debounce — release the lock, let additional updates
        // coalesce into the same `pending` slot. Interruptible by shutdown:
        // a shutdown signal that arrives during this wait wakes us early,
        // so the worker can drain the latest pending snapshot and exit.
        {
            // Mutex poisoning: fatal by design.
            let inner = shared.mu.lock().unwrap();
            let _guard = shared
                .cv
                .wait_timeout_while(inner, shared.debounce, |st| !st.shutdown)
                .unwrap()
                .0;
        }

        // Phase 3: take the latest snapshot under the lock. `snapshot` is
        // owned by the worker from here onwards so phase 5 can re-queue it
        // if the write in phase 4 fails.
        let taken = {
            // Mutex poisoning: fatal by design.
            let mut inner = shared.mu.lock().unwrap();
            if let Some(snapshot) = inner.pending.take() {
                Some((snapshot, inner.pending_version))
            } else if inner.shutdown {
                break;
            } else {
                None
            }
        };
        let Some((snapshot, version)) = taken else {
            continue;
        };

        // Phase 4: write OUTSIDE the lock. Skip default-state snapshots to
        // match the pre-RFC-004 "don't persist empty state" guard. We still
        // advance `last_saved_version` in phase 5 so that `flush_blocking`
        // does not spin on a write that will never happen.
        let is_default_skip = snapshot.is_default();
        let result: std::io::Result<()> = if is_default_skip {
            Ok(())
        } else {
            shared.writer.write(&shared.path, &snapshot)
        };

        // Phase 5: publish result, notify waiters. On failure, re-queue the
        // snapshot so the worker retries after the next phase 1 wake-up —
        // unless a newer update has already replaced it (coalescing: newest
        // wins). The caller-side `flush_blocking` will keep waiting until
        // either a retry succeeds or its deadline elapses.
        {
            // Mutex poisoning: fatal by design.
            let mut inner = shared.mu.lock().unwrap();
            match result {
                Ok(()) => {
                    inner.last_saved_version = version;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %shared.path.display(),
                        "persistence worker write failed; will retry",
                    );
                    if inner.pending.is_none() {
                        // Nothing newer arrived while we were writing —
                        // put the failed snapshot back so the next loop
                        // iteration retries it.
                        inner.pending = Some(snapshot);
                        inner.pending_version = version;
                    }
                    // If `inner.pending` is already Some, a newer snapshot
                    // superseded this one during the write — drop the
                    // failed one on the floor; the newer write wins.
                }
            }
            drop(inner);
            shared.cv.notify_all();
        }
    }
    tracing::debug!("persistence worker exiting");
}

/// Upper bound on entries kept in any project-id HashSet loaded from disk.
/// A corrupt or malicious state file that lists millions of ids would
/// otherwise bloat memory and slow down every `lookup` on the render path.
const PROJECT_SET_CAP: usize = 10_000;

/// Upper bound on the length of a single project id. Anything longer is a
/// corruption signal, not a legitimate uuid/path-derived id. Entries past
/// this bound are dropped silently (one-shot warn) during load.
const PROJECT_ID_MAX_LEN: usize = 64;

/// Sanitize a HashSet of project ids loaded from disk: strips entries
/// longer than [`PROJECT_ID_MAX_LEN`] and truncates to [`PROJECT_SET_CAP`]
/// entries. Logs once per load if either limit triggered.
fn sanitize_project_id_set(set: &mut HashSet<String>, field: &'static str) {
    let before = set.len();
    set.retain(|id| id.len() <= PROJECT_ID_MAX_LEN);
    let after_length_filter = set.len();
    if after_length_filter < before {
        tracing::warn!(
            field,
            dropped = before - after_length_filter,
            "persistence: project ids exceeded max length, dropped",
        );
    }
    if set.len() > PROJECT_SET_CAP {
        // Iteration order on HashSet is unspecified; arbitrary truncation
        // is acceptable because the sets are hints, not authoritative data.
        let retained: HashSet<String> = set.iter().take(PROJECT_SET_CAP).cloned().collect();
        let dropped = set.len() - retained.len();
        *set = retained;
        tracing::warn!(
            field,
            dropped,
            cap = PROJECT_SET_CAP,
            "persistence: project-id set exceeded cap, truncated",
        );
    }
}

fn load_state_from_disk(path: &Path) -> GuiState {
    if !path.exists() {
        return GuiState::default();
    }
    match std::fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str::<GuiState>(&contents) {
            Ok(mut state) => {
                sanitize_project_id_set(&mut state.expanded_projects, "expanded_projects");
                sanitize_project_id_set(&mut state.collapsed_projects, "collapsed_projects");
                state
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "failed to parse GUI state, using defaults",
                );
                GuiState::default()
            }
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "failed to read GUI state file",
            );
            GuiState::default()
        }
    }
}

fn state_file_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("zremote")
        .join("gui-state.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    // -------- Test writers --------

    struct CountingWriter {
        calls: Arc<AtomicU64>,
        last_written: Arc<Mutex<Option<GuiState>>>,
    }

    impl PersistenceWriter for CountingWriter {
        fn write(&self, _path: &Path, state: &GuiState) -> std::io::Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            // Mutex poisoning: test-only, a poisoned lock would fail the test.
            *self.last_written.lock().unwrap() = Some(state.clone());
            Ok(())
        }
    }

    struct BlockingWriter {
        unblock: Arc<(Mutex<bool>, Condvar)>,
        calls: Arc<AtomicU64>,
    }

    impl PersistenceWriter for BlockingWriter {
        fn write(&self, _path: &Path, _state: &GuiState) -> std::io::Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let (lock, cv) = &*self.unblock;
            // Mutex poisoning: test-only.
            let mut unblocked = lock.lock().unwrap();
            while !*unblocked {
                unblocked = cv.wait(unblocked).unwrap();
            }
            Ok(())
        }
    }

    struct FailingWriter {
        calls: Arc<AtomicU64>,
    }

    impl PersistenceWriter for FailingWriter {
        fn write(&self, _path: &Path, _state: &GuiState) -> std::io::Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(std::io::Error::other("synthetic failure"))
        }
    }

    /// Fails `fail_count` times, then returns `Ok` on subsequent calls.
    /// Used to verify retry-to-success in `test_write_retry_on_transient_failure`.
    struct FlakyWriter {
        remaining_failures: Arc<AtomicU64>,
        calls: Arc<AtomicU64>,
    }

    impl PersistenceWriter for FlakyWriter {
        fn write(&self, _path: &Path, _state: &GuiState) -> std::io::Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.remaining_failures.load(Ordering::Relaxed) > 0 {
                self.remaining_failures.fetch_sub(1, Ordering::Relaxed);
                Err(std::io::Error::other("transient synthetic failure"))
            } else {
                Ok(())
            }
        }
    }

    struct SleepingWriter {
        sleep: Duration,
        calls: Arc<AtomicU64>,
    }

    impl PersistenceWriter for SleepingWriter {
        fn write(&self, _path: &Path, _state: &GuiState) -> std::io::Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            thread::sleep(self.sleep);
            Ok(())
        }
    }

    /// Writer that signals a `Drop` sentinel when the worker releases it.
    /// Used by `test_worker_terminates_on_drop` to prove the worker thread
    /// has released its `Arc<SharedSaver>` (which holds the writer) after
    /// the caller drops the `Persistence`.
    struct SentinelWriter {
        calls: Arc<AtomicU64>,
        dropped: Arc<AtomicBool>,
    }

    impl PersistenceWriter for SentinelWriter {
        fn write(&self, _path: &Path, _state: &GuiState) -> std::io::Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    impl Drop for SentinelWriter {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Relaxed);
        }
    }

    // -------- Helpers --------

    fn temp_path(label: &str) -> PathBuf {
        // Per-test unique directory under the system temp dir. Tests only
        // ever write inside this directory, so it is safe to leave behind
        // for the OS to clean up. No tempdir guard is created — which means
        // no leaked drop logic.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join("zremote-persist-tests")
            .join(format!("{}-{label}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create test temp dir");
        dir.join(format!("{label}.json"))
    }

    fn counting_writer() -> (
        Box<CountingWriter>,
        Arc<AtomicU64>,
        Arc<Mutex<Option<GuiState>>>,
    ) {
        let calls = Arc::new(AtomicU64::new(0));
        let last = Arc::new(Mutex::new(None));
        let writer = Box::new(CountingWriter {
            calls: Arc::clone(&calls),
            last_written: Arc::clone(&last),
        });
        (writer, calls, last)
    }

    fn non_default_mutation(state: &mut GuiState) {
        // Touch a field that makes `is_default()` return false so the worker
        // actually writes the snapshot.
        state.server_url = Some("http://example:1234".to_string());
    }

    // -------- Tests --------

    #[test]
    fn test_update_is_nonblocking() {
        let calls = Arc::new(AtomicU64::new(0));
        let writer = Box::new(SleepingWriter {
            sleep: Duration::from_millis(100),
            calls: Arc::clone(&calls),
        });
        let path = temp_path("nonblocking");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        let start = Instant::now();
        for i in 0..20u32 {
            p.update(|s| {
                s.server_url = Some(format!("http://host:{i}"));
            });
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "20 updates took {elapsed:?}, expected < 50 ms",
        );
        // Drop runs flush_blocking(2 s). With 20 updates coalesced, the
        // worker performs at most 2 writes of 100 ms each, well under the
        // drop deadline — this is intentional, just observed here.
        drop(p);
    }

    #[test]
    fn test_coalescing_100_updates() {
        let (writer, calls, _) = counting_writer();
        let path = temp_path("coalescing");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(30));

        for i in 0..100u32 {
            p.update(|s| {
                s.server_url = Some(format!("http://host:{i}"));
            });
        }

        p.flush_blocking(Duration::from_secs(2))
            .expect("flush should complete within 2 s");

        let n = calls.load(Ordering::Relaxed);
        assert!(n <= 2, "expected <= 2 writes, got {n}");
        assert!(n >= 1, "expected at least one write, got {n}");
    }

    #[test]
    fn test_flush_blocking_returns_after_save() {
        let (writer, calls, _) = counting_writer();
        let path = temp_path("flush-returns");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        p.update(non_default_mutation);
        p.flush_blocking(Duration::from_secs(1)).expect("flush ok");

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(p.last_saved_version(), p.data_version);
    }

    #[test]
    fn test_flush_blocking_timeout() {
        let unblock = Arc::new((Mutex::new(false), Condvar::new()));
        let calls = Arc::new(AtomicU64::new(0));
        let writer = Box::new(BlockingWriter {
            unblock: Arc::clone(&unblock),
            calls: Arc::clone(&calls),
        });
        let path = temp_path("flush-timeout");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        p.update(non_default_mutation);
        let result = p.flush_blocking(Duration::from_millis(50));
        assert!(matches!(result, Err(FlushTimeout)));

        // Unblock the writer so the worker can exit cleanly on drop.
        {
            let (lock, cv) = &*unblock;
            let mut v = lock.lock().unwrap();
            *v = true;
            cv.notify_all();
        }
        drop(p);
    }

    #[test]
    fn test_drop_flushes_pending_writes() {
        let (writer, calls, _) = counting_writer();
        let path = temp_path("drop-flushes");
        {
            let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));
            p.update(non_default_mutation);
            // Drop without explicit flush — Drop should call flush_blocking(2s).
        }
        assert!(
            calls.load(Ordering::Relaxed) >= 1,
            "writer should have been called at least once",
        );
    }

    #[test]
    fn test_write_failure_does_not_advance_last_saved() {
        let calls = Arc::new(AtomicU64::new(0));
        let writer = Box::new(FailingWriter {
            calls: Arc::clone(&calls),
        });
        let path = temp_path("write-failure");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        p.update(non_default_mutation);
        let result = p.flush_blocking(Duration::from_millis(100));
        assert!(matches!(result, Err(FlushTimeout)));
        assert!(
            calls.load(Ordering::Relaxed) >= 1,
            "writer should have been attempted",
        );
        // On permanent failure the worker keeps retrying the same snapshot
        // indefinitely (bounded by the debounce window between attempts).
        // We verify that by re-reading the call count after another short
        // window: it must have grown past the initial attempt.
        let calls_at_timeout = calls.load(Ordering::Relaxed);
        thread::sleep(Duration::from_millis(80));
        assert!(
            calls.load(Ordering::Relaxed) > calls_at_timeout,
            "worker should retry failed writes; calls={calls_at_timeout} then no growth",
        );
        // Drop runs flush_blocking(2 s) which will hit its own timeout —
        // that is the intended permanent-failure contract. To keep this
        // test fast we forget the `Persistence` instead of dropping it.
        // The leaked worker thread exits when the process ends.
        std::mem::forget(p);
    }

    #[test]
    fn test_write_retry_on_transient_failure() {
        // Writer fails the first 3 attempts then succeeds. Verifies that
        // the worker re-queues the failed snapshot and retries without
        // losing data, and that `flush_blocking` eventually returns Ok.
        let calls = Arc::new(AtomicU64::new(0));
        let remaining = Arc::new(AtomicU64::new(3));
        let writer = Box::new(FlakyWriter {
            remaining_failures: Arc::clone(&remaining),
            calls: Arc::clone(&calls),
        });
        let path = temp_path("flaky-retry");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        p.update(non_default_mutation);
        p.flush_blocking(Duration::from_secs(3))
            .expect("flaky writer should eventually succeed");

        let total_calls = calls.load(Ordering::Relaxed);
        assert!(
            total_calls >= 4,
            "expected at least 3 failed attempts + 1 success, got {total_calls}",
        );
        assert_eq!(
            remaining.load(Ordering::Relaxed),
            0,
            "all transient failures should have been consumed",
        );
        assert_eq!(p.last_saved_version(), p.data_version);
    }

    #[test]
    fn test_default_state_is_not_written() {
        // Sanity guard: if a future change to `GuiState` breaks the
        // `is_default()` invariant (e.g. a new field whose default is
        // treated as non-default), this assertion fails loudly instead of
        // silently flipping the test into a meaningless pass.
        assert!(
            GuiState::default().is_default(),
            "GuiState::default() must satisfy is_default() — \
             otherwise the default-state skip guard below becomes a no-op",
        );

        let (writer, calls, _) = counting_writer();
        let path = temp_path("default-skip");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        // Bump the version without leaving default state.
        p.update(|_| {});
        p.flush_blocking(Duration::from_secs(1))
            .expect("flush should complete even when snapshot is default");

        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "default state must not be written",
        );
        assert_eq!(
            p.last_saved_version(),
            p.data_version,
            "last_saved_version must advance even when the write was skipped",
        );
    }

    #[test]
    fn test_record_session_access_queues_save() {
        let (writer, calls, last) = counting_writer();
        let path = temp_path("record-session");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        p.record_session_access("abc123");
        p.flush_blocking(Duration::from_secs(1)).expect("flush ok");

        assert!(calls.load(Ordering::Relaxed) >= 1);
        let snapshot = last.lock().unwrap().clone().expect("snapshot");
        assert_eq!(snapshot.recent_sessions.len(), 1);
        assert_eq!(snapshot.recent_sessions[0].session_id, "abc123");
    }

    #[test]
    fn test_worker_terminates_on_drop() {
        let calls = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let writer = Box::new(SentinelWriter {
            calls: Arc::clone(&calls),
            dropped: Arc::clone(&dropped),
        });
        let path = temp_path("worker-join");
        let start = Instant::now();
        {
            let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));
            p.update(non_default_mutation);
            // Drop runs flush_blocking + signal + join. If the worker hangs
            // the test itself hangs — the time bound below catches that.
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "drop should return promptly, took {elapsed:?}",
        );
        assert!(
            dropped.load(Ordering::Relaxed),
            "SentinelWriter should have been dropped (worker released the Arc)",
        );
        assert!(calls.load(Ordering::Relaxed) >= 1);
    }

    #[test]
    fn test_burst_then_idle_then_burst() {
        let (writer, calls, _) = counting_writer();
        let path = temp_path("burst-idle-burst");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(20));

        for i in 0..50u32 {
            p.update(|s| {
                s.server_url = Some(format!("http://first:{i}"));
            });
        }
        p.flush_blocking(Duration::from_secs(2)).expect("flush 1");
        let after_first = calls.load(Ordering::Relaxed);

        // Idle interval large enough for the worker to park on the cv.
        thread::sleep(Duration::from_millis(100));

        for i in 0..50u32 {
            p.update(|s| {
                s.server_url = Some(format!("http://second:{i}"));
            });
        }
        p.flush_blocking(Duration::from_secs(2)).expect("flush 2");
        let total = calls.load(Ordering::Relaxed);

        assert!(
            (1..=2).contains(&after_first),
            "first burst writes = {after_first}"
        );
        assert!(
            (2..=4).contains(&total),
            "total writes across both bursts expected 2..=4, got {total}",
        );
    }

    #[test]
    fn test_file_writer_produces_same_bytes_as_reference() {
        let path = temp_path("file-writer-bytes");
        let state = GuiState {
            version: 1,
            server_url: Some("http://a:1".to_string()),
            active_session_id: Some("s".to_string()),
            window_width: Some(800.0),
            window_height: Some(600.0),
            recent_sessions: vec![RecentSession {
                session_id: "s".to_string(),
                timestamp: 1_700_000_000,
            }],
            recent_actions: vec![],
            activity_panel_visible: false,
            expanded_projects: HashSet::new(),
            collapsed_projects: HashSet::new(),
            recent_add_paths: Vec::new(),
        };

        FileWriter.write(&path, &state).expect("write");
        let actual = std::fs::read_to_string(&path).expect("read back");

        let expected = "{\n  \"version\": 1,\n  \"server_url\": \"http://a:1\",\n  \"active_session_id\": \"s\",\n  \"window_width\": 800.0,\n  \"window_height\": 600.0,\n  \"recent_sessions\": [\n    {\n      \"session_id\": \"s\",\n      \"timestamp\": 1700000000\n    }\n  ]\n}";
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_record_action_usage() {
        let (writer, _, _) = counting_writer();
        let path = temp_path("record-action");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        p.record_action_usage("NewSession");
        p.record_action_usage("SearchInTerminal");
        p.record_action_usage("NewSession"); // duplicate — should move to front

        let actions = &p.state().recent_actions;
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].action_key, "NewSession");
        assert_eq!(actions[1].action_key, "SearchInTerminal");
    }

    #[test]
    fn test_record_action_usage_cap() {
        let (writer, _, _) = counting_writer();
        let path = temp_path("record-action-cap");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        for i in 0..25 {
            p.record_action_usage(&format!("Action{i}"));
        }
        assert_eq!(p.state().recent_actions.len(), 20);
        assert_eq!(p.state().recent_actions[0].action_key, "Action24");
    }

    #[test]
    fn test_clear_recent_actions() {
        let (writer, _, _) = counting_writer();
        let path = temp_path("clear-actions");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        p.record_action_usage("NewSession");
        p.record_action_usage("Search");
        assert_eq!(p.state().recent_actions.len(), 2);

        p.clear_recent_actions();
        assert!(p.state().recent_actions.is_empty());
    }

    #[test]
    fn test_backward_compat_v1() {
        let v1_json = r#"{"version":1,"recent_sessions":[]}"#;
        let state: GuiState = serde_json::from_str(v1_json).unwrap();
        assert!(state.recent_actions.is_empty());
    }

    #[test]
    fn test_backward_compat_without_collapsed_projects() {
        // State written before collapsed_projects was added must still load.
        let legacy_json = r#"{"version":2,"recent_sessions":[],"expanded_projects":["pa"]}"#;
        let state: GuiState = serde_json::from_str(legacy_json).unwrap();
        assert!(state.expanded_projects.contains("pa"));
        assert!(state.collapsed_projects.is_empty());
    }

    #[test]
    fn test_toggle_project_expanded_rotation() {
        // With default_expanded = true (heuristic says "show"), first click
        // overrides to collapsed, second click clears the override
        // (returning to default = expanded), third click overrides to
        // collapsed again. Symmetric when the default is false.
        let (writer, _, _) = counting_writer();
        let path = temp_path("toggle-rotation");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        // Start: no override recorded anywhere.
        assert!(!p.state().expanded_projects.contains("pa"));
        assert!(!p.state().collapsed_projects.contains("pa"));

        // Default = expanded: first toggle should stash `pa` into collapsed.
        p.toggle_project_expanded("pa", true);
        assert!(p.state().collapsed_projects.contains("pa"));
        assert!(!p.state().expanded_projects.contains("pa"));

        // Second toggle: clears the override entirely (back to default).
        p.toggle_project_expanded("pa", true);
        assert!(!p.state().collapsed_projects.contains("pa"));
        assert!(!p.state().expanded_projects.contains("pa"));

        // Third toggle: same as first — record the opposite of default.
        p.toggle_project_expanded("pa", true);
        assert!(p.state().collapsed_projects.contains("pa"));

        // Flip the default: now the toggle should write to expanded.
        p.toggle_project_expanded("pb", false);
        assert!(p.state().expanded_projects.contains("pb"));
        assert!(!p.state().collapsed_projects.contains("pb"));
    }

    #[test]
    fn test_toggle_project_expanded_clears_opposing_set() {
        // Explicitly expanded then toggled with default=true should clear
        // the expanded entry, not add it to collapsed as well. The two
        // sets must stay mutually exclusive.
        let (writer, _, _) = counting_writer();
        let path = temp_path("toggle-exclusive");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        p.set_project_expanded("pa", true);
        assert!(p.state().expanded_projects.contains("pa"));
        assert!(!p.state().collapsed_projects.contains("pa"));

        // Toggling with default=true should clear the expanded entry
        // (had_explicit branch) without ever touching collapsed.
        p.toggle_project_expanded("pa", true);
        assert!(!p.state().expanded_projects.contains("pa"));
        assert!(!p.state().collapsed_projects.contains("pa"));
    }

    #[test]
    fn test_set_project_expanded_keeps_sets_mutually_exclusive() {
        let (writer, _, _) = counting_writer();
        let path = temp_path("set-exclusive");
        let mut p = Persistence::with_writer(path, writer, Duration::from_millis(10));

        // Force collapsed, then force expanded — collapsed must be cleared.
        p.set_project_expanded("pa", false);
        assert!(p.state().collapsed_projects.contains("pa"));
        assert!(!p.state().expanded_projects.contains("pa"));

        p.set_project_expanded("pa", true);
        assert!(!p.state().collapsed_projects.contains("pa"));
        assert!(p.state().expanded_projects.contains("pa"));

        // And back the other way.
        p.set_project_expanded("pa", false);
        assert!(p.state().collapsed_projects.contains("pa"));
        assert!(!p.state().expanded_projects.contains("pa"));
    }

    #[test]
    fn test_load_truncates_oversized_project_set() {
        // A state file with >10_000 ids should be truncated to the cap on
        // load; the exact retained entries are unspecified because HashSet
        // iteration order is random, so we only verify the cap.
        let mut big = HashSet::new();
        for i in 0..(PROJECT_SET_CAP + 250) {
            big.insert(format!("id-{i:06}"));
        }
        let state = GuiState {
            expanded_projects: big,
            ..GuiState::default()
        };
        let tmp = temp_path("oversized-set");
        FileWriter.write(&tmp, &state).unwrap();
        let loaded = load_state_from_disk(&tmp);
        assert_eq!(loaded.expanded_projects.len(), PROJECT_SET_CAP);
    }

    #[test]
    fn test_load_drops_overlong_ids() {
        // Ids longer than PROJECT_ID_MAX_LEN must be stripped at load time.
        let mut ids = HashSet::new();
        ids.insert("short".to_string());
        ids.insert("x".repeat(PROJECT_ID_MAX_LEN + 1));
        let state = GuiState {
            collapsed_projects: ids,
            ..GuiState::default()
        };
        let tmp = temp_path("overlong-ids");
        FileWriter.write(&tmp, &state).unwrap();
        let loaded = load_state_from_disk(&tmp);
        assert_eq!(loaded.collapsed_projects.len(), 1);
        assert!(loaded.collapsed_projects.contains("short"));
    }

    #[test]
    fn test_toggle_project_expanded_persists() {
        // End-to-end: toggle once and confirm the worker wrote the
        // override to disk by loading the file back into a fresh state.
        let (writer, calls, _) = counting_writer();
        let path = temp_path("toggle-persists");
        {
            let mut p = Persistence::with_writer(path.clone(), writer, Duration::from_millis(10));
            p.toggle_project_expanded("pa", true);
            p.flush_blocking(Duration::from_secs(1))
                .expect("flush after toggle");
        }
        assert!(calls.load(Ordering::Relaxed) >= 1);
        // Using the counting writer the file itself isn't written, so
        // also exercise the FileWriter round-trip separately for full
        // persistence coverage.
        let path2 = temp_path("toggle-persists-fs");
        {
            let mut p = Persistence::with_writer(
                path2.clone(),
                Box::new(FileWriter),
                Duration::from_millis(10),
            );
            p.toggle_project_expanded("pa", true);
            p.flush_blocking(Duration::from_secs(1)).expect("flush");
        }
        let reloaded = load_state_from_disk(&path2);
        assert!(reloaded.collapsed_projects.contains("pa"));
    }

    #[test]
    fn push_recent_add_path_adds_to_front() {
        let mut state = GuiState::default();
        state.push_recent_add_path("a".to_string());
        state.push_recent_add_path("b".to_string());
        assert_eq!(
            state.recent_add_paths(),
            &["b".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn push_recent_add_path_dedupes() {
        let mut state = GuiState::default();
        state.push_recent_add_path("a".to_string());
        state.push_recent_add_path("b".to_string());
        state.push_recent_add_path("a".to_string());
        assert_eq!(
            state.recent_add_paths(),
            &["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn push_recent_add_path_trims_to_20() {
        let mut state = GuiState::default();
        for i in 0..25 {
            state.push_recent_add_path(format!("/path/{i}"));
        }
        assert_eq!(state.recent_add_paths().len(), RECENT_ADD_PATHS_CAP);
        assert_eq!(state.recent_add_paths()[0], "/path/24");
        // The 5 oldest pushes must have fallen off the back.
        assert!(!state.recent_add_paths().iter().any(|p| p == "/path/0"));
        assert!(!state.recent_add_paths().iter().any(|p| p == "/path/4"));
        // The 20 newest must still be present.
        assert_eq!(
            state.recent_add_paths()[RECENT_ADD_PATHS_CAP - 1],
            "/path/5"
        );
    }

    #[test]
    fn deserialize_without_recent_add_paths_field() {
        // A state blob written before recent_add_paths existed must still
        // load with an empty vec for the new field (serde(default)).
        let legacy_json = r#"{"version":2,"recent_sessions":[]}"#;
        let state: GuiState = serde_json::from_str(legacy_json).unwrap();
        assert!(state.recent_add_paths().is_empty());
    }
}
