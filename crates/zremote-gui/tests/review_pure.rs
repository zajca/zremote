//! Integration tests for pure review helpers.
//!
//! These tests live outside `src/` because gpui_macros expansion in
//! `views::diff::review_*` makes `cargo test --lib` crash `rustc` with
//! SIGSEGV regardless of `recursion_limit`. Moving them to integration
//! scope avoids pulling the full view tree into the test compilation
//! graph.

use zremote_client::{Session, SessionStatus};
use zremote_gui::test_exports::{
    body_is_empty, compact_preview, infer_side_from_kind, session_label,
};
use zremote_protocol::project::{DiffLineKind, ReviewSide};

fn mk_session(id: &str, name: Option<&str>, working_dir: Option<&str>) -> Session {
    Session {
        id: id.to_string(),
        host_id: "h".to_string(),
        name: name.map(str::to_string),
        shell: None,
        status: SessionStatus::Active,
        working_dir: working_dir.map(str::to_string),
        project_id: None,
        pid: None,
        exit_code: None,
        created_at: "2026-04-20T00:00:00Z".to_string(),
        closed_at: None,
    }
}

#[test]
fn body_is_empty_for_whitespace_and_empty() {
    assert!(body_is_empty(""));
    assert!(body_is_empty("   "));
    assert!(body_is_empty("\n\t  \n"));
}

#[test]
fn body_is_not_empty_for_any_printable_char() {
    assert!(!body_is_empty("a"));
    assert!(!body_is_empty("  nit  "));
    assert!(!body_is_empty("\n  fix this\n"));
}

#[test]
fn infer_side_returns_left_for_removed_lines() {
    assert_eq!(
        infer_side_from_kind(DiffLineKind::Removed),
        ReviewSide::Left
    );
}

#[test]
fn infer_side_returns_right_for_added_and_context() {
    assert_eq!(infer_side_from_kind(DiffLineKind::Added), ReviewSide::Right);
    assert_eq!(
        infer_side_from_kind(DiffLineKind::Context),
        ReviewSide::Right
    );
    assert_eq!(
        infer_side_from_kind(DiffLineKind::NoNewlineMarker),
        ReviewSide::Right
    );
}

#[test]
fn compact_preview_collapses_newlines_and_truncates() {
    let body = "first line\n\nsecond line with more content";
    assert_eq!(
        compact_preview(body),
        "first line second line with more content"
    );
}

#[test]
fn compact_preview_applies_max_length() {
    let body = "a".repeat(120);
    let p = compact_preview(&body);
    // 80 chars + U+2026 (ellipsis) = 81 chars
    assert_eq!(p.chars().count(), 81);
    assert!(p.ends_with('…'));
}

#[test]
fn session_label_includes_working_dir_when_present() {
    let s = mk_session(
        "abc12345-0000-0000-0000-000000000000",
        Some("task"),
        Some("/tmp/proj"),
    );
    assert_eq!(session_label(&s), "task — /tmp/proj");
}

#[test]
fn session_label_falls_back_to_id_prefix_when_name_missing() {
    let s = mk_session("abc12345-0000-0000-0000-000000000000", None, None);
    assert_eq!(session_label(&s), "session abc12345");
}
