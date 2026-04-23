//! End-to-end review-send integration coverage (CODE-B1 in P5 fix list).
//!
//! Spawns a real `LocalAppState` + `SessionManager` backed by a genuine PTY
//! running `/bin/cat`, drives the axum `post_send_review` handler over a
//! `tower::ServiceExt::oneshot`, and drains the PTY output channel to
//! confirm two invariants:
//!
//! 1. The rendered prompt with two comments over two files reaches the PTY
//!    grouped by file, with the preamble preserved and a `Diff source:`
//!    anchor that cites the source.
//! 2. CSI / OSC / C1 escape sequences submitted in a comment body are
//!    stripped before the payload touches the PTY, while the surrounding
//!    plain text survives.
//!
//! The test uses `/bin/cat` as the shell so the PTY echoes whatever is
//! written to it verbatim — shells would interpret markdown list items
//! (`- L12 (new): …`) as commands and rewrite their output, making
//! content assertions brittle.
//!
//! NOTE: skipped automatically on platforms without `/bin/cat`. CI (Linux
//! + Darwin) both ship it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;
use zremote_protocol::project::{
    ReviewComment, ReviewDelivery, ReviewSide, SendReviewRequest, SendReviewResponse,
};

use zremote_agent::config::PersistenceBackend;
use zremote_agent::local::routes::projects::post_send_review;
use zremote_agent::local::state::LocalAppState;
use zremote_agent::local::upsert_local_host;

/// Locate `cat` on `$PATH`. nix-shell environments do not ship `/bin/cat`,
/// so we scan `PATH` and memoize the first hit.
fn cat_path() -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("cat");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Fallbacks for minimalist CI images that strip PATH.
    for hardcoded in ["/bin/cat", "/usr/bin/cat"] {
        let p = Path::new(hardcoded);
        if p.is_file() {
            return Some(p.to_path_buf());
        }
    }
    None
}

async fn build_state() -> Arc<LocalAppState> {
    let pool = zremote_core::db::init_db("sqlite::memory:").await.unwrap();
    let shutdown = CancellationToken::new();
    let host_id = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"review-integration-host");
    upsert_local_host(&pool, &host_id, "review-host")
        .await
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    LocalAppState::new(
        pool,
        "review-host".to_string(),
        host_id,
        shutdown,
        PersistenceBackend::None,
        tmp.path().to_path_buf(),
        Uuid::new_v4(),
    )
}

async fn seed_project(state: &Arc<LocalAppState>, path: &Path) -> String {
    let project_id = Uuid::new_v4().to_string();
    let host_id = state.host_id.to_string();
    sqlx::query(
        "INSERT INTO projects (id, host_id, path, name, project_type) VALUES (?, ?, ?, ?, 'repo')",
    )
    .bind(&project_id)
    .bind(&host_id)
    .bind(path.to_string_lossy().to_string())
    .bind("review-project")
    .execute(&state.db)
    .await
    .unwrap();
    project_id
}

async fn spawn_cat_session(state: &Arc<LocalAppState>, project_id: &str, cat: &Path) -> Uuid {
    let session_id = Uuid::new_v4();
    // Row in DB first so post_send_review's session-project check + active
    // status both pass.
    sqlx::query(
        "INSERT INTO sessions (id, host_id, name, status, working_dir, project_id) \
         VALUES (?, ?, ?, 'active', NULL, ?)",
    )
    .bind(session_id.to_string())
    .bind(state.host_id.to_string())
    .bind("review-test-session")
    .bind(project_id)
    .execute(&state.db)
    .await
    .unwrap();

    // Spawn the real PTY backing that row.
    {
        let mut mgr = state.session_manager.lock().await;
        mgr.create(
            session_id,
            cat.to_str().expect("cat path is valid UTF-8"),
            80,
            24,
            None,
            None,
            None,
        )
        .await
        .expect("spawn cat PTY");
    }
    session_id
}

fn router(state: Arc<LocalAppState>) -> Router {
    Router::new()
        .route(
            "/api/projects/{project_id}/review/send",
            post(post_send_review),
        )
        .with_state(state)
}

fn make_comment(path: &str, line: u32, body: &str) -> ReviewComment {
    ReviewComment {
        id: Uuid::new_v4(),
        path: path.to_string(),
        commit_id: "deadbeef".to_string(),
        side: ReviewSide::Right,
        line,
        start_side: None,
        start_line: None,
        body: body.to_string(),
        created_at: Utc::now(),
    }
}

/// Drain the PTY output channel until `timeout` elapses without a new chunk
/// (read-until-quiescence). `/bin/cat` echoes every input byte roughly
/// immediately, so 250 ms of silence is more than enough on a local test
/// machine.
async fn drain_pty_until_quiet(state: &Arc<LocalAppState>, timeout: Duration) -> String {
    let mut out = Vec::<u8>::new();
    let mut rx = state.pty_output_rx.lock().await;
    // Exits on either channel close or quiescence timeout — both surface as
    // a non-`Ok(Some(..))` branch from `timeout()`.
    while let Ok(Some(msg)) = tokio::time::timeout(timeout, rx.recv()).await {
        out.extend_from_slice(&msg.data);
    }
    // PTY output often mixes CR/LF; normalize for assertion readability.
    String::from_utf8_lossy(&out).replace('\r', "")
}

#[tokio::test]
async fn full_pty_injection_round_trip() {
    let Some(cat) = cat_path() else {
        eprintln!("skipping: cat not available on this platform");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let state = build_state().await;
    let project_id = seed_project(&state, tmp.path()).await;
    let session_id = spawn_cat_session(&state, &project_id, &cat).await;
    let app = router(state.clone());

    let req = SendReviewRequest {
        project_id: project_id.clone(),
        source: zremote_protocol::project::DiffSource::WorkingTree,
        delivery: ReviewDelivery::InjectSession,
        session_id: Some(session_id),
        preamble: Some("Please address these comments:".to_string()),
        comments: vec![
            make_comment("src/foo.rs", 12, "use tracing::info! here"),
            make_comment("src/bar.rs", 44, "this block can be a single .map()"),
        ],
    };
    let response = app
        .oneshot(
            Request::post(format!("/api/projects/{project_id}/review/send"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let parsed: SendReviewResponse = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(parsed.session_id, session_id);
    assert_eq!(parsed.delivered, 2);

    let out = drain_pty_until_quiet(&state, Duration::from_millis(400)).await;

    // Group headers for both files must both appear.
    assert!(
        out.contains("### `src/foo.rs`"),
        "foo.rs group header missing in PTY output:\n{out}"
    );
    assert!(
        out.contains("### `src/bar.rs`"),
        "bar.rs group header missing in PTY output:\n{out}"
    );
    // Both bodies survived sanitization.
    assert!(out.contains("use tracing::info! here"), "foo body missing");
    assert!(out.contains(".map()"), "bar body missing");
    // Preamble preserved.
    assert!(
        out.contains("Please address these comments:"),
        "preamble missing from PTY payload:\n{out}"
    );
    // Diff source anchor present.
    assert!(
        out.contains("Diff source: working tree"),
        "source anchor missing:\n{out}"
    );

    // Cleanup the PTY so the test doesn't leak a cat process.
    let mut mgr = state.session_manager.lock().await;
    mgr.close(&session_id);
}

#[tokio::test]
async fn csi_and_c1_escapes_are_stripped_end_to_end() {
    let Some(cat) = cat_path() else {
        eprintln!("skipping: cat not available on this platform");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let state = build_state().await;
    let project_id = seed_project(&state, tmp.path()).await;
    let session_id = spawn_cat_session(&state, &project_id, &cat).await;
    let app = router(state.clone());

    // Comment body combines a 7-bit CSI (ESC + '['), a C1 CSI (U+009B), and
    // a C1 OSC (U+009D). Surrounding plain text must reach the PTY; every
    // escape form must be dropped.
    let malicious = "foo \u{001b}[31m BAD \u{009b}31m alt \u{009d}0;evil\u{0007} end";
    let req = SendReviewRequest {
        project_id: project_id.clone(),
        source: zremote_protocol::project::DiffSource::WorkingTree,
        delivery: ReviewDelivery::InjectSession,
        session_id: Some(session_id),
        preamble: None,
        comments: vec![make_comment("a.rs", 1, malicious)],
    };
    let response = app
        .oneshot(
            Request::post(format!("/api/projects/{project_id}/review/send"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let out = drain_pty_until_quiet(&state, Duration::from_millis(400)).await;

    // No raw ESC bytes in what the PTY echoed.
    assert!(
        !out.contains('\u{001b}'),
        "raw ESC byte reached the PTY:\n{out:?}"
    );
    // No C1 control code points survived.
    for c in out.chars() {
        let cp = c as u32;
        assert!(
            !(0x80..=0x9f).contains(&cp),
            "C1 control U+{cp:04X} reached the PTY:\n{out:?}"
        );
    }
    // No CSI parameter fragments survived either.
    assert!(
        !out.contains("[31m"),
        "CSI fragment reached the PTY:\n{out}"
    );
    // Plain text segments made it through.
    assert!(out.contains("foo"));
    assert!(out.contains("BAD"));
    assert!(out.contains("end"));

    let mut mgr = state.session_manager.lock().await;
    mgr.close(&session_id);
}

/// Touch a path so clippy doesn't complain about an unused import when the
/// platform-skip branch hides the real use.
#[allow(dead_code)]
fn _touch_path(_: PathBuf) {}
