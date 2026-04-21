//! Center-pane diff renderer for a single `DiffFile`.
//!
//! Supports two layouts:
//!
//! - `ViewMode::Unified`: one column, `+` / `-` prefixes, old + new line
//!   numbers in twin gutters.
//! - `ViewMode::SideBySide`: two columns, old (pre-image) on the left,
//!   new (post-image) on the right. Context lines appear on both sides,
//!   aligned. Adjacent `-` / `+` pairs inside a hunk align horizontally
//!   (modification pair); unpaired removals or additions get a blank
//!   opposite cell.
//!
//! Syntax-highlight spans come from the parent `DiffView` which owns the
//! cache (keyed by blob SHA + syntax name). This module only paints
//! whatever it is handed; if the incoming `LineSpans` slice is empty (or
//! highlighting is disabled for the file) the line renders plain with the
//! diff background tint as its only colour cue.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;

use zremote_protocol::project::{DiffFile, DiffHunk, DiffLine, DiffLineKind};

use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::diff::highlight::{LineSpans, SideKey};
use crate::views::diff::large_file::prepare_hunks;
use crate::views::diff::state::ViewMode;

/// Cached per-file highlight spans. Keyed by the logical identity of the
/// pre- / post-image text that produced the spans — so a second call for
/// the same file with an unchanged side is a cache hit.
///
/// Inner key (`SideKey`) disambiguates pre- vs post-image within a single
/// cache entry, because a renamed file with new content still shares its
/// outer `blob_sha` key with other cache lookups for that same side.
pub type HighlightCache = HashMap<(String, String, SideKey), Arc<HashMap<u32, LineSpans>>>;

pub struct DiffPane {
    file: Option<DiffFile>,
    view_mode: ViewMode,
    unified_rows: Vec<UnifiedRow>,
    side_rows: Vec<SideRow>,
    /// Clone of the highlight cache snapshot relevant to the currently
    /// displayed file. The outer DiffView owns the canonical cache; it
    /// pushes updates here via `set_highlights`.
    highlights: HighlightSlot,
}

#[derive(Default, Clone)]
struct HighlightSlot {
    old: Option<Arc<HashMap<u32, LineSpans>>>,
    new: Option<Arc<HashMap<u32, LineSpans>>>,
}

#[derive(Clone)]
enum UnifiedRow {
    HunkHeader(String),
    Line(DiffLine),
}

/// Side-by-side row. A hunk header spans the full width; data rows
/// carry two independent slots so removals / additions / context can each
/// land on the correct side with the opposite side blank.
#[derive(Clone)]
pub(crate) enum SideRow {
    HunkHeader(String),
    Data {
        old: Option<DiffLine>,
        new: Option<DiffLine>,
    },
}

impl DiffPane {
    pub fn new() -> Self {
        Self {
            file: None,
            view_mode: ViewMode::Unified,
            unified_rows: Vec::new(),
            side_rows: Vec::new(),
            highlights: HighlightSlot::default(),
        }
    }

    pub fn set_file(&mut self, file: Option<DiffFile>, cx: &mut Context<Self>) {
        match &file {
            Some(f) => {
                self.unified_rows = build_unified_rows(f);
                self.side_rows = build_side_by_side_rows(&f.hunks);
            }
            None => {
                self.unified_rows.clear();
                self.side_rows.clear();
            }
        }
        self.file = file;
        // When the selected file changes the per-side highlight slots from
        // the previous file are stale. Clear them so a mis-keyed paint
        // can't survive into the new file.
        self.highlights = HighlightSlot::default();
        cx.notify();
    }

    pub fn set_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if self.view_mode == mode {
            return;
        }
        self.view_mode = mode;
        cx.notify();
    }

    /// Push the latest highlight results for the currently displayed file.
    /// Either slot may be `None` (e.g. old-side highlight pending, new-side
    /// ready). Stored behind `Arc` so paint-time clones are cheap.
    pub fn set_highlights(
        &mut self,
        old: Option<Arc<HashMap<u32, LineSpans>>>,
        new: Option<Arc<HashMap<u32, LineSpans>>>,
        cx: &mut Context<Self>,
    ) {
        self.highlights = HighlightSlot { old, new };
        cx.notify();
    }
}

impl Render for DiffPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let container = div()
            .flex_1()
            .h_full()
            .overflow_hidden()
            .bg(theme::bg_primary());

        let Some(file) = self.file.clone() else {
            return container.child(render_empty_pane());
        };

        if file.summary.binary {
            return container.child(render_info_card(
                "Binary file",
                "Content not shown for binary files.",
            ));
        }
        if file.summary.submodule {
            return container.child(render_info_card(
                "Submodule",
                "Submodule pointers are not rendered as textual diffs.",
            ));
        }
        if file.summary.too_large {
            return container.child(render_info_card(
                "File too large",
                "Diff hunks omitted — file exceeds the agent-side size threshold.",
            ));
        }

        let header = render_file_header(&file);
        match self.view_mode {
            ViewMode::Unified => container
                .flex()
                .flex_col()
                .child(header)
                .child(self.render_unified_body(cx)),
            ViewMode::SideBySide => container
                .flex()
                .flex_col()
                .child(header)
                .child(self.render_side_by_side_body(cx)),
        }
    }
}

impl DiffPane {
    fn render_unified_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.unified_rows.clone();
        let count = rows.len();
        let highlights = self.highlights.clone();
        uniform_list(
            "diff-line-list",
            count,
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, _cx| {
                let mut out = Vec::with_capacity(range.len());
                for idx in range {
                    let Some(row) = rows.get(idx) else {
                        continue;
                    };
                    out.push(render_unified_row(row, idx, &highlights));
                }
                out
            }),
        )
        .flex_1()
        .into_any_element()
    }

    fn render_side_by_side_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.side_rows.clone();
        let count = rows.len();
        let highlights = self.highlights.clone();
        uniform_list(
            "diff-side-list",
            count,
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, _cx| {
                let mut out = Vec::with_capacity(range.len());
                for idx in range {
                    let Some(row) = rows.get(idx) else {
                        continue;
                    };
                    out.push(render_side_row(row, idx, &highlights));
                }
                out
            }),
        )
        .flex_1()
        .into_any_element()
    }
}

fn build_unified_rows(file: &DiffFile) -> Vec<UnifiedRow> {
    let prepared = prepare_hunks(&file.hunks);
    let mut out = Vec::new();
    for hunk in prepared {
        out.push(UnifiedRow::HunkHeader(hunk.header.clone()));
        for line in hunk.lines {
            out.push(UnifiedRow::Line(line));
        }
    }
    out
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

fn render_file_header(file: &DiffFile) -> impl IntoElement {
    let path = file.summary.path.clone();
    let additions = file.summary.additions;
    let deletions = file.summary.deletions;
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(12.0))
        .py(px(6.0))
        .border_b_1()
        .border_color(theme::border())
        .bg(theme::bg_secondary())
        .child(
            div()
                .flex_1()
                .text_size(px(13.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_primary())
                .whitespace_nowrap()
                .overflow_hidden()
                .child(path),
        )
        .when(additions > 0 || deletions > 0, |el| {
            el.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(11.0))
                    .font_family("monospace")
                    .child(
                        div()
                            .text_color(theme::success())
                            .child(format!("+{additions}")),
                    )
                    .child(
                        div()
                            .text_color(theme::error())
                            .child(format!("-{deletions}")),
                    ),
            )
        })
}

fn render_unified_row(row: &UnifiedRow, idx: usize, highlights: &HighlightSlot) -> AnyElement {
    match row {
        UnifiedRow::HunkHeader(header) => render_hunk_header(header, idx).into_any_element(),
        UnifiedRow::Line(line) => {
            render_unified_diff_line(line, idx, highlights).into_any_element()
        }
    }
}

fn render_side_row(row: &SideRow, idx: usize, highlights: &HighlightSlot) -> AnyElement {
    match row {
        SideRow::HunkHeader(header) => render_hunk_header(header, idx).into_any_element(),
        SideRow::Data { old, new } => {
            render_side_data_row(old.as_ref(), new.as_ref(), idx, highlights).into_any_element()
        }
    }
}

fn render_hunk_header(header: &str, idx: usize) -> Stateful<Div> {
    div()
        .id(("hunk-header", idx))
        .flex()
        .items_center()
        .px(px(8.0))
        .py(px(2.0))
        .h(px(20.0))
        .bg(theme::bg_tertiary())
        .border_t_1()
        .border_b_1()
        .border_color(theme::border())
        .text_size(px(12.0))
        .font_family("monospace")
        .text_color(theme::text_secondary())
        .child(header.to_string())
}

fn kind_colors(kind: DiffLineKind) -> (&'static str, Rgba, Rgba) {
    match kind {
        DiffLineKind::Context => (" ", theme::bg_primary(), theme::text_primary()),
        DiffLineKind::Added => ("+", theme::success_bg(), theme::text_primary()),
        DiffLineKind::Removed => ("-", theme::error_bg(), theme::text_primary()),
        DiffLineKind::NoNewlineMarker => ("~", theme::bg_primary(), theme::text_tertiary()),
    }
}

fn render_unified_diff_line(
    line: &DiffLine,
    idx: usize,
    highlights: &HighlightSlot,
) -> Stateful<Div> {
    let (prefix, bg, fg) = kind_colors(line.kind);
    let old_ln = fmt_lineno(line.old_lineno);
    let new_ln = fmt_lineno(line.new_lineno);

    div()
        .id(("diff-line", idx))
        .flex()
        .items_center()
        .h(px(18.0))
        .bg(bg)
        .text_size(px(12.0))
        .font_family("monospace")
        .child(render_gutter(&old_ln))
        .child(render_gutter(&new_ln))
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
                .text_color(fg)
                .whitespace_nowrap()
                .overflow_hidden()
                .child(styled_line_content(
                    &line.content,
                    resolve_highlight_for_unified(line, highlights),
                )),
        )
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

/// Look up highlight spans for a `DiffLine` when rendering in UNIFIED mode.
///
/// The GUI caches spans per-side keyed by 1-based line number (`old` maps
/// from pre-image line numbers, `new` from post-image line numbers). Added
/// lines only exist on the new side; removed lines only on the old side;
/// context lines appear on both (we prefer the new-side lookup for
/// stability — same colours whether the line is a context row here or a
/// context row in side-by-side).
fn resolve_highlight_for_unified<'a>(
    line: &DiffLine,
    highlights: &'a HighlightSlot,
) -> Option<&'a [(std::ops::Range<usize>, HighlightStyle)]> {
    match line.kind {
        DiffLineKind::Added => line
            .new_lineno
            .and_then(|n| lookup(highlights.new.as_ref(), n)),
        DiffLineKind::Removed => line
            .old_lineno
            .and_then(|n| lookup(highlights.old.as_ref(), n)),
        DiffLineKind::Context => line
            .new_lineno
            .and_then(|n| lookup(highlights.new.as_ref(), n))
            .or_else(|| {
                line.old_lineno
                    .and_then(|n| lookup(highlights.old.as_ref(), n))
            }),
        DiffLineKind::NoNewlineMarker => None,
    }
}

fn resolve_highlight_for_side<'a>(
    line: &DiffLine,
    side: SideKey,
    highlights: &'a HighlightSlot,
) -> Option<&'a [(std::ops::Range<usize>, HighlightStyle)]> {
    match side {
        SideKey::Old => line
            .old_lineno
            .and_then(|n| lookup(highlights.old.as_ref(), n)),
        SideKey::New => line
            .new_lineno
            .and_then(|n| lookup(highlights.new.as_ref(), n)),
    }
}

fn lookup(
    spans: Option<&Arc<HashMap<u32, LineSpans>>>,
    lineno_1_based: u32,
) -> Option<&[(std::ops::Range<usize>, HighlightStyle)]> {
    let spans = spans?;
    spans.get(&lineno_1_based).map(Vec::as_slice)
}

/// Build a `StyledText` for a diff-line body. The parent container supplies
/// font family + base text colour via cascading style; this function just
/// overlays per-span colour / weight via `with_highlights` on top. Spans
/// that fall outside the byte length of `content` are skipped defensively —
/// stale cache entries can theoretically outlive the hunk they were sized
/// for.
fn styled_line_content(
    content: &str,
    spans: Option<&[(std::ops::Range<usize>, HighlightStyle)]>,
) -> StyledText {
    let text = content.to_string();
    let Some(spans) = spans else {
        return StyledText::new(text);
    };
    let byte_len = text.len();
    let filtered: Vec<(std::ops::Range<usize>, HighlightStyle)> = spans
        .iter()
        .filter(|(r, _)| {
            r.end <= byte_len
                && r.start < r.end
                && text.is_char_boundary(r.start)
                && text.is_char_boundary(r.end)
        })
        .cloned()
        .collect();
    StyledText::new(text).with_highlights(filtered)
}

fn render_gutter(label: &str) -> Div {
    div()
        .w(px(48.0))
        .flex_shrink_0()
        .px(px(6.0))
        .text_size(px(11.0))
        .text_color(theme::text_tertiary())
        .font_family("monospace")
        .text_align(gpui::TextAlign::Right)
        .child(label.to_string())
}

fn fmt_lineno(n: Option<u32>) -> String {
    n.map_or_else(String::new, |v| format!("{v}"))
}

fn render_empty_pane() -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.0))
        .child(
            icon(Icon::FileText)
                .size(px(28.0))
                .text_color(theme::text_tertiary()),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::text_secondary())
                .child("Select a file to view its diff"),
        )
}

fn render_info_card(title: &str, body: &str) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.0))
                .p(px(24.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(theme::border())
                .bg(theme::bg_secondary())
                .child(
                    icon(Icon::Info)
                        .size(px(24.0))
                        .text_color(theme::text_secondary()),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme::text_secondary())
                        .child(body.to_string()),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::{SideRow, UnifiedRow, build_side_by_side_rows, build_unified_rows, fmt_lineno};
    use zremote_protocol::project::{
        DiffFile, DiffFileStatus, DiffFileSummary, DiffHunk, DiffLine, DiffLineKind,
    };

    fn summary() -> DiffFileSummary {
        DiffFileSummary {
            path: "a.rs".to_string(),
            old_path: None,
            status: DiffFileStatus::Modified,
            binary: false,
            submodule: false,
            too_large: false,
            additions: 1,
            deletions: 1,
            old_sha: None,
            new_sha: None,
            old_mode: None,
            new_mode: None,
        }
    }

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

    fn file_with_hunks() -> DiffFile {
        DiffFile {
            summary: summary(),
            hunks: vec![DiffHunk {
                old_start: 1,
                old_lines: 2,
                new_start: 1,
                new_lines: 2,
                header: "@@ -1,2 +1,2 @@".to_string(),
                lines: vec![ctx(1, 1, "a"), removed(2, "b"), added(2, "c")],
            }],
        }
    }

    #[test]
    fn build_unified_rows_flattens_hunks_and_lines() {
        let f = file_with_hunks();
        let rows = build_unified_rows(&f);
        // 1 header + 3 lines
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0], UnifiedRow::HunkHeader(_)));
        assert!(matches!(rows[1], UnifiedRow::Line(_)));
    }

    #[test]
    fn fmt_lineno_empty_for_none() {
        assert_eq!(fmt_lineno(None), "");
        assert_eq!(fmt_lineno(Some(12)), "12");
    }

    #[test]
    fn side_by_side_pairs_adjacent_removed_and_added() {
        // Classic modification pair: - immediately followed by +.
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: "@@ -1 +1 @@".to_string(),
            lines: vec![removed(1, "old"), added(1, "new")],
        }];
        let rows = build_side_by_side_rows(&hunks);
        assert_eq!(rows.len(), 2); // header + 1 paired row
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
        // Pure deletion, no counterpart addition.
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 0,
            header: "@@ -1,2 +1,0 @@".to_string(),
            lines: vec![removed(1, "x"), removed(2, "y")],
        }];
        let rows = build_side_by_side_rows(&hunks);
        assert_eq!(rows.len(), 3); // header + 2 old-only rows
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
        // Order: ctx / -a / -b / +A / ctx / +C.
        // Expected alignment: ctx-on-both, (-a,+A), (-b, blank), ctx-on-both,
        // (blank, +C).
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
        // header + c1 + (a,A) + (b,_) + c2 + (_,C) = 6
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
