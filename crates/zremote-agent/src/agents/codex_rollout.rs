//! Filesystem fallback for resolving a Codex native session id.
//!
//! When Codex hooks are unavailable or untrusted (no live capture), ZRemote can
//! still recover the native session id by reading Codex's on-disk rollout files.
//! Each session writes
//! `<codex_home>/sessions/<YYYY>/<MM>/<DD>/rollout-<ISO8601>-<UUID>.jsonl`, whose
//! first JSONL line is a `session_meta` record carrying the session `id` and the
//! `cwd` it ran in (verified against `codex-cli` 0.135.0).
//!
//! Resolution strategy (RFC-012 Open Question #2): among rollouts whose
//! `session_meta.cwd` matches the requested working dir, pick the one with the
//! newest `session_meta.timestamp` (falling back to file mtime when the
//! timestamp is absent). This is a best-effort recovery path, not the primary
//! capture mechanism.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Parsed `session_meta` fields we care about (first line of a rollout file).
///
/// Codex wraps the meta in a small envelope; the fields we need (`id`, `cwd`,
/// `timestamp`) appear either at the top level or nested under a `payload`
/// object depending on the codex version, so we accept both shapes.
#[derive(Debug, Clone, Deserialize)]
struct SessionMeta {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
}

/// One resolved rollout candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutSession {
    /// Native codex session id from `session_meta.id`.
    pub session_id: String,
    /// Working directory the session ran in (`session_meta.cwd`).
    pub cwd: String,
    /// `session_meta.timestamp` if present (used for tie-breaking).
    pub timestamp: Option<String>,
    /// Absolute path to the rollout file.
    pub path: PathBuf,
}

/// Resolve the Codex config/home directory, honoring `$CODEX_HOME`.
///
/// When `CODEX_HOME` is set and non-empty it is used verbatim; otherwise
/// `<home>/.codex`.
#[must_use]
pub fn codex_home(home: &Path) -> PathBuf {
    codex_home_with_override(home, std::env::var("CODEX_HOME").ok().as_deref())
}

/// Pure core of [`codex_home`]: resolve against an explicit override instead of
/// reading the process environment (keeps the env-honoring logic unit-testable
/// without mutating global env, which the workspace's `unsafe_code = "deny"`
/// forbids).
#[must_use]
fn codex_home_with_override(home: &Path, override_dir: Option<&str>) -> PathBuf {
    match override_dir {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => home.join(".codex"),
    }
}

/// Find the newest Codex rollout whose `session_meta.cwd` equals
/// `working_dir`, searching under `<codex_home>/sessions`.
///
/// Returns `None` when the sessions directory is absent, no rollout matches the
/// `cwd`, or none carry a parseable `session_meta.id`. Among matches, the one
/// with the lexicographically greatest `timestamp` wins (ISO-8601 sorts
/// chronologically); candidates without a timestamp fall back to file mtime and
/// rank below any timestamped match.
#[must_use]
pub fn newest_rollout_for_cwd(codex_home: &Path, working_dir: &str) -> Option<RolloutSession> {
    let sessions_dir = codex_home.join("sessions");
    let mut best: Option<(RolloutSession, RankKey)> = None;

    for path in rollout_files(&sessions_dir) {
        let Some(meta) = read_session_meta(&path) else {
            continue;
        };
        let (Some(id), Some(cwd)) = (meta.id, meta.cwd) else {
            continue;
        };
        if cwd != working_dir {
            continue;
        }
        let rank = rank_key(meta.timestamp.as_deref(), &path);
        let candidate = RolloutSession {
            session_id: id,
            cwd,
            timestamp: meta.timestamp,
            path,
        };
        match &best {
            Some((_, best_rank)) if *best_rank >= rank => {}
            _ => best = Some((candidate, rank)),
        }
    }

    best.map(|(session, _)| session)
}

/// Ranking key for tie-breaking. A present ISO-8601 timestamp always outranks a
/// missing one; within each class, larger sorts newer (timestamp string, then
/// mtime nanos).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RankKey {
    /// No `session_meta.timestamp`; rank by file mtime (nanos since epoch).
    Mtime(u128),
    /// Has a timestamp; rank by the raw ISO-8601 string (chronological order).
    Timestamp(String),
}

fn rank_key(timestamp: Option<&str>, path: &Path) -> RankKey {
    match timestamp {
        Some(ts) if !ts.is_empty() => RankKey::Timestamp(ts.to_string()),
        _ => RankKey::Mtime(file_mtime_nanos(path)),
    }
}

fn file_mtime_nanos(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

/// Maximum bytes read from a rollout's first line. A `session_meta` record is
/// small; cap the read so a malformed/hostile rollout (no newline, or a
/// gigabyte first line) cannot OOM the resolver (CWE-400).
const MAX_META_LINE_BYTES: u64 = 1_048_576;

/// Read and parse the first line of a rollout file as a `session_meta` record.
///
/// Accepts both the flat shape (`{"id":..,"cwd":..}`) and the enveloped shape
/// (`{"type":"session_meta","payload":{"id":..,"cwd":..}}`). Returns `None` on
/// any I/O or parse failure (the caller skips the candidate). The read is
/// bounded to [`MAX_META_LINE_BYTES`] via a `Take` so a missing newline cannot
/// pull an unbounded amount into memory.
fn read_session_meta(path: &Path) -> Option<SessionMeta> {
    use std::io::{BufRead, BufReader, Read};

    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(MAX_META_LINE_BYTES));
    let mut first_line = String::new();
    // Read just the first line, capped by the `Take` above; rollouts can be large.
    if reader.read_line(&mut first_line).ok()? == 0 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(first_line.trim()).ok()?;

    // Prefer a nested `payload` object when present, else parse the top level.
    let target = value.get("payload").unwrap_or(&value);
    serde_json::from_value::<SessionMeta>(target.clone()).ok()
}

/// Enumerate `rollout-*.jsonl` files under `sessions_dir` (recursively through
/// the `<Y>/<M>/<D>` layout). Returns an empty vec if the directory is absent.
fn rollout_files(sessions_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_rollouts(sessions_dir, &mut out, 0);
    out
}

/// Bounded recursive walk. Codex nests rollouts three levels deep
/// (`<Y>/<M>/<D>`); cap depth at 4 to stay defensive against symlink loops or
/// unexpected layouts without an unbounded walk.
fn collect_rollouts(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    const MAX_DEPTH: usize = 4;
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_rollouts(&path, out, depth + 1);
        } else if file_type.is_file() && is_rollout_file(&path) {
            out.push(path);
        }
    }
}

/// `true` for files named `rollout-*.jsonl`.
fn is_rollout_file(path: &Path) -> bool {
    let has_jsonl_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"));
    let has_rollout_prefix = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("rollout-"));
    has_rollout_prefix && has_jsonl_ext
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Create `<root>/sessions/<y>/<m>/<d>/rollout-<ts>-<uuid>.jsonl` with a
    /// `session_meta` first line, returning the file path.
    fn write_rollout(
        root: &Path,
        date: (&str, &str, &str),
        file_stamp: &str,
        meta_line: &str,
    ) -> PathBuf {
        let dir = root.join("sessions").join(date.0).join(date.1).join(date.2);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-{file_stamp}.jsonl"));
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{meta_line}").unwrap();
        // A couple of non-meta lines to ensure we only read the first.
        writeln!(f, r#"{{"type":"message","role":"user"}}"#).unwrap();
        path
    }

    #[test]
    fn codex_home_honors_override() {
        // Override set + non-empty -> used verbatim.
        assert_eq!(
            codex_home_with_override(Path::new("/home/u"), Some("/custom/codex")),
            PathBuf::from("/custom/codex")
        );
        // Absent -> default under home.
        assert_eq!(
            codex_home_with_override(Path::new("/home/u"), None),
            PathBuf::from("/home/u/.codex")
        );
        // Empty/whitespace override -> falls back to default.
        assert_eq!(
            codex_home_with_override(Path::new("/home/u"), Some("   ")),
            PathBuf::from("/home/u/.codex")
        );
    }

    #[test]
    fn newest_rollout_picks_matching_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollout(
            tmp.path(),
            ("2026", "06", "04"),
            "2026-06-04T10-00-00-11111111-1111-1111-1111-111111111111",
            r#"{"id":"11111111-1111-1111-1111-111111111111","cwd":"/work/a","timestamp":"2026-06-04T10:00:00Z"}"#,
        );
        write_rollout(
            tmp.path(),
            ("2026", "06", "04"),
            "2026-06-04T11-00-00-22222222-2222-2222-2222-222222222222",
            r#"{"id":"22222222-2222-2222-2222-222222222222","cwd":"/work/b","timestamp":"2026-06-04T11:00:00Z"}"#,
        );

        let got = newest_rollout_for_cwd(tmp.path(), "/work/a").expect("match for /work/a");
        assert_eq!(got.session_id, "11111111-1111-1111-1111-111111111111");
        assert_eq!(got.cwd, "/work/a");
    }

    #[test]
    fn newest_rollout_tie_breaks_by_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        // Two rollouts in the SAME cwd; the later timestamp must win.
        write_rollout(
            tmp.path(),
            ("2026", "06", "04"),
            "older-aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            r#"{"id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","cwd":"/work/same","timestamp":"2026-06-04T09:00:00Z"}"#,
        );
        write_rollout(
            tmp.path(),
            ("2026", "06", "04"),
            "newer-bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            r#"{"id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","cwd":"/work/same","timestamp":"2026-06-04T12:30:00Z"}"#,
        );

        let got = newest_rollout_for_cwd(tmp.path(), "/work/same").expect("match");
        assert_eq!(
            got.session_id, "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            "newest timestamp must win the tie-break"
        );
    }

    #[test]
    fn newest_rollout_accepts_enveloped_session_meta() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollout(
            tmp.path(),
            ("2026", "06", "04"),
            "env-cccccccc-cccc-cccc-cccc-cccccccccccc",
            r#"{"type":"session_meta","payload":{"id":"cccccccc-cccc-cccc-cccc-cccccccccccc","cwd":"/work/env","timestamp":"2026-06-04T08:00:00Z"}}"#,
        );
        let got = newest_rollout_for_cwd(tmp.path(), "/work/env").expect("enveloped match");
        assert_eq!(got.session_id, "cccccccc-cccc-cccc-cccc-cccccccccccc");
    }

    #[test]
    fn newest_rollout_none_when_no_cwd_match() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollout(
            tmp.path(),
            ("2026", "06", "04"),
            "x-dddddddd-dddd-dddd-dddd-dddddddddddd",
            r#"{"id":"dddddddd-dddd-dddd-dddd-dddddddddddd","cwd":"/work/other","timestamp":"2026-06-04T08:00:00Z"}"#,
        );
        assert!(newest_rollout_for_cwd(tmp.path(), "/work/nope").is_none());
    }

    #[test]
    fn newest_rollout_none_when_sessions_dir_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(newest_rollout_for_cwd(tmp.path(), "/anything").is_none());
    }

    #[test]
    fn newest_rollout_skips_missing_id() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollout(
            tmp.path(),
            ("2026", "06", "04"),
            "noid-eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee",
            r#"{"cwd":"/work/noid","timestamp":"2026-06-04T08:00:00Z"}"#,
        );
        assert!(newest_rollout_for_cwd(tmp.path(), "/work/noid").is_none());
    }

    #[test]
    fn is_rollout_file_matches_expected_names() {
        assert!(is_rollout_file(Path::new(
            "/x/rollout-2026-06-04-uuid.jsonl"
        )));
        assert!(!is_rollout_file(Path::new("/x/notes.jsonl")));
        assert!(!is_rollout_file(Path::new("/x/rollout-2026.json")));
    }
}
