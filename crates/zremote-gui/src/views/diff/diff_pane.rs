//! Center-pane unified diff renderer for a single `DiffFile`. MVP: no
//! syntax highlighting, no collapsed hunks, plain monospace lines.

use gpui::prelude::FluentBuilder;
use gpui::*;

use zremote_protocol::project::{DiffFile, DiffLine, DiffLineKind};

use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::diff::large_file::prepare_hunks;

pub struct DiffPane {
    file: Option<DiffFile>,
    /// Flat row list derived from `file.hunks` — one entry per rendered
    /// row (hunk headers + lines). Kept so `uniform_list` can virtualize.
    rows: Vec<Row>,
}

#[derive(Clone)]
enum Row {
    HunkHeader(String),
    Line(DiffLine),
}

impl DiffPane {
    pub fn new() -> Self {
        Self {
            file: None,
            rows: Vec::new(),
        }
    }

    pub fn set_file(&mut self, file: Option<DiffFile>, cx: &mut Context<Self>) {
        self.rows = match &file {
            Some(f) => build_rows(f),
            None => Vec::new(),
        };
        self.file = file;
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
        let rows = self.rows.clone();
        let count = rows.len();

        container.flex().flex_col().child(header).child(
            uniform_list(
                "diff-line-list",
                count,
                cx.processor(move |_this, range: std::ops::Range<usize>, _window, _cx| {
                    let mut out = Vec::with_capacity(range.len());
                    for idx in range {
                        let Some(row) = rows.get(idx) else {
                            continue;
                        };
                        out.push(render_row(row, idx));
                    }
                    out
                }),
            )
            .flex_1(),
        )
    }
}

fn build_rows(file: &DiffFile) -> Vec<Row> {
    let prepared = prepare_hunks(&file.hunks);
    let mut out = Vec::new();
    for hunk in prepared {
        out.push(Row::HunkHeader(hunk.header.clone()));
        for line in hunk.lines {
            out.push(Row::Line(line));
        }
    }
    out
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

fn render_row(row: &Row, idx: usize) -> AnyElement {
    match row {
        Row::HunkHeader(header) => render_hunk_header(header, idx).into_any_element(),
        Row::Line(line) => render_diff_line(line, idx).into_any_element(),
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

fn render_diff_line(line: &DiffLine, idx: usize) -> Stateful<Div> {
    let (prefix, bg, fg) = match line.kind {
        DiffLineKind::Context => (" ", theme::bg_primary(), theme::text_primary()),
        DiffLineKind::Added => ("+", theme::success_bg(), theme::text_primary()),
        DiffLineKind::Removed => ("-", theme::error_bg(), theme::text_primary()),
        DiffLineKind::NoNewlineMarker => ("~", theme::bg_primary(), theme::text_tertiary()),
    };

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
                .child(line.content.clone()),
        )
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
    use super::{Row, build_rows, fmt_lineno};
    use zremote_protocol::project::{
        DiffFile, DiffFileStatus, DiffFileSummary, DiffHunk, DiffLine, DiffLineKind,
    };

    fn file_with_hunks() -> DiffFile {
        DiffFile {
            summary: DiffFileSummary {
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
            },
            hunks: vec![DiffHunk {
                old_start: 1,
                old_lines: 2,
                new_start: 1,
                new_lines: 2,
                header: "@@ -1,2 +1,2 @@".to_string(),
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Context,
                        old_lineno: Some(1),
                        new_lineno: Some(1),
                        content: "a".to_string(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Removed,
                        old_lineno: Some(2),
                        new_lineno: None,
                        content: "b".to_string(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Added,
                        old_lineno: None,
                        new_lineno: Some(2),
                        content: "c".to_string(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn build_rows_flattens_hunks_and_lines() {
        let f = file_with_hunks();
        let rows = build_rows(&f);
        // 1 header + 3 lines
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0], Row::HunkHeader(_)));
        assert!(matches!(rows[1], Row::Line(_)));
    }

    #[test]
    fn fmt_lineno_empty_for_none() {
        assert_eq!(fmt_lineno(None), "");
        assert_eq!(fmt_lineno(Some(12)), "12");
    }
}
