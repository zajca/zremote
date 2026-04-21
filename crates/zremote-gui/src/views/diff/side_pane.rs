//! Side-by-side row model + rendering for `DiffPane`.
//!
//! Extracted from `diff_pane.rs` to keep that module below the 800-line cap
//! demanded by review. Owns the alignment heuristic (context-on-both,
//! adjacent removed/added pair as a modification, unpaired remnants get a
//! blank opposite cell) and the per-half paint path.

use std::collections::VecDeque;

use gpui::*;

use zremote_protocol::project::{DiffHunk, DiffLine, DiffLineKind};

use super::diff_pane::{
    HighlightSlot, fmt_lineno, kind_colors, render_gutter, render_hunk_header,
    resolve_highlight_for_side, styled_line_content,
};
use super::highlight::SideKey;
use super::large_file::prepare_hunks;
use crate::theme;

/// Side-by-side row. A hunk header spans the full width; data rows carry
/// two independent slots so removals / additions / context can each land on
/// the correct side with the opposite side blank.
#[derive(Clone)]
pub(crate) enum SideRow {
    HunkHeader(String),
    Data {
        old: Option<DiffLine>,
        new: Option<DiffLine>,
    },
}

/// Build side-by-side rows from hunks.
///
/// Alignment rules:
/// - `Context` line: flush any pending removed/added buffers (pair them
///   index-by-index, unmatched entries emit with the opposite side blank),
///   then emit a `Data` row with the context on both sides.
/// - `Removed` line: push onto removed-buffer.
/// - `Added` line: push onto added-buffer.
/// - `NoNewlineMarker`: flush buffers, then emit as an old-only row (git
///   prints `\ No newline at end of file` attached to the side it applies
///   to — for simplicity we dock it on the old side; visually distinct via
///   its `fg = text_tertiary`).
/// - End of hunk: flush buffers.
///
/// This is the same heuristic GitHub / GitLab / Okena use for side-by-side
/// within a hunk.
pub(crate) fn build_side_by_side_rows(hunks: &[DiffHunk]) -> Vec<SideRow> {
    let mut out = Vec::new();
    let prepared = prepare_hunks(hunks);
    for hunk in prepared {
        out.push(SideRow::HunkHeader(hunk.header.clone()));
        let mut removed: VecDeque<DiffLine> = VecDeque::new();
        let mut added: VecDeque<DiffLine> = VecDeque::new();
        for line in hunk.lines {
            match line.kind {
                DiffLineKind::Context => {
                    flush_side_buffers(&mut out, &mut removed, &mut added);
                    out.push(SideRow::Data {
                        old: Some(line.clone()),
                        new: Some(line),
                    });
                }
                DiffLineKind::Removed => removed.push_back(line),
                DiffLineKind::Added => added.push_back(line),
                DiffLineKind::NoNewlineMarker => {
                    flush_side_buffers(&mut out, &mut removed, &mut added);
                    out.push(SideRow::Data {
                        old: Some(line),
                        new: None,
                    });
                }
            }
        }
        flush_side_buffers(&mut out, &mut removed, &mut added);
    }
    out
}

fn flush_side_buffers(
    out: &mut Vec<SideRow>,
    removed: &mut VecDeque<DiffLine>,
    added: &mut VecDeque<DiffLine>,
) {
    while !removed.is_empty() && !added.is_empty() {
        out.push(SideRow::Data {
            old: removed.pop_front(),
            new: added.pop_front(),
        });
    }
    for line in removed.drain(..) {
        out.push(SideRow::Data {
            old: Some(line),
            new: None,
        });
    }
    for line in added.drain(..) {
        out.push(SideRow::Data {
            old: None,
            new: Some(line),
        });
    }
}

pub(crate) fn render_side_row(row: &SideRow, idx: usize, highlights: &HighlightSlot) -> AnyElement {
    match row {
        SideRow::HunkHeader(header) => render_hunk_header(header, idx).into_any_element(),
        SideRow::Data { old, new } => {
            render_side_data_row(old.as_ref(), new.as_ref(), idx, highlights).into_any_element()
        }
    }
}

fn render_side_data_row(
    old: Option<&DiffLine>,
    new: Option<&DiffLine>,
    idx: usize,
    highlights: &HighlightSlot,
) -> Stateful<Div> {
    div()
        .id(("diff-side-row", idx))
        .flex()
        .items_center()
        .h(px(18.0))
        .text_size(px(12.0))
        .font_family("monospace")
        .child(render_side_half(old, SideKey::Old, highlights))
        .child(render_side_divider())
        .child(render_side_half(new, SideKey::New, highlights))
}

fn render_side_divider() -> Div {
    div().w(px(1.0)).flex_shrink_0().bg(theme::border())
}

fn render_side_half(line: Option<&DiffLine>, side: SideKey, highlights: &HighlightSlot) -> Div {
    let Some(line) = line else {
        return div()
            .flex_1()
            .flex_basis(px(0.0))
            .min_w(px(0.0))
            .bg(theme::bg_primary());
    };
    let (prefix, bg, fg) = kind_colors(line.kind);
    let lineno = match side {
        SideKey::Old => line.old_lineno,
        SideKey::New => line.new_lineno,
    };
    let label = fmt_lineno(lineno);

    let spans = resolve_highlight_for_side(line, side, highlights);

    div()
        .flex()
        .flex_1()
        .flex_basis(px(0.0))
        .min_w(px(0.0))
        .items_center()
        .bg(bg)
        .child(render_gutter(&label))
        .child(
            div()
                .w(px(16.0))
                .flex_shrink_0()
                .text_color(fg)
                .text_align(gpui::TextAlign::Center)
                .child(prefix.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_color(fg)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(styled_line_content(&line.content, spans)),
        )
}

#[cfg(test)]
mod tests {
    use super::{SideRow, build_side_by_side_rows};
    use zremote_protocol::project::{DiffHunk, DiffLine, DiffLineKind};

    fn ctx(old: u32, new: u32, s: &str) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Context,
            old_lineno: Some(old),
            new_lineno: Some(new),
            content: s.to_string(),
        }
    }

    fn removed(old: u32, s: &str) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Removed,
            old_lineno: Some(old),
            new_lineno: None,
            content: s.to_string(),
        }
    }

    fn added(new: u32, s: &str) -> DiffLine {
        DiffLine {
            kind: DiffLineKind::Added,
            old_lineno: None,
            new_lineno: Some(new),
            content: s.to_string(),
        }
    }

    #[test]
    fn side_by_side_pairs_adjacent_removed_and_added() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: "@@ -1 +1 @@".to_string(),
            lines: vec![removed(1, "old"), added(1, "new")],
        }];
        let rows = build_side_by_side_rows(&hunks);
        assert_eq!(rows.len(), 2);
        match &rows[1] {
            SideRow::Data { old, new } => {
                assert_eq!(old.as_ref().unwrap().content, "old");
                assert_eq!(new.as_ref().unwrap().content, "new");
            }
            SideRow::HunkHeader(_) => panic!("expected Data row"),
        }
    }

    #[test]
    fn side_by_side_emits_context_on_both_sides() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: "@@ -1 +1 @@".to_string(),
            lines: vec![ctx(1, 1, "ctx")],
        }];
        let rows = build_side_by_side_rows(&hunks);
        assert_eq!(rows.len(), 2);
        match &rows[1] {
            SideRow::Data { old, new } => {
                assert_eq!(old.as_ref().unwrap().content, "ctx");
                assert_eq!(new.as_ref().unwrap().content, "ctx");
            }
            SideRow::HunkHeader(_) => panic!("expected Data row"),
        }
    }

    #[test]
    fn side_by_side_unmatched_removed_has_blank_new() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 0,
            header: "@@ -1,2 +1,0 @@".to_string(),
            lines: vec![removed(1, "x"), removed(2, "y")],
        }];
        let rows = build_side_by_side_rows(&hunks);
        assert_eq!(rows.len(), 3);
        for row in rows.iter().skip(1) {
            match row {
                SideRow::Data { old, new } => {
                    assert!(old.is_some());
                    assert!(new.is_none());
                }
                SideRow::HunkHeader(_) => panic!("expected Data row"),
            }
        }
    }

    #[test]
    fn side_by_side_mixed_context_modification_deletion_addition() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 3,
            header: "@@ -1,3 +1,3 @@".to_string(),
            lines: vec![
                ctx(1, 1, "c1"),
                removed(2, "a"),
                removed(3, "b"),
                added(2, "A"),
                ctx(4, 3, "c2"),
                added(4, "C"),
            ],
        }];
        let rows = build_side_by_side_rows(&hunks);
        assert_eq!(rows.len(), 6);
        let expect_sides: Vec<(Option<&str>, Option<&str>)> = vec![
            (Some("c1"), Some("c1")),
            (Some("a"), Some("A")),
            (Some("b"), None),
            (Some("c2"), Some("c2")),
            (None, Some("C")),
        ];
        for (row, (eo, en)) in rows.iter().skip(1).zip(expect_sides.iter()) {
            match row {
                SideRow::Data { old, new } => {
                    assert_eq!(old.as_ref().map(|l| l.content.as_str()), *eo);
                    assert_eq!(new.as_ref().map(|l| l.content.as_str()), *en);
                }
                SideRow::HunkHeader(_) => panic!("expected Data row"),
            }
        }
    }
}
