//! Per-file, per-side highlight cache + keying for the diff view.
//!
//! Storage is a `HashMap<(key, syntax_name, SideKey), Arc<HashMap<u32,
//! LineSpans>>>` owned by `DiffView`. The outer tuple identifies the blob
//! whose highlighting was computed; the inner map is keyed by 1-based line
//! number so multi-hunk files keep correct spans for every hunk (not just
//! the first).
//!
//! The cache-key helper `side_cache_key_for_text` deliberately takes the
//! already-computed per-side text — storage and lookup paths both use it
//! with the same `side_lines` output, so the two paths cannot diverge.

use std::collections::HashMap;
use std::sync::Arc;

use zremote_protocol::project::{DiffFile, DiffLineKind};

use super::highlight::{HighlightEngine, LineSpans, SideKey};

/// Shared cache shape for `DiffView` and `DiffPane`. Exposed here so both
/// modules reference a single definition.
pub type HighlightCache = HashMap<(String, String, SideKey), Arc<HashMap<u32, LineSpans>>>;

/// Collect the lines on one side of a diff (pre- or post-image) together
/// with their 1-based line numbers. Highlighter fires one syntect pass over
/// the concatenated text but stores results keyed by the real file line
/// number, so multi-hunk files keep correct spans for every hunk (not just
/// the first).
///
/// "Relevant lines" means:
/// - `SideKey::Old`: `Context` + `Removed` lines (anything with an
///   `old_lineno`).
/// - `SideKey::New`: `Context` + `Added` lines (anything with a
///   `new_lineno`).
///
/// Each returned text ends in a newline so syntect's state machine closes
/// each line cleanly.
pub fn side_lines(file: &DiffFile, side: SideKey) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for hunk in &file.hunks {
        for line in &hunk.lines {
            let include = match side {
                SideKey::Old => {
                    matches!(line.kind, DiffLineKind::Context | DiffLineKind::Removed)
                }
                SideKey::New => {
                    matches!(line.kind, DiffLineKind::Context | DiffLineKind::Added)
                }
            };
            if !include {
                continue;
            }
            let lineno = match side {
                SideKey::Old => line.old_lineno,
                SideKey::New => line.new_lineno,
            };
            let Some(lineno) = lineno else {
                continue;
            };
            let mut text = line.content.clone();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            out.push((lineno, text));
        }
    }
    out
}

/// Run syntect over the concatenated side text and distribute per-line
/// spans into a `lineno → spans` map. The cache is indexed by real 1-based
/// file line number (never by position in the concatenated string) so
/// multi-hunk files produce correct lookups for every hunk.
pub fn highlight_by_lineno(
    engine: &HighlightEngine,
    syntax: &syntect::parsing::SyntaxReference,
    lines: &[(u32, String)],
) -> HashMap<u32, LineSpans> {
    let text: String = lines.iter().map(|(_, t)| t.as_str()).collect();
    let spans = engine.highlight_file(&text, syntax);
    let mut map = HashMap::with_capacity(lines.len());
    for ((lineno, _), line_spans) in lines.iter().zip(spans.into_iter()) {
        map.insert(*lineno, line_spans);
    }
    map
}

/// Canonical cache key for a file side, using the already-computed side
/// text to derive the content-fallback key. Taking the text as an argument
/// (rather than recomputing `side_lines` inside) lets storage and lookup
/// paths share this helper while avoiding a second O(hunks × lines) pass
/// on every lookup.
///
/// When the protocol ships a blob SHA (server-side diffs from committed
/// revisions) we use it verbatim. Working-tree diffs always have `new_sha
/// = None` (and often `old_sha = None` too), so we fall back to a key that
/// embeds the raw text: `content:{len}:{text}`. Using the text itself —
/// not a non-cryptographic hash — sidesteps `DefaultHasher`'s non-determinism
/// across process boots and eliminates hash-collision cache corruption.
/// The extra memory is a single `String` clone per cache insert, paid once
/// per (file, side) pair; negligible next to the hunk content we already
/// hold.
pub fn side_cache_key_for_text(
    file: &DiffFile,
    side: SideKey,
    syntax_name: &str,
    text: &str,
) -> (String, String, SideKey) {
    let sha = match side {
        SideKey::Old => file.summary.old_sha.as_ref(),
        SideKey::New => file.summary.new_sha.as_ref(),
    };
    let content_key = sha
        .cloned()
        .map(|s| format!("sha:{s}"))
        .unwrap_or_else(|| format!("content:{}:{}", text.len(), text));
    (content_key, syntax_name.to_string(), side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zremote_protocol::project::{
        DiffFile, DiffFileStatus, DiffFileSummary, DiffHunk, DiffLine, DiffLineKind,
    };

    fn mk_line(kind: DiffLineKind, old: Option<u32>, new: Option<u32>, content: &str) -> DiffLine {
        DiffLine {
            kind,
            old_lineno: old,
            new_lineno: new,
            content: content.to_string(),
        }
    }

    fn mk_file(
        path: &str,
        old_sha: Option<&str>,
        new_sha: Option<&str>,
        hunks: Vec<DiffHunk>,
    ) -> DiffFile {
        DiffFile {
            summary: DiffFileSummary {
                path: path.to_string(),
                old_path: None,
                status: DiffFileStatus::Modified,
                binary: false,
                submodule: false,
                too_large: false,
                additions: 0,
                deletions: 0,
                old_sha: old_sha.map(String::from),
                new_sha: new_sha.map(String::from),
                old_mode: None,
                new_mode: None,
            },
            hunks,
        }
    }

    fn text_for(file: &DiffFile, side: SideKey) -> String {
        side_lines(file, side).into_iter().map(|(_, t)| t).collect()
    }

    #[test]
    fn side_lines_preserves_real_linenos_across_multiple_hunks() {
        let hunks = vec![
            DiffHunk {
                old_start: 10,
                old_lines: 3,
                new_start: 10,
                new_lines: 3,
                header: "@@ -10,3 +10,3 @@".into(),
                lines: vec![
                    mk_line(DiffLineKind::Context, Some(10), Some(10), "fn a() {\n"),
                    mk_line(DiffLineKind::Context, Some(11), Some(11), "    body\n"),
                    mk_line(DiffLineKind::Context, Some(12), Some(12), "}\n"),
                ],
            },
            DiffHunk {
                old_start: 50,
                old_lines: 3,
                new_start: 50,
                new_lines: 3,
                header: "@@ -50,3 +50,3 @@".into(),
                lines: vec![
                    mk_line(DiffLineKind::Context, Some(50), Some(50), "fn b() {\n"),
                    mk_line(DiffLineKind::Context, Some(51), Some(51), "    body2\n"),
                    mk_line(DiffLineKind::Context, Some(52), Some(52), "}\n"),
                ],
            },
        ];
        let file = mk_file("x.rs", Some("old"), Some("new"), hunks);
        let lines = side_lines(&file, SideKey::New);
        let linenos: Vec<u32> = lines.iter().map(|(n, _)| *n).collect();
        assert_eq!(linenos, vec![10, 11, 12, 50, 51, 52]);
    }

    #[test]
    fn highlights_available_for_all_hunks_in_multi_hunk_file() {
        let hunks = vec![
            DiffHunk {
                old_start: 10,
                old_lines: 2,
                new_start: 10,
                new_lines: 2,
                header: "@@ -10,2 +10,2 @@".into(),
                lines: vec![
                    mk_line(DiffLineKind::Context, Some(10), Some(10), "let a = 1;\n"),
                    mk_line(DiffLineKind::Context, Some(11), Some(11), "let b = 2;\n"),
                ],
            },
            DiffHunk {
                old_start: 50,
                old_lines: 2,
                new_start: 50,
                new_lines: 2,
                header: "@@ -50,2 +50,2 @@".into(),
                lines: vec![
                    mk_line(DiffLineKind::Context, Some(50), Some(50), "let c = 3;\n"),
                    mk_line(DiffLineKind::Context, Some(51), Some(51), "let d = 4;\n"),
                ],
            },
        ];
        let file = mk_file("x.rs", Some("old"), Some("new"), hunks);
        let engine = HighlightEngine::global();
        let syntax = engine.detect_syntax("x.rs");
        let lines = side_lines(&file, SideKey::New);
        let map = highlight_by_lineno(engine, syntax, &lines);
        assert!(map.contains_key(&10), "hunk 1 missing lineno 10");
        assert!(map.contains_key(&11), "hunk 1 missing lineno 11");
        assert!(map.contains_key(&50), "hunk 2 missing lineno 50");
        assert!(map.contains_key(&51), "hunk 2 missing lineno 51");
    }

    #[test]
    fn side_cache_key_falls_back_to_content_when_sha_none() {
        // Regression guard for B2: working-tree diffs have `new_sha = None`
        // and the cache key must be content-derived AND deterministic across
        // process boots (no DefaultHasher) so storage and lookup paths agree.
        let hunk = DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: "@@ -1 +1 @@".into(),
            lines: vec![mk_line(DiffLineKind::Context, Some(1), Some(1), "hello\n")],
        };
        let file = mk_file("x.rs", None, None, vec![hunk]);
        let text = text_for(&file, SideKey::New);
        let key = side_cache_key_for_text(&file, SideKey::New, "Rust", &text);
        assert!(
            key.0.starts_with("content:"),
            "expected content fallback, got {}",
            key.0
        );
    }

    #[test]
    fn side_cache_key_uses_sha_when_available() {
        let file = mk_file("x.rs", Some("abc123"), Some("def456"), vec![]);
        let key_new = side_cache_key_for_text(&file, SideKey::New, "Rust", "");
        let key_old = side_cache_key_for_text(&file, SideKey::Old, "Rust", "");
        assert_eq!(key_new.0, "sha:def456");
        assert_eq!(key_old.0, "sha:abc123");
        assert_eq!(key_new.2, SideKey::New);
        assert_eq!(key_old.2, SideKey::Old);
    }

    #[test]
    fn cache_key_storage_and_lookup_paths_match() {
        // M4 regression guard: storage path and lookup path MUST derive the
        // same key. Simulate both: storage computes key once from side_lines
        // output, lookup re-computes from the same source. If either path
        // changes derivation, this test fires.
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
            header: "@@ -1,2 +1,2 @@".into(),
            lines: vec![
                mk_line(DiffLineKind::Context, Some(1), Some(1), "alpha\n"),
                mk_line(DiffLineKind::Context, Some(2), Some(2), "beta\n"),
            ],
        }];
        let file = mk_file("x.rs", None, None, hunks);

        // Storage path.
        let storage_text = text_for(&file, SideKey::New);
        let storage_key = side_cache_key_for_text(&file, SideKey::New, "Rust", &storage_text);

        // Lookup path — independent recomputation.
        let lookup_text = text_for(&file, SideKey::New);
        let lookup_key = side_cache_key_for_text(&file, SideKey::New, "Rust", &lookup_text);

        assert_eq!(storage_key, lookup_key);
    }

    #[test]
    fn content_fallback_key_is_deterministic_and_distinct() {
        // Without DefaultHasher (B2 fix) identical content yields identical
        // keys across calls, and distinct content yields distinct keys.
        let hunk_a = DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: "@@".into(),
            lines: vec![mk_line(DiffLineKind::Context, Some(1), Some(1), "foo\n")],
        };
        let hunk_b = DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: "@@".into(),
            lines: vec![mk_line(DiffLineKind::Context, Some(1), Some(1), "bar\n")],
        };
        let file_a = mk_file("x.rs", None, None, vec![hunk_a]);
        let file_b = mk_file("x.rs", None, None, vec![hunk_b]);
        let text_a = text_for(&file_a, SideKey::New);
        let text_b = text_for(&file_b, SideKey::New);
        let k_a1 = side_cache_key_for_text(&file_a, SideKey::New, "Rust", &text_a);
        let k_a2 = side_cache_key_for_text(&file_a, SideKey::New, "Rust", &text_a);
        let k_b = side_cache_key_for_text(&file_b, SideKey::New, "Rust", &text_b);
        assert_eq!(k_a1, k_a2);
        assert_ne!(k_a1, k_b);
    }

    #[test]
    fn side_lines_skips_lines_without_lineno() {
        let hunk = DiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
            header: "@@".into(),
            lines: vec![
                mk_line(DiffLineKind::Context, Some(1), Some(1), "ok\n"),
                mk_line(DiffLineKind::Context, None, None, "broken\n"),
            ],
        };
        let file = mk_file("x.rs", None, None, vec![hunk]);
        let lines = side_lines(&file, SideKey::Old);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, 1);
    }
}
