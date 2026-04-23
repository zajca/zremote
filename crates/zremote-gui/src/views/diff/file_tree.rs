//! Left-pane file list. Flat virtualized list of `DiffFileSummary` rows.

use gpui::prelude::FluentBuilder;
use gpui::*;

use zremote_protocol::project::{DiffFileStatus, DiffFileSummary};

use crate::icons::{Icon, icon};
use crate::theme;

pub enum FileTreeEvent {
    Select(String),
}

pub struct FileTree {
    files: Vec<DiffFileSummary>,
    selected: Option<String>,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl FileTree {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            selected: None,
        }
    }

    pub fn set_files(&mut self, files: Vec<DiffFileSummary>, cx: &mut Context<Self>) {
        self.files = files;
        cx.notify();
    }

    pub fn set_selected(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        self.selected = path;
        cx.notify();
    }

    pub fn files(&self) -> &[DiffFileSummary] {
        &self.files
    }
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.files.len();
        let files = self.files.clone();
        let selected = self.selected.clone();

        div()
            .flex()
            .flex_col()
            .w(px(250.0))
            .h_full()
            .border_r_1()
            .border_color(theme::border())
            .bg(theme::bg_secondary())
            .child(render_header(count))
            .child(
                uniform_list(
                    "diff-file-list",
                    count,
                    cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                        let mut rows = Vec::with_capacity(range.len());
                        for idx in range {
                            let Some(file) = files.get(idx) else {
                                continue;
                            };
                            rows.push(render_row(file, idx, selected.as_deref(), cx));
                        }
                        rows
                    }),
                )
                .flex_1(),
            )
    }
}

fn render_row(
    file: &DiffFileSummary,
    idx: usize,
    selected: Option<&str>,
    cx: &mut Context<FileTree>,
) -> Stateful<Div> {
    let is_selected = selected == Some(file.path.as_str());
    let path = file.path.clone();
    let path_for_click = path.clone();
    let status = file.status;
    let additions = file.additions;
    let deletions = file.deletions;
    let truncated = truncate_path(&path, 32);

    let bg = if is_selected {
        theme::bg_tertiary()
    } else {
        theme::bg_secondary()
    };

    div()
        .id(idx)
        .flex()
        .items_center()
        .gap(px(6.0))
        .px(px(10.0))
        .py(px(4.0))
        .h(px(22.0))
        .cursor_pointer()
        .bg(bg)
        .hover(|s| s.bg(theme::bg_tertiary()))
        .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
            this.selected = Some(path_for_click.clone());
            cx.emit(FileTreeEvent::Select(path_for_click.clone()));
            cx.notify();
        }))
        .child(render_status_icon(status))
        .child(
            div()
                .flex_1()
                .text_size(px(12.0))
                .text_color(theme::text_primary())
                .whitespace_nowrap()
                .overflow_hidden()
                .child(truncated),
        )
        .when(additions > 0 || deletions > 0, |el| {
            el.child(render_counts(additions, deletions))
        })
}

fn render_header(count: usize) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(12.0))
        .py(px(6.0))
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme::text_secondary())
                .font_weight(FontWeight::MEDIUM)
                .child(format!("Files ({count})")),
        )
}

fn render_status_icon(status: DiffFileStatus) -> impl IntoElement {
    let (ic, color) = status_icon_color(status);
    icon(ic).size(px(12.0)).text_color(color)
}

pub fn status_icon_color(status: DiffFileStatus) -> (Icon, gpui::Rgba) {
    match status {
        DiffFileStatus::Added => (Icon::Plus, theme::success()),
        DiffFileStatus::Deleted => (Icon::X, theme::error()),
        DiffFileStatus::Modified => (Icon::FileText, theme::warning()),
        DiffFileStatus::Renamed => (Icon::ChevronRight, theme::text_secondary()),
        DiffFileStatus::Copied => (Icon::Plus, theme::text_secondary()),
        DiffFileStatus::TypeChanged => (Icon::AlertTriangle, theme::warning()),
    }
}

fn render_counts(additions: u32, deletions: u32) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
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
        )
}

/// Path truncation that keeps the trailing segment readable. If the string
/// fits in `max`, returned as-is. Otherwise prepends `…/` to the last
/// `max-2` chars (so the file name remains visible).
pub fn truncate_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_string();
    }
    let take = max.saturating_sub(2);
    let tail: String = path
        .chars()
        .rev()
        .take(take)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…/{tail}")
}

#[cfg(test)]
mod tests {
    use super::{status_icon_color, truncate_path};
    use zremote_protocol::project::DiffFileStatus;

    #[test]
    fn truncate_short_returns_as_is() {
        assert_eq!(truncate_path("src/a.rs", 32), "src/a.rs");
    }

    #[test]
    fn truncate_long_keeps_tail() {
        let long = "a/very/deeply/nested/path/to/some/file.rs";
        let out = truncate_path(long, 20);
        assert!(out.starts_with("…/"));
        assert!(out.ends_with("file.rs"));
    }

    #[test]
    fn status_icon_color_maps_each_status_to_expected_icon() {
        use crate::icons::Icon;

        let (icon, _) = status_icon_color(DiffFileStatus::Added);
        assert!(matches!(icon, Icon::Plus));

        let (icon, _) = status_icon_color(DiffFileStatus::Deleted);
        assert!(matches!(icon, Icon::X));

        let (icon, _) = status_icon_color(DiffFileStatus::Modified);
        assert!(matches!(icon, Icon::FileText));

        let (icon, _) = status_icon_color(DiffFileStatus::Renamed);
        assert!(matches!(icon, Icon::ChevronRight));

        let (icon, _) = status_icon_color(DiffFileStatus::Copied);
        assert!(matches!(icon, Icon::Plus));

        let (icon, _) = status_icon_color(DiffFileStatus::TypeChanged);
        assert!(matches!(icon, Icon::AlertTriangle));
    }
}
