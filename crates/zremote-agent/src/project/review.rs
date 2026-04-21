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
    // Iterate by `char` so multi-byte UTF-8 scalars (e.g. U+0082 encoded as
    // `C2 82`) are decoded first and the C1 control range is matched against
    // the code point — not the raw continuation byte. Byte-level detection
    // would both miss genuine C1 escapes expressed via their UTF-8 encoding
    // AND drop harmless continuation bytes of non-control scalars.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{001b}' {
            // ESC — CSI starts with ESC '['; we also drop lone ESCs + OSC/SS3.
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    // CSI: drop params + final byte in 0x40..=0x7E.
                    for nc in chars.by_ref() {
                        let nb = nc as u32;
                        if (0x40..=0x7e).contains(&nb) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: terminated by BEL (0x07) or ST (ESC '\').
                    while let Some(nc) = chars.next() {
                        if nc == '\u{0007}' {
                            break;
                        }
                        if nc == '\u{001b}' {
                            if chars.peek().copied() == Some('\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    // ESC + single char (e.g. SS3). Drop the char.
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        // Keep the two whitespace bytes we want through.
        if c == '\n' || c == '\t' {
            out.push(c);
            continue;
        }
        // Drop C0 control chars (0..0x20 except LF/TAB), DEL (0x7f), and
        // C1 control chars (0x80..=0x9f). In UTF-8 terminals the C1 range
        // includes working single-byte aliases for ESC-prefixed sequences:
        // 0x9b == CSI, 0x9d == OSC, 0x8f == SS3, etc. Dropping the ESC-form
        // alone (above) would leave the C1-form available for injection.
        let cp = c as u32;
        if cp < 0x20 || cp == 0x7f || (0x80..=0x9f).contains(&cp) {
            continue;
        }
        out.push(c);
    }
    out
}

/// Render a markdown prompt from a `SendReviewRequest`. Terminating newline
/// is included (the PTY injector uses it as submission).
///
/// SECURITY: the rendered output is written verbatim into a PTY. Any new
/// `ReviewComment` field added to the output below MUST be passed through
/// `sanitize_body()` first — otherwise a CSI / OSC / C1 escape in an
/// attacker-controlled field can re-enter the PTY as an active terminal
/// escape (CWE-79 / CWE-116).
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

    /// CWE-116: C1 control characters (0x80..=0x9F) are single-byte
    /// aliases for ESC-prefixed sequences in UTF-8 terminals — 0x9b == CSI,
    /// 0x9d == OSC, 0x8f == SS3. Stripping only the 7-bit ESC form would
    /// leave these bypasses usable.
    #[test]
    fn sanitize_body_strips_c1_control_bytes() {
        // C1 CSI (U+009B), C1 OSC (U+009D), C1 SS3 (U+008F). All three must
        // be dropped by the sanitizer — printable tokens around them survive.
        let input = "foo\u{009b}31mbar\u{009d}0;evil\u{0007}\u{008f}baz";
        let cleaned = sanitize_body(input);
        for c in cleaned.chars() {
            let cp = c as u32;
            assert!(
                !(0x80..=0x9f).contains(&cp),
                "C1 control scalar leaked: U+{cp:04X} in {cleaned:?}"
            );
        }
        assert!(cleaned.contains("foo"));
        assert!(cleaned.contains("bar"));
        assert!(cleaned.contains("baz"));
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
