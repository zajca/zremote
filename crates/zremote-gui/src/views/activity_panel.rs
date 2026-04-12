//! Activity panel: structured view of Claude Code execution progress.
//!
//! Shows a live feed of execution nodes (commands, tool calls, file operations)
//! alongside the terminal for print mode tasks.

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::cc_widgets;
use crate::views::sidebar::CcMetrics;
use zremote_client::AgenticStatus;

/// Maximum number of nodes kept in the feed (oldest are dropped).
const MAX_NODES: usize = 200;

/// A single execution node displayed in the activity feed.
#[derive(Clone)]
pub struct ExecutionNodeItem {
    pub node_id: i64,
    pub timestamp: i64,
    pub kind: String,
    pub input: Option<String>,
    pub output_summary: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
}

/// Activity panel showing CC execution progress (execution nodes, status, metrics).
pub struct ActivityPanel {
    session_id: String,
    nodes: Vec<ExecutionNodeItem>,
    cc_status: Option<AgenticStatus>,
    cc_metrics: Option<CcMetrics>,
    task_name: Option<String>,
    scroll_handle: ScrollHandle,
}

/// Events emitted by the activity panel.
pub enum ActivityPanelEvent {
    Close,
}

impl EventEmitter<ActivityPanelEvent> for ActivityPanel {}

impl ActivityPanel {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            nodes: Vec::new(),
            cc_status: None,
            cc_metrics: None,
            task_name: None,
            scroll_handle: ScrollHandle::new(),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Prepend a node to the feed (newest first), capping at `MAX_NODES`.
    pub fn push_node(&mut self, node: ExecutionNodeItem, cx: &mut Context<Self>) {
        self.nodes.insert(0, node);
        if self.nodes.len() > MAX_NODES {
            self.nodes.truncate(MAX_NODES);
        }
        cx.notify();
    }

    /// Load historical nodes (oldest-first from API, reversed to newest-first).
    pub fn load_nodes(&mut self, mut nodes: Vec<ExecutionNodeItem>, cx: &mut Context<Self>) {
        nodes.reverse();
        self.nodes = nodes;
        if self.nodes.len() > MAX_NODES {
            self.nodes.truncate(MAX_NODES);
        }
        cx.notify();
    }

    pub fn update_status(&mut self, status: Option<AgenticStatus>, cx: &mut Context<Self>) {
        self.cc_status = status;
        cx.notify();
    }

    pub fn update_metrics(&mut self, metrics: CcMetrics, cx: &mut Context<Self>) {
        self.cc_metrics = Some(metrics);
        cx.notify();
    }

    pub fn update_task_name(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.task_name = name;
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.nodes.clear();
        self.cc_status = None;
        self.cc_metrics = None;
        self.task_name = None;
        cx.notify();
    }

    // ------------------------------------------------------------------
    // Render helpers
    // ------------------------------------------------------------------

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.cc_status.unwrap_or(AgenticStatus::Unknown);
        let status_text = status_label(status);

        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(cc_widgets::cc_bot_icon(status, 14.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_primary())
                            .child(status_text.to_string()),
                    )
                    .when_some(self.task_name.as_ref(), |el, name| {
                        el.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::text_tertiary())
                                .child(truncate_str(name, 30)),
                        )
                    }),
            )
            .child(
                div()
                    .id("activity-close")
                    .cursor_pointer()
                    .p(px(2.0))
                    .rounded(px(4.0))
                    .hover(|s| s.bg(theme::bg_tertiary()))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(ActivityPanelEvent::Close);
                    }))
                    .child(
                        icon(Icon::X)
                            .size(px(14.0))
                            .text_color(theme::text_secondary()),
                    ),
            )
    }

    fn render_metrics_summary(&self) -> Option<AnyElement> {
        let metrics = self.cc_metrics.as_ref()?;

        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(theme::border())
            .text_size(px(11.0))
            .text_color(theme::text_secondary());

        // Model name
        if let Some(ref model) = metrics.model {
            row = row.child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(cc_widgets::short_model_name(model)),
            );
        }

        // Context bar
        row = row.child(cc_widgets::render_context_bar(metrics, 60.0, 4.0));

        // Context percentage
        if let Some(pct) = metrics.context_used_pct {
            let (_, pct_200k) =
                cc_widgets::context_usage_200k(pct, metrics.context_window_size.unwrap_or(200_000));
            row = row.child(div().child(format!("{pct_200k:.0}%")));
        }

        // Cost
        if let Some(cost) = metrics.cost_usd {
            row = row.child(div().child(format!("${cost:.2}")));
        }

        // Lines
        if metrics.lines_added.is_some() || metrics.lines_removed.is_some() {
            let added = metrics.lines_added.unwrap_or(0);
            let removed = metrics.lines_removed.unwrap_or(0);
            row = row.child(
                div().child(
                    div()
                        .flex()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_color(theme::success())
                                .child(format!("+{added}")),
                        )
                        .child(
                            div()
                                .text_color(theme::error())
                                .child(format!("-{removed}")),
                        ),
                ),
            );
        }

        Some(row.into_any_element())
    }

    fn render_node_feed(&self) -> impl IntoElement {
        let mut feed = div()
            .id("activity-feed")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle);

        if self.nodes.is_empty() {
            feed = feed.child(self.render_empty_state());
        } else {
            for (i, node) in self.nodes.iter().enumerate() {
                feed = feed.child(self.render_node_item(node, i));
            }
        }

        feed
    }

    fn render_node_item(&self, node: &ExecutionNodeItem, _index: usize) -> impl IntoElement {
        let node_icon = kind_icon(&node.kind);
        let duration = format_duration(node.duration_ms);
        let label = kind_label(&node.kind).to_string();
        let input_text = node.input.as_deref().map(|s| truncate_str(s, 60));
        let summary_text = node.output_summary.as_deref().map(|s| truncate_str(s, 80));
        let exit_code = node.exit_code;
        let node_id = node.node_id;

        let mut item = div()
            .id(ElementId::NamedInteger("node".into(), node_id as u64))
            .flex()
            .items_start()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(theme::border())
            .hover(|s| s.bg(theme::bg_tertiary()));

        // Left: icon
        item = item.child(
            div().pt(px(2.0)).child(
                icon(node_icon)
                    .size(px(14.0))
                    .text_color(theme::text_tertiary()),
            ),
        );

        // Middle: kind + input + output
        let mut middle = div().flex().flex_col().flex_1().overflow_hidden();

        // Kind + input on first line
        let mut first_line = div().flex().items_center().gap(px(4.0)).child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_primary())
                .child(label),
        );

        if let Some(input) = input_text {
            first_line = first_line.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_secondary())
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(input),
            );
        }

        middle = middle.child(first_line);

        // Output summary on second line
        if let Some(summary) = summary_text {
            middle = middle.child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme::text_tertiary())
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(summary),
            );
        }

        item = item.child(middle);

        // Right: duration + exit code
        let mut right = div().flex().items_center().gap(px(4.0)).flex_shrink_0();

        right = right.child(
            div()
                .text_size(px(10.0))
                .text_color(theme::text_tertiary())
                .child(duration),
        );

        if let Some(code) = exit_code {
            if code == 0 {
                right = right.child(
                    icon(Icon::CheckCircle)
                        .size(px(12.0))
                        .text_color(theme::success()),
                );
            } else {
                right = right.child(
                    icon(Icon::XCircle)
                        .size(px(12.0))
                        .text_color(theme::error()),
                );
            }
        }

        item = item.child(right);
        item
    }

    fn render_empty_state(&self) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .flex_1()
            .gap(px(8.0))
            .py(px(40.0))
            .child(
                icon(Icon::Zap)
                    .size(px(24.0))
                    .text_color(theme::text_tertiary()),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_tertiary())
                    .child("No activity yet"),
            )
    }
}

impl Render for ActivityPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::bg_secondary());

        panel = panel.child(self.render_header(cx));

        if let Some(metrics) = self.render_metrics_summary() {
            panel = panel.child(metrics);
        }

        panel = panel.child(self.render_node_feed());
        panel
    }
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn status_label(status: AgenticStatus) -> &'static str {
    match status {
        AgenticStatus::Working => "Working",
        AgenticStatus::RequiresAction => "Requires action",
        AgenticStatus::WaitingForInput => "Waiting for input",
        AgenticStatus::Error => "Error",
        AgenticStatus::Completed => "Completed",
        _ => "Idle",
    }
}

fn kind_icon(kind: &str) -> Icon {
    match kind {
        "bash" | "shell_command" | "terminal" => Icon::SquareTerminal,
        "read" | "edit" | "write" | "file_read" | "file_write" | "file" => Icon::FileText,
        "tool_call" | "agent" => Icon::Bot,
        _ => Icon::Zap,
    }
}

fn kind_label(kind: &str) -> &str {
    match kind {
        "bash" | "shell_command" | "terminal" => "Bash",
        "read" | "file_read" => "Read",
        "edit" => "Edit",
        "write" | "file_write" => "Write",
        "tool_call" => "Tool",
        "agent" => "Agent",
        _ => kind,
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut result: String = s.chars().take(max.saturating_sub(1)).collect();
        result.push('\u{2026}');
        result
    }
}

fn format_duration(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{:.1}m", ms as f64 / 60_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{format_duration, kind_icon, truncate_str};
    use crate::icons::Icon;

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let result = truncate_str("hello world this is long", 10);
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn format_duration_ms() {
        assert_eq!(format_duration(500), "500ms");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(1500), "1.5s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(90_000), "1.5m");
    }

    #[test]
    fn kind_icon_mapping() {
        assert!(matches!(kind_icon("bash"), Icon::SquareTerminal));
        assert!(matches!(kind_icon("read"), Icon::FileText));
        assert!(matches!(kind_icon("tool_call"), Icon::Bot));
        assert!(matches!(kind_icon("unknown"), Icon::Zap));
    }
}
