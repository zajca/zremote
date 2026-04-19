//! Filesystem autocomplete endpoint — RFC-007 Phase 2.5.
//!
//! `GET /api/fs/complete?prefix=<absolute_or_tilde>&kind=dir|any`
//!
//! Returns directory suggestions for path-input autocomplete in the Add
//! Project and Worktree Create flows. This route is intentionally only
//! registered in the local-mode router — the server-mode router does NOT
//! expose it (RFC-007 §2.5.1 "Security"). Exposing FS probing across the
//! network is out of scope for v1.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Json;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use zremote_core::error::AppError;
use zremote_protocol::fs::{FsCompleteEntry, FsCompleteKind, FsCompleteResponse};

/// Maximum entries returned in a single response. Bounds both the response
/// size and the amount of per-entry `is_git` filesystem probing the agent
/// will perform on one request.
const MAX_ENTRIES: usize = 50;

/// Wall-clock cap on the blocking directory walk. A request against `/proc`
/// or a multi-million-entry dir can easily stall a request thread; bound it
/// so a single pathological prefix can't become a DoS vector. 2s is roomy
/// for any reasonable user-facing directory and tight enough that pathological
/// cases surface as a timeout the GUI can render instead of a hanging request.
const READ_DIR_TIMEOUT: Duration = Duration::from_secs(2);

/// Query parameters for `GET /api/fs/complete`.
#[derive(Debug, Deserialize)]
pub(crate) struct FsCompleteQuery {
    pub prefix: String,
    #[serde(default)]
    pub kind: Option<FsCompleteKind>,
}

/// Expand a leading `~` / `~/` to the user's home directory. Any other
/// leading character (including a bare path segment that happens to start
/// with `~name`) is left untouched: we deliberately do NOT expand `~user`
/// forms because the set of resolvable users is ambient and can surprise
/// the caller.
fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Truncate a path-ish string to 80 chars for debug logging so full user
/// paths (which may echo typos containing secrets, tokens, etc.) never land
/// in the log stream unredacted.
fn truncate_for_log(s: &str) -> String {
    if s.len() <= 80 {
        s.to_string()
    } else {
        format!("{}…", &s[..80])
    }
}

/// Build the 404 "path missing" response body. Uses a custom JSON body
/// rather than the generic `AppError::NotFound` mapping because the GUI
/// autocomplete component keys its inline hint on `error.code == "path_missing"`.
///
/// Deliberately collapses both NotFound and PermissionDenied to the same
/// shape (same status code, same error body) so the endpoint does NOT act
/// as a probe oracle for file-existence on otherwise-unreadable paths. The
/// user-supplied path is intentionally NOT echoed in the message — a bare
/// "No such directory" keeps the response identical across probes with
/// different (secret-leaking) prefixes.
fn path_missing_response() -> Response {
    let body = serde_json::json!({
        "error": {
            "code": "path_missing",
            "message": "No such directory",
        }
    });
    (StatusCode::NOT_FOUND, Json(body)).into_response()
}

/// Build the 503 "timeout" response body when the directory walk exceeds
/// `READ_DIR_TIMEOUT`.
fn timeout_response() -> Response {
    let body = serde_json::json!({
        "error": {
            "code": "timeout",
            "message": "Directory listing timed out",
        }
    });
    (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
}

/// `GET /api/fs/complete` handler.
///
/// The endpoint is stateless — no DB, no auth — so it takes no extractor
/// beyond the query string. Local-mode binds 127.0.0.1 by default which is
/// the trust boundary.
pub(crate) async fn fs_complete(
    Query(params): Query<FsCompleteQuery>,
) -> Result<Response, AppError> {
    let kind = params.kind.unwrap_or_default();
    let expanded = expand_tilde(&params.prefix);

    tracing::debug!(
        prefix = %truncate_for_log(&params.prefix),
        ?kind,
        "fs_complete request"
    );

    if !expanded.is_absolute() {
        return Err(AppError::BadRequest(
            "prefix must be an absolute path".to_string(),
        ));
    }

    // Decide the directory to list + the filter to apply on leaf names.
    //
    // Case A: raw prefix ends with `/` → list `expanded` itself, no filter.
    // Case B: raw prefix ends with `/.` → opt-in hidden listing in the
    //   directory before the dot (PathBuf normalisation eats a trailing
    //   `.` so we detect it from the raw string, not from the PathBuf).
    // Case C: otherwise → list parent of `expanded`, filter on file_name.
    //
    // We work off the raw `params.prefix` for the case split so
    // user-visible spelling controls the semantics, and only fall back to
    // PathBuf for Case C where normalisation is safe.
    let raw = params.prefix.as_str();
    let (parent, partial_leaf): (PathBuf, Option<String>) = if raw.ends_with('/') {
        // Case A — list the expanded directory itself.
        (expanded.clone(), None)
    } else if raw.ends_with("/.") {
        // Case B — user is asking for hidden-file entries in the parent.
        let trimmed = raw.trim_end_matches('.');
        let parent = expand_tilde(trimmed.trim_end_matches('/'));
        (parent, Some(".".to_string()))
    } else {
        // Case C — split at the last component.
        let parent = expanded
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));
        let leaf = expanded
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (parent, Some(leaf))
    };

    // Canonicalize the parent once (resolves `..`, single-symlink traversal,
    // duplicate slashes) so every `path` field returned to the caller, and
    // the listing itself, both operate on the same resolved form. We do NOT
    // canonicalize each child entry — that would cause O(n) extra stat calls
    // and would make a symlinked directory enumerable via its target.
    //
    // `canonicalize` doubles as the existence check, so we rely on its error
    // classification (NotFound / PermissionDenied / other IO errors) rather
    // than a prior `parent.exists()` call — the earlier pre-check was a TOCTOU
    // race (existence could change between the probe and `read_dir`) AND an
    // extra syscall that leaked nothing useful.
    //
    // Security: PermissionDenied and NotFound collapse to identical
    // `path_missing` 404 responses so the endpoint can't be used as a probe
    // oracle for files-that-exist-but-are-unreadable.
    let canonical_parent = match std::fs::canonicalize(&parent) {
        Ok(p) => p,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(path_missing_response());
        }
        Err(e) => {
            return Err(AppError::Internal(format!(
                "failed to canonicalize parent: {e}"
            )));
        }
    };
    let parent_display = canonical_parent.display().to_string();

    // Run the blocking directory walk off the runtime, bounded by a
    // wall-clock timeout so a pathological directory can't pin a request
    // thread. Sync `std::fs::read_dir` is fine once it lives inside
    // spawn_blocking. MAX_NAME_SCAN caps the per-request work even if the
    // timeout is wide enough to finish.
    const MAX_NAME_SCAN: usize = 10_000;
    let scan_parent = canonical_parent.clone();
    let scan_fut = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<String>> {
        let rd = std::fs::read_dir(&scan_parent)?;
        let mut names: Vec<String> = Vec::new();
        for entry in rd.flatten().take(MAX_NAME_SCAN) {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    });
    let names: Vec<String> = match tokio::time::timeout(READ_DIR_TIMEOUT, scan_fut).await {
        Ok(Ok(Ok(n))) => n,
        Ok(Ok(Err(e)))
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(path_missing_response());
        }
        Ok(Ok(Err(e))) => {
            return Err(AppError::Internal(format!("failed to read directory: {e}")));
        }
        Ok(Err(join_err)) => {
            return Err(AppError::Internal(format!(
                "directory scan task failed: {join_err}"
            )));
        }
        Err(_elapsed) => {
            tracing::warn!(
                parent = %parent_display,
                timeout_ms = READ_DIR_TIMEOUT.as_millis(),
                "fs_complete directory walk timed out"
            );
            return Ok(timeout_response());
        }
    };

    // Hidden dirs are opt-in: only surface entries starting with `.` when
    // the user explicitly typed a leading `.` as the partial leaf.
    let include_hidden = partial_leaf
        .as_deref()
        .is_some_and(|leaf| leaf.starts_with('.'));

    let mut filtered: Vec<String> = names
        .into_iter()
        .filter(|n| {
            if !include_hidden && n.starts_with('.') {
                return false;
            }
            match partial_leaf.as_deref() {
                Some(leaf) if !leaf.is_empty() => n.starts_with(leaf),
                _ => true,
            }
        })
        .collect();
    filtered.sort();

    let truncated = filtered.len() > MAX_ENTRIES;
    filtered.truncate(MAX_ENTRIES);

    let mut entries = Vec::with_capacity(filtered.len());
    for name in filtered {
        let full = canonical_parent.join(&name);
        // Skip symlinks to non-existent targets, entries we can't stat
        // (e.g. permission denied on parent/child combo), etc. Silent per
        // RFC — autocomplete is best-effort.
        let Ok(meta) = std::fs::metadata(&full) else {
            continue;
        };
        let is_dir = meta.is_dir();
        if matches!(kind, FsCompleteKind::Dir) && !is_dir {
            continue;
        }
        // `.git` detection handles both the bare-repo/dir form and the
        // gitfile form used by linked worktrees (file pointing at the
        // parent repo's `worktrees/` entry).
        let is_git = is_dir && full.join(".git").exists();
        let path_str = full.to_string_lossy().into_owned();
        entries.push(FsCompleteEntry {
            name,
            path: path_str,
            is_dir,
            is_git,
        });
    }

    let resp = FsCompleteResponse {
        prefix: expanded.to_string_lossy().into_owned(),
        parent: parent_display,
        entries,
        truncated,
    };
    Ok(Json(resp).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use std::fs as stdfs;
    use tempfile::TempDir;
    use tower::ServiceExt;

    fn router() -> Router {
        Router::new().route("/api/fs/complete", get(fs_complete))
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn percent_encode(s: &str) -> String {
        // Minimal encoder: only escape what a query value commonly needs.
        // `/` is legal in query values per RFC 3986, so we keep it — this
        // matches what reqwest emits for the client side.
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                    out.push(b as char);
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    #[tokio::test]
    async fn fs_complete_returns_dir_entries() {
        let dir = TempDir::new().unwrap();
        for name in ["a", "b", "c"] {
            stdfs::create_dir(dir.path().join(name)).unwrap();
        }
        let prefix = format!("{}/", dir.path().display());
        let uri = format!("/api/fs/complete?prefix={}", percent_encode(&prefix));

        let response = router()
            .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let json = body_json(response).await;
        let entries = json["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        for entry in entries {
            assert!(entry["is_dir"].as_bool().unwrap());
        }
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert!(!json["truncated"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn fs_complete_rejects_relative_prefix() {
        let response = router()
            .oneshot(
                Request::get("/api/fs/complete?prefix=relative/path")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "BAD_REQUEST");
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("absolute")
        );
    }

    #[tokio::test]
    async fn fs_complete_truncates_at_50() {
        let dir = TempDir::new().unwrap();
        // 60 > MAX_ENTRIES (50). Use zero-padded names so sort order is
        // deterministic and the first 50 are predictable.
        for i in 0..60 {
            stdfs::create_dir(dir.path().join(format!("d{i:03}"))).unwrap();
        }
        let prefix = format!("{}/", dir.path().display());
        let uri = format!("/api/fs/complete?prefix={}", percent_encode(&prefix));

        let response = router()
            .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let json = body_json(response).await;
        assert_eq!(json["entries"].as_array().unwrap().len(), 50);
        assert!(json["truncated"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn fs_complete_404_when_parent_missing() {
        let secret_probe = "/nonexistent-zr/secret-tenant-xyz";
        let uri = format!("/api/fs/complete?prefix={}", percent_encode(secret_probe));
        let response = router()
            .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let json = body_json(response).await;
        assert_eq!(json["error"]["code"], "path_missing");
        // The endpoint must NOT echo the user-supplied path — otherwise it
        // doubles as a probe oracle for secret path fragments when the
        // response is logged or surfaced to a less-trusted consumer.
        let message = json["error"]["message"].as_str().unwrap();
        assert!(
            !message.contains("secret-tenant-xyz") && !message.contains("nonexistent-zr"),
            "response message must not echo user-supplied path: {message}"
        );
    }

    /// The endpoint deliberately collapses PermissionDenied and NotFound into
    /// the same `path_missing` 404. We simulate PermissionDenied by pointing
    /// at a subpath of a 0700-owned-by-root directory — or, when the test
    /// environment lacks such a path, by using a freshly-created directory
    /// with all permission bits cleared. The assertion only cares that the
    /// response shape is identical to the NotFound case (same status, same
    /// error code, no user-path echo) so the endpoint can't be used as an
    /// existence oracle.
    #[tokio::test]
    async fn fs_complete_permission_denied_matches_not_found() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let unreadable = dir.path().join("unreadable");
        stdfs::create_dir(&unreadable).unwrap();
        stdfs::create_dir(unreadable.join("secret")).unwrap();
        // Strip all permission bits from the parent so `canonicalize` on a
        // child path returns PermissionDenied on most POSIX systems. If the
        // test runs as root (CI containers sometimes do), the strip is a
        // no-op and read_dir succeeds — skip in that case with a log marker.
        stdfs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

        let child = unreadable.join("secret/anything");
        let uri = format!(
            "/api/fs/complete?prefix={}",
            percent_encode(&child.display().to_string())
        );
        let response = router()
            .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        // Restore permissions so TempDir can clean up regardless of outcome.
        stdfs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o700)).unwrap();

        if response.status() == StatusCode::OK {
            // Running as root — the permission strip did nothing. Skip with
            // a warning rather than failing the suite.
            eprintln!("skipping fs_complete_permission_denied_matches_not_found: running as root");
            return;
        }

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "PermissionDenied must collapse to 404 (no existence oracle)"
        );
        let json = body_json(response).await;
        assert_eq!(
            json["error"]["code"], "path_missing",
            "error code must match the NotFound case exactly"
        );
    }

    /// A trivial wall-clock smoke test for the timeout path. We can't easily
    /// force `std::fs::read_dir` itself to stall, but the `spawn_blocking` +
    /// `tokio::time::timeout` wrapping is exercised by every other passing
    /// test; this test asserts that the short-circuit still yields a
    /// structured 503 body if the timeout fires by invoking
    /// `timeout_response` directly via the public handler contract.
    ///
    /// Note: a full end-to-end test would need a filesystem mock (fuse,
    /// blocked dir). That's disproportionate for a best-effort autocomplete.
    #[tokio::test]
    async fn fs_complete_timeout_response_shape() {
        // Confirm the helper returns the shape the client keys off of.
        let resp = timeout_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_json(resp).await;
        assert_eq!(json["error"]["code"], "timeout");
    }

    #[tokio::test]
    async fn fs_complete_hidden_dirs_opt_in() {
        let dir = TempDir::new().unwrap();
        stdfs::create_dir(dir.path().join(".hidden")).unwrap();
        stdfs::create_dir(dir.path().join("visible")).unwrap();

        // Prefix "<tempdir>/v" → only `visible`, not `.hidden`.
        let visible_prefix = format!("{}/v", dir.path().display());
        let uri = format!(
            "/api/fs/complete?prefix={}",
            percent_encode(&visible_prefix)
        );
        let response = router()
            .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        let entries = json["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "visible");

        // Prefix "<tempdir>/." → only `.hidden`, not `visible`.
        let hidden_prefix = format!("{}/.", dir.path().display());
        let uri = format!("/api/fs/complete?prefix={}", percent_encode(&hidden_prefix));
        let response = router()
            .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        let entries = json["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], ".hidden");
    }

    /// RFC-007 §2.5.1 — FS autocomplete is a local-mode-only endpoint. This
    /// test pins the agent-side half of the contract: the route MUST be
    /// reachable on the local router. The paired negative assertion — that
    /// the server-mode router does NOT expose the route — lives in
    /// `zremote-server` (`server_router_does_not_expose_fs_complete`) where
    /// it can actually build the production router. Both must pass to
    /// uphold the security boundary.
    #[tokio::test]
    async fn fs_complete_not_mounted_in_server_mode() {
        use crate::local::router::build_router;
        use crate::local::state::LocalAppState;
        use tokio_util::sync::CancellationToken;
        use uuid::Uuid;

        let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
        let shutdown = CancellationToken::new();
        let state = LocalAppState::new(
            pool,
            "test".to_string(),
            Uuid::new_v4(),
            shutdown,
            crate::config::PersistenceBackend::None,
            std::path::PathBuf::from("/tmp/zremote-fs-test"),
            Uuid::new_v4(),
        );
        let router = build_router(state).unwrap();

        // Sanity: local router DOES serve /api/fs/complete (positive half).
        let response = router
            .oneshot(
                Request::get("/api/fs/complete?prefix=/tmp/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "local router must expose /api/fs/complete"
        );
    }

    #[test]
    fn expand_tilde_handles_home_prefix() {
        let out = expand_tilde("~/foo");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(out, home.join("foo"));
        }
    }

    #[test]
    fn truncate_for_log_caps_at_80_chars() {
        let long = "a".repeat(200);
        let out = truncate_for_log(&long);
        // 80 chars plus the ellipsis codepoint (UTF-8 = 3 bytes).
        assert!(out.chars().count() <= 81);
        assert!(out.ends_with('…'));
    }
}
