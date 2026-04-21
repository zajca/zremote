//! Source picker: pill button at top of diff view. Click opens a popover
//! listing sources derived from `DiffSourceOptions`. MVP scope:
//! working-tree-vs-HEAD default, staged, working tree, HEAD vs branch
//! (first few local branches), recent commits, and Commit by SHA (fixed
//! entries — full SHA-input editor lands with P4).

use gpui::prelude::FluentBuilder;
use gpui::*;

use zremote_protocol::project::{DiffSource, DiffSourceOptions, RecentCommit};

use crate::icons::{Icon, icon};
use crate::theme;

pub enum SourcePickerEvent {
    Select(DiffSource),
}

pub struct SourcePicker {
    options: Option<DiffSourceOptions>,
    current: DiffSource,
    open: bool,
}

impl EventEmitter<SourcePickerEvent> for SourcePicker {}

impl SourcePicker {
    pub fn new() -> Self {
        Self {
            options: None,
            current: DiffSource::WorkingTreeVsHead,
            open: false,
        }
    }

    pub fn set_options(&mut self, opts: DiffSourceOptions, cx: &mut Context<Self>) {
        self.options = Some(opts);
        cx.notify();
    }

    pub fn set_current(&mut self, source: DiffSource, cx: &mut Context<Self>) {
        self.current = source;
        cx.notify();
    }
}

impl Render for SourcePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pill_label = label_for_source(&self.current);
        let open = self.open;
        let options = self.options.clone();

        div()
            .relative()
            .child(
                div()
                    .id("source-picker-pill")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(14.0))
                    .bg(theme::bg_tertiary())
                    .border_1()
                    .border_color(theme::border())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::bg_secondary()))
                    .child(
                        icon(Icon::GitBranch)
                            .size(px(12.0))
                            .text_color(theme::text_secondary()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_primary())
                            .child(pill_label),
                    )
                    .child(
                        icon(if open {
                            Icon::ChevronUp
                        } else {
                            Icon::ChevronDown
                        })
                        .size(px(10.0))
                        .text_color(theme::text_tertiary()),
                    )
                    .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.open = !this.open;
                        cx.notify();
                    })),
            )
            .when(open, |el| {
                // Full-viewport invisible scrim behind the popover catches
                // outside clicks and closes the picker. The popover itself
                // sits above the scrim (later child = higher in z-order) and
                // stops propagation so its own clicks don't dismiss it.
                el.child(render_dismiss_scrim(cx))
                    .child(render_popover(options.as_ref(), cx))
            })
    }
}

fn render_dismiss_scrim(cx: &mut Context<SourcePicker>) -> impl IntoElement {
    div()
        .id("source-picker-scrim")
        .absolute()
        .top(-px(10_000.0))
        .left(-px(10_000.0))
        .w(px(20_000.0))
        .h(px(20_000.0))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _event, _window, cx| {
                this.open = false;
                cx.notify();
            }),
        )
}

fn render_popover(
    options: Option<&DiffSourceOptions>,
    cx: &mut Context<SourcePicker>,
) -> Stateful<Div> {
    let mut popover = div()
        .id("source-picker-popover")
        .absolute()
        .top(px(32.0))
        .left(px(0.0))
        .w(px(280.0))
        .max_h(px(360.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme::border())
        .bg(theme::bg_secondary())
        .flex()
        .flex_col()
        .overflow_hidden()
        // Prevent clicks inside the popover from reaching the dismiss scrim
        // behind it — otherwise every row-click would close the picker
        // before its own handler ran.
        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
            cx.stop_propagation();
        });

    popover = popover.child(section_label("Local changes"));
    popover = popover.child(entry_row(
        "Working tree vs HEAD",
        DiffSource::WorkingTreeVsHead,
        cx,
    ));
    popover = popover.child(entry_row("Working tree", DiffSource::WorkingTree, cx));
    popover = popover.child(entry_row("Staged", DiffSource::Staged, cx));

    if let Some(opts) = options {
        // Local branches
        let local_branches: Vec<String> = opts
            .branches
            .local
            .iter()
            .map(|b| b.name.clone())
            .filter(|name| name != &opts.branches.current)
            .take(5)
            .collect();
        if !local_branches.is_empty() {
            popover = popover.child(section_label("Compare HEAD to branch"));
            for name in local_branches {
                let label = format!("HEAD vs {name}");
                popover = popover.child(entry_row(
                    &label,
                    DiffSource::HeadVs {
                        reference: name.clone(),
                    },
                    cx,
                ));
            }
        }

        if !opts.recent_commits.is_empty() {
            popover = popover.child(section_label("Recent commits"));
            for commit in opts.recent_commits.iter().take(8) {
                popover = popover.child(commit_entry_row(commit, cx));
            }
        }
    }

    popover
}

fn section_label(text: &str) -> impl IntoElement {
    div()
        .px(px(10.0))
        .py(px(4.0))
        .text_size(px(10.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_tertiary())
        .bg(theme::bg_tertiary())
        .child(text.to_string())
}

fn entry_row(label: &str, source: DiffSource, cx: &mut Context<SourcePicker>) -> Stateful<Div> {
    let id = SharedString::from(format!("src-entry-{label}"));
    let source_for_click = source.clone();
    div()
        .id(id)
        .flex()
        .items_center()
        .px(px(10.0))
        .py(px(5.0))
        .text_size(px(12.0))
        .text_color(theme::text_primary())
        .cursor_pointer()
        .hover(|s| s.bg(theme::bg_tertiary()))
        .child(label.to_string())
        .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
            this.open = false;
            this.current = source_for_click.clone();
            cx.emit(SourcePickerEvent::Select(source_for_click.clone()));
            cx.notify();
        }))
}

fn commit_entry_row(commit: &RecentCommit, cx: &mut Context<SourcePicker>) -> Stateful<Div> {
    let id = SharedString::from(format!("src-commit-{}", commit.sha));
    let source = DiffSource::Commit {
        sha: commit.sha.clone(),
    };
    let short = commit.short_sha.clone();
    let subject = commit.subject.clone();
    div()
        .id(id)
        .flex()
        .flex_col()
        .px(px(10.0))
        .py(px(5.0))
        .cursor_pointer()
        .hover(|s| s.bg(theme::bg_tertiary()))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .font_family("monospace")
                .text_color(theme::text_tertiary())
                .child(short),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme::text_primary())
                .whitespace_nowrap()
                .overflow_hidden()
                .child(subject),
        )
        .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
            this.open = false;
            this.current = source.clone();
            cx.emit(SourcePickerEvent::Select(source.clone()));
            cx.notify();
        }))
}

pub fn label_for_source(source: &DiffSource) -> String {
    match source {
        DiffSource::WorkingTree => "Working tree".to_string(),
        DiffSource::Staged => "Staged".to_string(),
        DiffSource::WorkingTreeVsHead => "Working tree vs HEAD".to_string(),
        DiffSource::HeadVs { reference } => format!("HEAD vs {reference}"),
        DiffSource::Range {
            from,
            to,
            symmetric,
        } => {
            if *symmetric {
                format!("{from}...{to}")
            } else {
                format!("{from}..{to}")
            }
        }
        DiffSource::Commit { sha } => {
            let short = if sha.len() > 7 { &sha[..7] } else { sha };
            format!("Commit {short}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::label_for_source;
    use zremote_protocol::project::DiffSource;

    #[test]
    fn label_for_working_tree() {
        assert_eq!(label_for_source(&DiffSource::WorkingTree), "Working tree");
    }

    #[test]
    fn label_for_head_vs() {
        assert_eq!(
            label_for_source(&DiffSource::HeadVs {
                reference: "main".to_string()
            }),
            "HEAD vs main"
        );
    }

    #[test]
    fn label_for_commit_truncates_sha() {
        assert_eq!(
            label_for_source(&DiffSource::Commit {
                sha: "abcdef0123456789".to_string()
            }),
            "Commit abcdef0"
        );
    }

    #[test]
    fn label_for_range_symmetric() {
        assert_eq!(
            label_for_source(&DiffSource::Range {
                from: "a".to_string(),
                to: "b".to_string(),
                symmetric: true
            }),
            "a...b"
        );
    }
}
