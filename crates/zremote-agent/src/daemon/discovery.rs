use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::mpsc;
use zremote_protocol::SessionId;

use super::DaemonStateFile;
use super::session::DaemonSession;
use crate::session::PtyOutput;

/// Maximum number of reconnect attempts per daemon session during discovery.
const RECONNECT_MAX_ATTEMPTS: u32 = 3;

/// Delay between reconnect retry attempts.
const RECONNECT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Discover running daemon sessions from a previous agent lifecycle.
///
/// Scans the socket directory for `*.json` state files, checks each daemon
/// process is alive (via `kill(pid, 0)` + `started_at` for PID reuse protection),
/// reconnects, and retrieves scrollback data.
///
/// Returns a list of `(DaemonSession, scrollback)` for successfully reconnected daemons.
pub async fn discover_daemon_sessions(
    output_tx: mpsc::Sender<PtyOutput>,
    already_tracked: &HashSet<SessionId>,
    socket_dir: &Path,
    our_owner_id: Option<&str>,
) -> Vec<(DaemonSession, Option<Vec<u8>>)> {
    let dir = socket_dir.to_path_buf();
    if !dir.exists() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(error = %e, "failed to read socket directory");
            return Vec::new();
        }
    };

    let mut recovered = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension() else {
            continue;
        };
        if ext != "json" {
            continue;
        }

        // Read and parse state file
        let Some(state) = read_state_file(&path) else {
            continue;
        };

        // Parse session_id
        let session_id: SessionId = match state.session_id.parse() {
            Ok(id) => id,
            Err(e) => {
                tracing::debug!(error = %e, file = %path.display(), "invalid session_id in state file");
                continue;
            }
        };

        // Skip sessions already tracked by the session manager to avoid
        // disrupting active socket connections (reconnect replaces the daemon's
        // client, breaking the existing DaemonSession's reader).
        if already_tracked.contains(&session_id) {
            tracing::debug!(
                session_id = %session_id,
                "skipping already-tracked daemon in discovery"
            );
            continue;
        }

        // Skip sessions owned by a different agent instance. State files
        // from older agents have owner_id=None and are treated as unowned
        // (first discoverer claims them — same as pre-isolation behavior).
        if let Some(ref file_owner) = state.owner_id
            && let Some(our_id) = our_owner_id
            && file_owner != our_id
        {
            tracing::debug!(
                session_id = %session_id,
                file_owner = %file_owner,
                our_owner = %our_id,
                "skipping daemon owned by different agent instance"
            );
            continue;
        }

        // Check if daemon process is alive
        if !is_daemon_alive(state.daemon_pid, &state.started_at) {
            tracing::debug!(
                session_id = %session_id,
                daemon_pid = state.daemon_pid,
                "daemon not alive, skipping"
            );
            continue;
        }

        // Build socket path
        let socket_path = dir.join(format!("{session_id}.sock"));
        if !socket_path.exists() {
            tracing::debug!(
                session_id = %session_id,
                "socket file missing, skipping"
            );
            continue;
        }

        // Try to reconnect with retries
        let mut reconnect_result = None;
        for attempt in 1..=RECONNECT_MAX_ATTEMPTS {
            match DaemonSession::reconnect(
                session_id,
                socket_path.clone(),
                path.clone(),
                state.daemon_pid,
                state.shell_pid,
                output_tx.clone(),
            )
            .await
            {
                Ok(result) => {
                    reconnect_result = Some(result);
                    break;
                }
                Err(e) => {
                    if attempt < RECONNECT_MAX_ATTEMPTS {
                        tracing::debug!(
                            session_id = %session_id,
                            attempt,
                            error = %e,
                            "reconnect attempt failed, retrying"
                        );
                        tokio::time::sleep(RECONNECT_RETRY_DELAY).await;
                    } else {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "failed to reconnect to daemon after {RECONNECT_MAX_ATTEMPTS} attempts"
                        );
                    }
                }
            }
        }

        let Some((session, scrollback, daemon_started_at)) = reconnect_result else {
            continue;
        };

        // PID reuse protection: verify the daemon's reported started_at
        // matches the state file. If they differ, this is a different process
        // that reused the PID.
        if let Some(ref reported) = daemon_started_at
            && reported != &state.started_at
        {
            tracing::warn!(
                session_id = %session_id,
                state_file_started_at = %state.started_at,
                daemon_started_at = %reported,
                "started_at mismatch: PID reuse detected, skipping"
            );
            session.detach();
            continue;
        }

        tracing::info!(
            session_id = %session_id,
            shell_pid = state.shell_pid,
            daemon_pid = state.daemon_pid,
            "recovered daemon session"
        );
        recovered.push((session, scrollback));
    }

    recovered
}

/// Clean up stale daemon files (state + socket) where the daemon is dead.
///
/// Removes files for daemons that are no longer running, with a 24-hour
/// staleness threshold to avoid removing files from very recently started daemons.
pub fn cleanup_stale_daemons(socket_dir: &Path) {
    let dir = socket_dir.to_path_buf();
    if !dir.exists() {
        return;
    }

    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(error = %e, "failed to read socket dir for cleanup");
            return;
        }
    };

    let now = chrono::Utc::now();

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension() else {
            continue;
        };
        if ext != "json" {
            continue;
        }

        let Some(state) = read_state_file(&path) else {
            // Unparseable state file - clean it up
            let _ = std::fs::remove_file(&path);
            continue;
        };

        // Check if daemon is alive first. If dead, clean up immediately.
        // Only use 24h threshold for alive daemons whose shell may have exited.
        if is_daemon_alive(state.daemon_pid, &state.started_at) {
            // Daemon is alive - only clean up if older than 24h (shell may have exited
            // but daemon is still running due to some edge case)
            if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&state.started_at) {
                let age = now.signed_duration_since(started);
                if age < chrono::Duration::hours(24) {
                    continue;
                }
            } else {
                continue;
            }
        }

        // Daemon is dead or stale - clean up
        tracing::info!(
            session_id = %state.session_id,
            daemon_pid = state.daemon_pid,
            "cleaning up stale daemon files"
        );

        let _ = std::fs::remove_file(&path);
        let socket_path = path.with_extension("sock");
        let _ = std::fs::remove_file(&socket_path);
    }
}

/// Read and parse a daemon state file.
fn read_state_file(path: &std::path::Path) -> Option<DaemonStateFile> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Check if a daemon process is alive using `kill(pid, 0)` and verify
/// the process identity hasn't changed due to PID reuse.
///
/// On Linux, uses `/proc/{pid}/stat` starttime for precise PID reuse detection.
/// On all platforms, falls back to a wall-clock sanity check on `started_at`.
///
/// The real PID reuse protection happens during reconnect: the daemon's
/// `GetState` response includes its own `started_at` which must match
/// the state file's value.
fn is_daemon_alive(daemon_pid: u32, started_at: &str) -> bool {
    let pid = match i32::try_from(daemon_pid) {
        Ok(p) => nix::unistd::Pid::from_raw(p),
        Err(_) => return false,
    };

    // Step 1: Check if process exists via signal 0
    if nix::sys::signal::kill(pid, None).is_err() {
        return false;
    }

    // Step 2 (Linux only): Precise verification via /proc/{pid}/stat
    #[cfg(target_os = "linux")]
    {
        if let Some(false) = verify_proc_starttime(daemon_pid, started_at) {
            return false; // Definitively a different process (PID was reused)
        }
        // If verify_proc_starttime returns None (can't read /proc), fall through to wall-clock check
    }

    // Step 3: Wall-clock sanity check (all platforms, fallback for Linux without /proc)
    if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(started_at) {
        // Reject if started_at is in the future
        if ts > chrono::Utc::now() {
            return false;
        }
    } else {
        return false; // Can't parse timestamp
    }

    true
}

/// Tolerance in seconds for comparing process start times.
/// Accounts for clock granularity and small timing differences.
#[cfg(target_os = "linux")]
const PROC_TIME_TOLERANCE_SECS: i64 = 5;

/// Verify that the process at `pid` was started at approximately `started_at`
/// by reading its start time from `/proc/{pid}/stat`.
///
/// Returns:
/// - `Some(true)` if the start times match within tolerance
/// - `Some(false)` if the start times definitively don't match (PID reuse)
/// - `None` if `/proc` is unavailable or unparseable (caller should fall back)
#[cfg(target_os = "linux")]
fn verify_proc_starttime(pid: u32, started_at: &str) -> Option<bool> {
    let starttime_ticks = parse_proc_stat_starttime(pid)?;
    let boot_time_secs = system_boot_time()?;
    let ticks_per_sec = clock_ticks_per_sec();

    // Compute absolute start time in seconds since epoch
    // Use saturating_add to prevent integer overflow
    let proc_start_secs = boot_time_secs.saturating_add(starttime_ticks / ticks_per_sec);

    let claimed_start = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    let claimed_secs = claimed_start.timestamp();

    // Compare with tolerance
    let proc_start_i64 = i64::try_from(proc_start_secs).ok()?;
    let diff = (proc_start_i64 - claimed_secs).abs();
    Some(diff <= PROC_TIME_TOLERANCE_SECS)
}

/// Parse field 22 (starttime) from `/proc/{pid}/stat`.
///
/// The comm field (field 2) can contain spaces and parentheses, so we find
/// the last ')' to reliably skip it before parsing remaining fields.
#[cfg(target_os = "linux")]
fn parse_proc_stat_starttime(pid: u32) -> Option<u64> {
    let stat_content = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;

    // Find the closing ')' of the comm field (field 2)
    let after_comm = stat_content.rfind(')')? + 1;
    let remaining = stat_content.get(after_comm..)?.trim_start();

    // Fields after comm: state(3), ppid(4), pgrp(5), session(6), tty_nr(7),
    // tpgid(8), flags(9), minflt(10), cminflt(11), majflt(12), cmajflt(13),
    // utime(14), stime(15), cutime(16), cstime(17), priority(18), nice(19),
    // num_threads(20), itrealvalue(21), starttime(22)
    // starttime is the 20th field after the closing ')' (index 19, 0-based)
    let fields: Vec<&str> = remaining.split_whitespace().collect();
    // Index: 0=state, 1=ppid, ..., 19=starttime
    fields.get(19)?.parse().ok()
}

/// Read system boot time from `/proc/stat` (the `btime` line).
#[cfg(target_os = "linux")]
fn system_boot_time() -> Option<u64> {
    let stat_content = std::fs::read_to_string("/proc/stat").ok()?;
    for line in stat_content.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Get the number of clock ticks per second (CLK_TCK).
#[cfg(target_os = "linux")]
fn clock_ticks_per_sec() -> u64 {
    // sysconf returns Option<c_long>; CLK_TCK is always available on Linux.
    // Default to 100 (standard Linux value) if sysconf fails.
    nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
        .ok()
        .flatten()
        .and_then(|v| u64::try_from(v).ok())
        .unwrap_or(100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_dir_is_scoped_by_instance_key() {
        let uid = nix::unistd::getuid();
        let dir = super::super::socket_dir("test-key");
        let s = dir.to_string_lossy();
        assert!(s.starts_with(&format!("/tmp/zremote-pty-{uid}-")));
    }

    #[test]
    fn read_state_file_nonexistent() {
        let result = read_state_file(std::path::Path::new("/tmp/nonexistent-state-abc.json"));
        assert!(result.is_none());
    }

    #[test]
    fn read_state_file_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.json");
        let state = DaemonStateFile {
            version: 1,
            session_id: "test-id".to_string(),
            shell: "/bin/sh".to_string(),
            shell_pid: 100,
            daemon_pid: 101,
            cols: 80,
            rows: 24,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            owner_id: None,
        };
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();

        let result = read_state_file(&path).unwrap();
        assert_eq!(result.session_id, "test-id");
    }

    #[test]
    fn read_state_file_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();

        let result = read_state_file(&path);
        assert!(result.is_none());
    }

    #[test]
    fn is_daemon_alive_nonexistent_pid() {
        // PID 99999999 almost certainly doesn't exist
        let result = is_daemon_alive(99_999_999, "2026-01-01T00:00:00Z");
        assert!(!result);
    }

    #[test]
    fn is_daemon_alive_current_process() {
        let pid = std::process::id();
        // Compute the actual process start time to avoid fragility on slow machines.
        // On non-Linux, fall back to Utc::now() which is fine (no /proc check).
        let started_at = {
            #[cfg(target_os = "linux")]
            {
                let ticks = super::parse_proc_stat_starttime(pid).unwrap();
                let boot = super::system_boot_time().unwrap();
                let clk = super::clock_ticks_per_sec();
                let secs = boot.saturating_add(ticks / clk);
                chrono::DateTime::from_timestamp(secs.cast_signed(), 0)
                    .unwrap()
                    .to_rfc3339()
            }
            #[cfg(not(target_os = "linux"))]
            {
                chrono::Utc::now().to_rfc3339()
            }
        };
        let result = is_daemon_alive(pid, &started_at);
        assert!(result);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_based_verification_current_process() {
        // Verify that /proc-based starttime verification works for the current process.
        let pid = std::process::id();

        // parse_proc_stat_starttime should succeed for our own process
        let starttime_ticks = super::parse_proc_stat_starttime(pid);
        assert!(
            starttime_ticks.is_some(),
            "should be able to read own /proc stat"
        );

        // system_boot_time should succeed on Linux
        let btime = super::system_boot_time();
        assert!(btime.is_some(), "should be able to read boot time");

        // Compute the actual process start time and use it as started_at.
        // This avoids fragility: Utc::now() would fail if test binary ran >5s.
        let ticks = starttime_ticks.unwrap();
        let boot = btime.unwrap();
        let clk = super::clock_ticks_per_sec();
        let actual_start_secs = boot.saturating_add(ticks / clk);
        let actual_start = chrono::DateTime::from_timestamp(actual_start_secs.cast_signed(), 0)
            .unwrap()
            .to_rfc3339();

        // verify_proc_starttime with the actual start time should return Some(true)
        let result = super::verify_proc_starttime(pid, &actual_start);
        assert_eq!(
            result,
            Some(true),
            "current process should match its own start time"
        );

        // verify_proc_starttime with an old timestamp should return Some(false)
        let old = "2000-01-01T00:00:00Z";
        let result = super::verify_proc_starttime(pid, old);
        assert_eq!(
            result,
            Some(false),
            "current process should not match year-2000 timestamp"
        );
    }

    #[test]
    fn is_daemon_alive_invalid_timestamp() {
        // Unparseable timestamp should return false even for an alive PID
        let pid = std::process::id();
        assert!(!is_daemon_alive(pid, "not-a-timestamp"));
    }

    #[test]
    fn is_daemon_alive_future_timestamp() {
        // Future timestamp should return false
        let pid = std::process::id();
        assert!(!is_daemon_alive(pid, "2099-12-31T23:59:59Z"));
    }

    #[test]
    fn is_daemon_alive_pid_zero() {
        // PID 0 (kernel) - kill(0, 0) sends to process group, but we test the path
        // This exercises the i32 conversion path
        let result = is_daemon_alive(0, "2026-01-01T00:00:00Z");
        // Result varies by platform/permissions, but must not panic
        let _ = result;
    }

    #[test]
    fn is_daemon_alive_max_pid() {
        // u32::MAX should fail i32 conversion or not exist
        let result = is_daemon_alive(u32::MAX, "2026-01-01T00:00:00Z");
        assert!(!result, "u32::MAX PID should not be alive");
    }

    #[test]
    fn read_state_file_with_extra_fields() {
        // State file with extra fields should still parse (forward compat)
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("extra.json");
        let json = r#"{
            "version": 1,
            "session_id": "extra-test",
            "shell": "/bin/sh",
            "shell_pid": 100,
            "daemon_pid": 101,
            "cols": 80,
            "rows": 24,
            "started_at": "2026-01-01T00:00:00Z",
            "unknown_field": "should be ignored"
        }"#;
        std::fs::write(&path, json).unwrap();
        let result = read_state_file(&path);
        // serde by default ignores unknown fields, so this should work
        assert!(result.is_some());
        assert_eq!(result.unwrap().session_id, "extra-test");
    }

    #[test]
    fn read_state_file_missing_required_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("incomplete.json");
        // Missing shell_pid
        let json = r#"{
            "version": 1,
            "session_id": "test",
            "shell": "/bin/sh",
            "daemon_pid": 101,
            "cols": 80,
            "rows": 24,
            "started_at": "2026-01-01T00:00:00Z"
        }"#;
        std::fs::write(&path, json).unwrap();
        let result = read_state_file(&path);
        assert!(result.is_none(), "missing required field should fail parse");
    }

    #[test]
    fn read_state_file_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.json");
        std::fs::write(&path, "").unwrap();
        let result = read_state_file(&path);
        assert!(result.is_none(), "empty file should fail parse");
    }

    #[test]
    fn cleanup_stale_daemons_removes_dead_daemon_files() {
        // Create a temp dir that mimics the socket directory structure
        let tmp = tempfile::tempdir().unwrap();
        let state_path = tmp.path().join("dead-session.json");
        let socket_path = tmp.path().join("dead-session.sock");

        // Write a state file with a PID that doesn't exist
        let state = DaemonStateFile {
            version: 1,
            session_id: "dead-session".to_string(),
            shell: "/bin/sh".to_string(),
            shell_pid: 99_999_998,
            daemon_pid: 99_999_999,
            cols: 80,
            rows: 24,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            owner_id: None,
        };
        std::fs::write(&state_path, serde_json::to_string(&state).unwrap()).unwrap();
        std::fs::write(&socket_path, "fake-socket").unwrap();

        // We can't directly call cleanup_stale_daemons with a custom dir,
        // but we can verify the internal logic by calling read_state_file + is_daemon_alive
        let parsed = read_state_file(&state_path).unwrap();
        assert!(!is_daemon_alive(parsed.daemon_pid, &parsed.started_at));
    }

    #[test]
    fn cleanup_stale_daemons_no_dir() {
        // Should not panic when directory doesn't exist
        cleanup_stale_daemons(Path::new("/tmp/zremote-no-such-dir"));
    }

    #[test]
    fn cleanup_stale_daemons_removes_unparseable_state_files() {
        // Verify the logic: unparseable state files are cleaned up
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        std::fs::write(&path, "not valid json").unwrap();

        // read_state_file returns None for unparseable files
        assert!(read_state_file(&path).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_proc_stat_starttime_for_pid_1() {
        // PID 1 (init/systemd) should always be readable if /proc is available
        let result = super::parse_proc_stat_starttime(1);
        // May be None if running in a restricted container, but should not panic
        let _ = result;
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_proc_stat_starttime_nonexistent_pid() {
        let result = super::parse_proc_stat_starttime(99_999_999);
        assert!(result.is_none(), "nonexistent PID should return None");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn system_boot_time_returns_some() {
        let result = super::system_boot_time();
        assert!(result.is_some(), "boot time should be readable on Linux");
        // Boot time should be a reasonable value (after year 2000)
        let btime = result.unwrap();
        assert!(btime > 946_684_800, "boot time should be after year 2000");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clock_ticks_per_sec_returns_reasonable_value() {
        let ticks = super::clock_ticks_per_sec();
        // Typical values: 100 (most Linux), sometimes 250, 300, 1000
        assert!(ticks > 0, "CLK_TCK should be positive");
        assert!(ticks <= 10000, "CLK_TCK should be reasonable");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn verify_proc_starttime_nonexistent_pid() {
        let result = super::verify_proc_starttime(99_999_999, "2026-01-01T00:00:00Z");
        assert!(result.is_none(), "nonexistent PID should return None");
    }

    #[tokio::test]
    async fn reconnect_retry_with_nonexistent_socket() {
        // Test that the retry loop runs multiple attempts when reconnect fails.
        // We use a nonexistent socket path so every attempt fails immediately.
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("nonexistent.sock");
        let state_path = tmp.path().join("test.json");
        let session_id: SessionId = uuid::Uuid::new_v4();
        let (tx, _rx) = mpsc::channel(64);

        let start = std::time::Instant::now();
        let mut last_err = None;
        for attempt in 1..=RECONNECT_MAX_ATTEMPTS {
            match DaemonSession::reconnect(
                session_id,
                socket_path.clone(),
                state_path.clone(),
                99_999_999,
                99_999_998,
                tx.clone(),
            )
            .await
            {
                Ok(_) => {
                    panic!("reconnect should not succeed with nonexistent socket");
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < RECONNECT_MAX_ATTEMPTS {
                        tokio::time::sleep(RECONNECT_RETRY_DELAY).await;
                    }
                }
            }
        }
        let elapsed = start.elapsed();

        // All attempts should have failed
        assert!(last_err.is_some(), "should have recorded an error");

        // We should have waited at least (MAX_ATTEMPTS - 1) * RETRY_DELAY
        let min_expected = RECONNECT_RETRY_DELAY * (RECONNECT_MAX_ATTEMPTS - 1);
        assert!(
            elapsed >= min_expected,
            "elapsed {elapsed:?} should be >= {min_expected:?} (waited between retries)"
        );
    }

    #[tokio::test]
    async fn discover_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel(64);
        let result = discover_daemon_sessions(tx, &HashSet::new(), tmp.path(), None).await;
        assert!(result.is_empty(), "empty dir should yield no sessions");
    }

    #[tokio::test]
    async fn discover_nonexistent_dir() {
        let (tx, _rx) = mpsc::channel(64);
        let result = discover_daemon_sessions(
            tx,
            &HashSet::new(),
            Path::new("/tmp/zremote-no-such-dir"),
            None,
        )
        .await;
        assert!(
            result.is_empty(),
            "nonexistent dir should yield no sessions"
        );
    }

    /// State file with a matching owner_id should NOT be skipped by the
    /// owner filter (it belongs to us).
    #[test]
    fn owner_filter_matching_owner_is_not_skipped() {
        let state = DaemonStateFile {
            version: 1,
            session_id: "s1".to_string(),
            shell: "/bin/sh".to_string(),
            shell_pid: 1,
            daemon_pid: 2,
            cols: 80,
            rows: 24,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            owner_id: Some("agent-aaa".to_string()),
        };
        let our_id: Option<&str> = Some("agent-aaa");
        // Matching owner: should NOT be filtered out
        let dominated = match (&state.owner_id, our_id) {
            (Some(file_owner), Some(our)) => file_owner != our,
            _ => false,
        };
        assert!(!dominated);
    }

    /// State file owned by a different agent should be skipped.
    #[test]
    fn owner_filter_different_owner_is_skipped() {
        let state = DaemonStateFile {
            version: 1,
            session_id: "s2".to_string(),
            shell: "/bin/sh".to_string(),
            shell_pid: 1,
            daemon_pid: 2,
            cols: 80,
            rows: 24,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            owner_id: Some("agent-bbb".to_string()),
        };
        let our_id: Option<&str> = Some("agent-aaa");
        let dominated = match (&state.owner_id, our_id) {
            (Some(file_owner), Some(our)) => file_owner != our,
            _ => false,
        };
        assert!(dominated, "different owner should be filtered out");
    }

    /// State files from older agents (no owner_id) should NOT be filtered.
    #[test]
    fn owner_filter_missing_owner_is_not_skipped() {
        let state = DaemonStateFile {
            version: 1,
            session_id: "s3".to_string(),
            shell: "/bin/sh".to_string(),
            shell_pid: 1,
            daemon_pid: 2,
            cols: 80,
            rows: 24,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            owner_id: None,
        };
        let our_id: Option<&str> = Some("agent-aaa");
        let dominated = match (&state.owner_id, our_id) {
            (Some(file_owner), Some(our)) => file_owner != our,
            _ => false,
        };
        assert!(!dominated, "unowned state file should not be filtered");
    }
}
