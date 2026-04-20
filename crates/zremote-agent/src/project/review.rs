//! Review prompt rendering + CSI sanitisation.
//!
//! The review drawer's "Send to agent" builds a `SendReviewRequest` and ships
//! it to this module. We render a markdown prompt grouped by file path
//! (§6.3 of RFC) and, crucially, strip CSI / control characters from every
//! comment body before the caller writes the result into a PTY (§4.7 CSI
//! injection guard).

use zremote_protocol::project::{DiffSource, ReviewComment, ReviewSide, SendReviewRequest};

/// Strip CSI escape sequences (starting with `\x1b[`) and other control
/// characters from `s`, preserving `\n` and `\t` for readability. This is
/// deliberately conservative: we drop anything that could re-enable a
/// terminal attribute, move the cursor, or clear the screen.
pub fn sanitize_body(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut bytes = s.as_bytes().iter().copied().peekable();
    while let Some(b) = bytes.next() {
        if b == 0x1b {
            // ESC — CSI starts with ESC '['; we also drop lone ESCs + OSC/SS3.
            // Consume a bracket if present, then consume until the sequence
            // terminator. For CSI the final byte is 0x40..=0x7E; for OSC it
            // ends with BEL (0x07) or ESC '\\'. For any other ESC-prefixed
            // sequence we drop the one following byte if any — good enough
            // defence in depth for comment bodies.
            match bytes.peek().copied() {
                Some(b'[') => {
                    bytes.next();
                    // CSI: drop params + final byte in 0x40..=0x7E.
                    for nb in bytes.by_ref() {
                        if (0x40..=0x7e).contains(&nb) {
                            break;
                        }
                    }
                }
                Some(b']') => {
                    bytes.next();
                    // OSC: terminated by BEL (0x07) or ST (ESC '\').
                    while let Some(nb) = bytes.next() {
                        if nb == 0x07 {
                            break;
                        }
                        if nb == 0x1b {
                            if let Some(b'\\') = bytes.peek().copied() {
                                bytes.next();
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    // ESC + single byte (e.g. SS3). Drop the byte.
                    bytes.next();
                }
                None => {}
            }
            continue;
        }
        // Keep printable and the two whitespace bytes we want through.
        if b == b'\n' || b == b'\t' {
            out.push(b as char);
            continue;
        }
        // Drop other ASCII control chars (0..0x20 except LF/TAB, and DEL).
        if b < 0x20 || b == 0x7f {
            continue;
        }
        out.push(b as char);
    }
    out
}

/// Render a markdown prompt from a `SendReviewRequest`. Terminating newline
/// is included (the PTY injector uses it as submission).
///
/// Format:
///
/// ```text
/// <preamble, if any>
///
/// ## Code review comments
///
/// Diff source: <source>
///
/// ### `<path>`
///
/// - L12 (new): this should use tracing::info! instead of println
/// - L42-48 (new): this block can be a single .map()
/// ```
pub fn render_review_prompt(req: &SendReviewRequest) -> String {
    let mut out = String::new();

    if let Some(preamble) = &req.preamble {
        let cleaned = sanitize_body(preamble);
        if !cleaned.trim().is_empty() {
            out.push_str(cleaned.trim_end());
            out.push_str("\n\n");
        }
    }

    out.push_str("## Code review comments\n\n");
    // Source string may include a user-supplied ref / SHA. Strip CSI / control
    // bytes before we embed it into the PTY payload (CWE-79 terminal injection).
    let source_clean = sanitize_body(&format_source(&req.source));
    out.push_str(&format!("Diff source: {source_clean}\n\n"));

    if req.comments.is_empty() {
        out.push_str("_(no comments)_\n");
        return out;
    }

    // Group by path, stable-sort by (path, line).
    let mut grouped: std::collections::BTreeMap<&str, Vec<&ReviewComment>> =
        std::collections::BTreeMap::new();
    for c in &req.comments {
        grouped.entry(c.path.as_str()).or_default().push(c);
    }
    for comments in grouped.values_mut() {
        comments.sort_by_key(|c| (c.start_line.unwrap_or(c.line), c.line));
    }

    for (path, comments) in grouped {
        // Comment paths are attacker-controlled (user types or pastes them in
        // the review drawer). Sanitise CSI / control bytes before they reach
        // the PTY (CWE-79 terminal injection).
        let path_clean = sanitize_body(path);
        out.push_str(&format!("### `{path_clean}`\n\n"));
        for c in comments {
            let body = sanitize_body(&c.body);
            let body_trim = body.trim();
            let line_label = format_line_label(c);
            if body_trim.is_empty() {
                out.push_str(&format!("- {line_label}: _(empty)_\n"));
            } else if body_trim.contains('\n') {
                // Multi-line body: indent continuation lines so they render
                // under the list item.
                out.push_str(&format!("- {line_label}:\n"));
                for bl in body_trim.split('\n') {
                    out.push_str("  ");
                    out.push_str(bl);
                    out.push('\n');
                }
            } else {
                out.push_str(&format!("- {line_label}: {body_trim}\n"));
            }
        }
        out.push('\n');
    }

    // Trim trailing blank line(s) then re-add a single terminating newline.
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn format_line_label(c: &ReviewComment) -> String {
    let side = match c.side {
        ReviewSide::Left => "old",
        ReviewSide::Right => "new",
    };
    match c.start_line {
        Some(start) if start != c.line => format!("L{start}-{} ({side})", c.line),
        _ => format!("L{} ({side})", c.line),
    }
}

fn format_source(source: &DiffSource) -> String {
    match source {
        DiffSource::WorkingTree => "working tree".to_string(),
        DiffSource::Staged => "staged".to_string(),
        DiffSource::WorkingTreeVsHead => "working tree vs HEAD".to_string(),
        DiffSource::HeadVs { reference } => format!("HEAD vs {reference}"),
        DiffSource::Range {
            from,
            to,
            symmetric: false,
        } => format!("{from}..{to}"),
        DiffSource::Range {
            from,
            to,
            symmetric: true,
        } => format!("{from}...{to}"),
        DiffSource::Commit { sha } => format!("commit {sha}"),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;
    use zremote_protocol::project::ReviewDelivery;

    use super::*;

    fn make_comment(path: &str, line: u32, body: &str) -> ReviewComment {
        ReviewComment {
            id: Uuid::new_v4(),
            path: path.to_string(),
            commit_id: "abc".to_string(),
            side: ReviewSide::Right,
            line,
            start_side: None,
            start_line: None,
            body: body.to_string(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn sanitize_body_strips_csi_sequences() {
        let input = "foo \x1b[31m BAD \x1b[0m end";
        let cleaned = sanitize_body(input);
        assert!(!cleaned.contains('\x1b'));
        assert!(!cleaned.contains("[31m"));
        assert!(!cleaned.contains("[0m"));
        assert!(cleaned.contains("foo"));
        assert!(cleaned.contains("end"));
    }

    #[test]
    fn sanitize_body_preserves_newlines_and_tabs() {
        let input = "line 1\nline 2\tcol";
        assert_eq!(sanitize_body(input), "line 1\nline 2\tcol");
    }

    #[test]
    fn sanitize_body_strips_other_control_bytes() {
        let input = "a\x07b\x08c\x7fd";
        let cleaned = sanitize_body(input);
        assert_eq!(cleaned, "abcd");
    }

    #[test]
    fn sanitize_body_strips_osc_sequences() {
        // OSC 0 (set title) terminated by BEL.
        let input = "hello\x1b]0;evil title\x07 world";
        let cleaned = sanitize_body(input);
        assert!(!cleaned.contains("evil title"));
        assert!(cleaned.contains("hello"));
        assert!(cleaned.contains("world"));
    }

    #[test]
    fn render_review_prompt_groups_by_file() {
        let req = SendReviewRequest {
            project_id: "p".to_string(),
            source: DiffSource::WorkingTree,
            delivery: ReviewDelivery::InjectSession,
            session_id: None,
            preamble: None,
            comments: vec![
                make_comment("a.rs", 10, "first on a"),
                make_comment("b.rs", 5, "first on b"),
                make_comment("a.rs", 20, "second on a"),
            ],
        };
        let out = render_review_prompt(&req);
        assert!(out.contains("### `a.rs`"));
        assert!(out.contains("### `b.rs`"));
        // a.rs should be grouped: both lines appear under the same heading.
        let a_idx = out.find("### `a.rs`").unwrap();
        let b_idx = out.find("### `b.rs`").unwrap();
        let first_a = out[a_idx..].find("first on a").unwrap() + a_idx;
        let second_a = out[a_idx..].find("second on a").unwrap() + a_idx;
        assert!(first_a < b_idx);
        assert!(second_a < b_idx);
    }

    #[test]
    fn render_review_prompt_formats_multi_line_range() {
        let mut c = make_comment("f.rs", 48, "block comment");
        c.start_line = Some(42);
        c.start_side = Some(ReviewSide::Right);
        let req = SendReviewRequest {
            project_id: "p".to_string(),
            source: DiffSource::Staged,
            delivery: ReviewDelivery::InjectSession,
            session_id: None,
            preamble: None,
            comments: vec![c],
        };
        let out = render_review_prompt(&req);
        assert!(out.contains("L42-48 (new)"), "output was: {out}");
    }

    #[test]
    fn render_review_prompt_strips_csi_from_body() {
        let c = make_comment("f.rs", 12, "foo \x1b[31m BAD");
        let req = SendReviewRequest {
            project_id: "p".to_string(),
            source: DiffSource::WorkingTree,
            delivery: ReviewDelivery::InjectSession,
            session_id: None,
            preamble: None,
            comments: vec![c],
        };
        let out = render_review_prompt(&req);
        assert!(
            !out.contains("\x1b[31m"),
            "CSI must be stripped; got: {out}"
        );
        assert!(!out.contains('\x1b'), "no raw ESC bytes");
    }

    #[test]
    fn render_review_prompt_includes_preamble() {
        let req = SendReviewRequest {
            project_id: "p".to_string(),
            source: DiffSource::WorkingTree,
            delivery: ReviewDelivery::InjectSession,
            session_id: None,
            preamble: Some("Please address:".to_string()),
            comments: vec![make_comment("a.rs", 1, "nit")],
        };
        let out = render_review_prompt(&req);
        assert!(out.starts_with("Please address:"));
    }

    #[test]
    fn render_review_prompt_empty_comments_still_renders() {
        let req = SendReviewRequest {
            project_id: "p".to_string(),
            source: DiffSource::WorkingTree,
            delivery: ReviewDelivery::InjectSession,
            session_id: None,
            preamble: None,
            comments: vec![],
        };
        let out = render_review_prompt(&req);
        assert!(out.contains("## Code review comments"));
        assert!(out.contains("Diff source: working tree"));
    }

    /// CWE-79: a comment with a CSI escape in its path must not reach the
    /// PTY payload. The path is attacker-controlled via the review drawer
    /// input; `sanitize_body` must be applied to it before we emit the
    /// `### \`<path>\`` heading.
    #[test]
    fn render_review_prompt_sanitizes_csi_in_path() {
        let c = make_comment("foo\x1b[31m.rs", 1, "safe body");
        let req = SendReviewRequest {
            project_id: "p".to_string(),
            source: DiffSource::WorkingTree,
            delivery: ReviewDelivery::InjectSession,
            session_id: None,
            preamble: None,
            comments: vec![c],
        };
        let out = render_review_prompt(&req);
        assert!(
            !out.contains('\x1b'),
            "rendered output must not contain ESC bytes from a path: {out}"
        );
        assert!(
            !out.contains("[31m"),
            "rendered output must not preserve CSI parameters: {out}"
        );
    }

    /// CWE-79: the `Diff source` line embeds a user-supplied ref / SHA.
    /// CSI in the ref name must be stripped before we write it to the PTY.
    #[test]
    fn render_review_prompt_sanitizes_csi_in_source() {
        let req = SendReviewRequest {
            project_id: "p".to_string(),
            // A ref string containing CSI — the validator would normally
            // reject this upstream, but defence in depth: the renderer must
            // sanitise unconditionally so it is safe even if a new code path
            // forgets the validator.
            source: DiffSource::HeadVs {
                reference: "main\x1b[31m".to_string(),
            },
            delivery: ReviewDelivery::InjectSession,
            session_id: None,
            preamble: None,
            comments: vec![make_comment("a.rs", 1, "ok")],
        };
        let out = render_review_prompt(&req);
        assert!(!out.contains('\x1b'));
        assert!(!out.contains("[31m"));
    }

    #[test]
    fn render_review_prompt_ends_with_newline() {
        let req = SendReviewRequest {
            project_id: "p".to_string(),
            source: DiffSource::WorkingTree,
            delivery: ReviewDelivery::InjectSession,
            session_id: None,
            preamble: None,
            comments: vec![make_comment("a.rs", 1, "nit")],
        };
        let out = render_review_prompt(&req);
        assert!(out.ends_with('\n'));
        assert!(!out.ends_with("\n\n\n"));
    }
}
