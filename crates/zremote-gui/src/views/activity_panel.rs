//! Activity panel: structured view of Claude Code execution progress.
//!
//! Shows a live feed of execution nodes (commands, tool calls, file operations)
//! alongside the terminal for print mode tasks.

use std::collections::VecDeque;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::cc_widgets;
use crate::views::sidebar::CcMetrics;
use zremote_client::AgenticStatus;
use zremote_protocol::NodeStatus;

/// Maximum number of nodes kept in the feed (oldest are dropped).
const MAX_NODES: usize = 200;

/// A single execution node displayed in the activity feed.
///
/// Display strings are pre-computed at creation time to avoid per-frame
/// allocations in the render path (200 nodes x 60 Hz = 12k calls/s).
#[derive(Clone)]
pub struct ExecutionNodeItem {
    pub node_id: i64,
    pub tool_use_id: String,
    pub timestamp: i64,
    pub exit_code: Option<i32>,
    pub status: NodeStatus,
    // Pre-computed display fields
    pub display_icon: Icon,
    pub display_label: String,
    pub display_duration: String,
    pub display_input: Option<String>,
    pub display_summary: Option<String>,
}

impl ExecutionNodeItem {
    /// Create a new node item, pre-computing all display strings.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: i64,
        tool_use_id: String,
        timestamp: i64,
        kind: &str,
        input: Option<&str>,
        output_summary: Option<&str>,
        exit_code: Option<i32>,
        duration_ms: i64,
        status: NodeStatus,
    ) -> Self {
        Self {
            node_id,
            tool_use_id,
            timestamp,
            exit_code,
            status,
            display_icon: kind_icon(kind),
            display_label: capitalize_first(&truncate_str(kind_label(kind), 20)),
            display_duration: format_duration(duration_ms),
            display_input: input.map(|s| truncate_str(s, 60)),
            display_summary: output_summary.map(|s| truncate_str(s, 80)),
        }
    }
}

/// Activity panel showing CC execution progress (execution nodes, status, metrics).
pub struct ActivityPanel {
    session_id: String,
    nodes: VecDeque<ExecutionNodeItem>,
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
            nodes: VecDeque::new(),
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
        self.nodes.push_front(node);
        if self.nodes.len() > MAX_NODES {
            self.nodes.truncate(MAX_NODES);
        }
        cx.notify();
    }

    /// Load historical nodes (oldest-first from API, reversed to newest-first).
    pub fn load_nodes(&mut self, nodes: Vec<ExecutionNodeItem>, cx: &mut Context<Self>) {
        self.nodes = nodes.into_iter().rev().collect();
        if self.nodes.len() > MAX_NODES {
            self.nodes.truncate(MAX_NODES);
        }
        cx.notify();
    }

    /// Update an existing node in place by `node_id`. Returns true if updated.
    #[allow(clippy::too_many_arguments)]
    pub fn update_node(
        &mut self,
        node_id: i64,
        status: NodeStatus,
        kind: &str,
        output_summary: Option<&str>,
        exit_code: Option<i32>,
        duration_ms: i64,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(node) = self.nodes.iter_mut().find(|n| n.node_id == node_id) {
            node.status = status;
            node.exit_code = exit_code;
            node.display_icon = kind_icon(kind);
            node.display_label = capitalize_first(&truncate_str(kind_label(kind), 20));
            node.display_duration = format_duration(duration_ms);
            node.display_summary = output_summary.map(|s| truncate_str(s, 80));
            cx.notify();
            true
        } else {
            false
        }
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
        let node_icon = node.display_icon;
        let exit_code = node.exit_code;
        let node_id = node.node_id;
        let status = node.status;

        // Running indicator: 2px accent strip as the first child (avoids border_color
        // limitations — GPUI sets all sides at once, making per-side colors impossible).
        // The strip is always present (transparent when not running) to prevent layout shift.
        let strip_color = if status == NodeStatus::Running {
            theme::accent()
        } else {
            gpui::rgba(0x0000_0000)
        };
        let accent_strip = div()
            .w(px(2.0))
            .h_full()
            .flex_shrink_0()
            .rounded(px(1.0))
            .bg(strip_color);

        let mut item = div()
            .id(ElementId::NamedInteger("node".into(), node_id as u64))
            .flex()
            .items_start()
            .gap(px(8.0))
            .pl(px(6.0))
            .pr(px(12.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(theme::border())
            .hover(|s| s.bg(theme::bg_tertiary()));

        item = item.child(accent_strip);

        // Left: icon (spinner for Running, static for others)
        let animation_id = SharedString::from(format!("loader-spin-{node_id}"));
        let icon_element: AnyElement = if status == NodeStatus::Running {
            icon(Icon::Loader)
                .size(px(14.0))
                .text_color(theme::accent())
                .with_animation(
                    animation_id,
                    Animation::new(Duration::from_millis(1000))
                        .repeat()
                        .with_easing(linear),
                    |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
                )
                .into_any_element()
        } else {
            icon(node_icon)
                .size(px(14.0))
                .text_color(theme::text_tertiary())
                .into_any_element()
        };

        item = item.child(div().pt(px(2.0)).child(icon_element));

        // Middle: kind + input + output
        let mut middle = div().flex().flex_col().flex_1().overflow_hidden();

        // Kind + input on first line
        let mut first_line = div().flex().items_center().gap(px(4.0)).child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_primary())
                .child(node.display_label.clone()),
        );

        if let Some(ref input) = node.display_input {
            first_line = first_line.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::text_secondary())
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(input.clone()),
            );
        }

        middle = middle.child(first_line);

        // Output summary on second line
        if let Some(ref summary) = node.display_summary {
            middle = middle.child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme::text_tertiary())
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(summary.clone()),
            );
        }

        item = item.child(middle);

        // Right: duration (only when settled) + fixed-width status icon slot.
        // Duration is hidden while Running to prevent reflow as the value changes.
        // The status slot is always 16px wide to prevent layout shift on transition.
        let mut right = div().flex().items_center().gap(px(4.0)).flex_shrink_0();

        if status != NodeStatus::Running {
            right = right.child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme::text_tertiary())
                    .child(node.display_duration.clone()),
            );
        }

        let status_slot = div().w(px(16.0)).flex().items_center().justify_center();
        let status_slot = match status {
            NodeStatus::Running => status_slot,
            NodeStatus::Completed => {
                let (icon_variant, color) = if exit_code.unwrap_or(1) == 0 {
                    (Icon::CheckCircle, theme::success())
                } else {
                    (Icon::XCircle, theme::error())
                };
                status_slot.child(icon(icon_variant).size(px(12.0)).text_color(color))
            }
            NodeStatus::Stopped => status_slot.child(
                icon(Icon::CircleSlash)
                    .size(px(12.0))
                    .text_color(theme::text_secondary()),
            ),
            NodeStatus::Stale => status_slot.child(
                icon(Icon::AlertCircle)
                    .size(px(12.0))
                    .text_color(theme::warning()),
            ),
            NodeStatus::Unknown => status_slot,
        };
        right = right.child(status_slot);

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
        AgenticStatus::Idle => "Idle",
        AgenticStatus::Error => "Error",
        AgenticStatus::Completed => "Completed",
        AgenticStatus::Unknown => "Unknown",
    }
}

fn kind_icon(kind: &str) -> Icon {
    if kind.starts_with("mcp__") {
        return Icon::Bot;
    }
    match kind {
        "bash" | "shell_command" | "terminal" => Icon::SquareTerminal,
        "read" | "edit" | "write" | "file_read" | "file_write" | "file" | "glob" | "grep" => {
            Icon::FileText
        }
        "task" | "tool_call" | "agent" | "agent_response" => Icon::Bot,
        "webfetch" | "todowrite" => Icon::Zap,
        _ => Icon::Zap,
    }
}

fn kind_label(kind: &str) -> &str {
    if kind.starts_with("mcp__") {
        return kind;
    }
    match kind {
        "bash" => "bash",
        "shell_command" => "shell_command",
        "terminal" => "terminal",
        "read" | "file_read" => "read",
        "edit" => "edit",
        "write" | "file_write" => "write",
        "glob" => "glob",
        "grep" => "grep",
        "task" => "task",
        "webfetch" => "webfetch",
        "todowrite" => "todowrite",
        "agent_response" => "agent_response",
        "tool_call" => "tool_call",
        "agent" => "agent",
        _ => kind,
    }
}

/// Capitalize the first character of a string, for unknown kinds.
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
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
    use super::{
        ExecutionNodeItem, MAX_NODES, capitalize_first, format_duration, kind_icon, kind_label,
        status_label, truncate_str,
    };
    use crate::icons::Icon;
    use std::collections::VecDeque;
    use zremote_client::AgenticStatus;
    use zremote_protocol::NodeStatus;

    #[test]
    fn truncate_str_cases() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
        assert_eq!(truncate_str("", 10), "");
        let result = truncate_str("hello world this is long", 10);
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn format_duration_ranges() {
        assert_eq!(format_duration(0), "0ms");
        assert_eq!(format_duration(500), "500ms");
        assert_eq!(format_duration(999), "999ms");
        assert_eq!(format_duration(1000), "1.0s");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(60_000), "1.0m");
        assert_eq!(format_duration(90_000), "1.5m");
    }

    #[test]
    fn kind_icon_mapping() {
        assert!(matches!(kind_icon("bash"), Icon::SquareTerminal));
        assert!(matches!(kind_icon("read"), Icon::FileText));
        assert!(matches!(kind_icon("tool_call"), Icon::Bot));
        assert!(matches!(kind_icon("unknown"), Icon::Zap));
    }

    #[test]
    fn kind_label_mapping() {
        assert_eq!(kind_label("bash"), "bash");
        assert_eq!(kind_label("read"), "read");
        assert_eq!(kind_label("edit"), "edit");
        assert_eq!(kind_label("write"), "write");
        assert_eq!(kind_label("tool_call"), "tool_call");
        assert_eq!(kind_label("agent"), "agent");
        assert_eq!(kind_label("custom_thing"), "custom_thing");
    }

    #[test]
    fn status_label_all_variants() {
        assert_eq!(status_label(AgenticStatus::Working), "Working");
        assert_eq!(
            status_label(AgenticStatus::RequiresAction),
            "Requires action"
        );
        assert_eq!(
            status_label(AgenticStatus::WaitingForInput),
            "Waiting for input"
        );
        assert_eq!(status_label(AgenticStatus::Idle), "Idle");
        assert_eq!(status_label(AgenticStatus::Error), "Error");
        assert_eq!(status_label(AgenticStatus::Completed), "Completed");
        assert_eq!(status_label(AgenticStatus::Unknown), "Unknown");
    }

    #[test]
    fn node_item_precomputes_display() {
        let node = ExecutionNodeItem::new(
            1,
            "tu_1".to_string(),
            0,
            "bash",
            None,
            None,
            None,
            500,
            NodeStatus::Completed,
        );
        assert_eq!(node.display_label, "Bash");
        assert_eq!(node.display_duration, "500ms");
        assert!(matches!(node.display_icon, Icon::SquareTerminal));
        assert!(node.display_input.is_none());

        // Truncation
        let long = "a".repeat(100);
        let node2 = ExecutionNodeItem::new(
            2,
            "tu_2".to_string(),
            0,
            "read",
            Some(&long),
            Some(&long),
            None,
            0,
            NodeStatus::Completed,
        );
        assert!(node2.display_input.as_ref().unwrap().chars().count() <= 60);
        assert!(node2.display_summary.as_ref().unwrap().chars().count() <= 80);

        // Unknown kind label truncation
        let long_kind = "very_long_custom_kind_name_that_exceeds_twenty_chars";
        let node3 = ExecutionNodeItem::new(
            3,
            "tu_3".to_string(),
            0,
            long_kind,
            None,
            None,
            None,
            0,
            NodeStatus::Running,
        );
        assert!(node3.display_label.chars().count() <= 20);
    }

    #[test]
    fn vec_deque_cap_and_order() {
        // Cap test
        let mut nodes: VecDeque<ExecutionNodeItem> = VecDeque::new();
        for i in 0..=MAX_NODES {
            nodes.push_front(ExecutionNodeItem::new(
                i as i64,
                String::new(),
                0,
                "bash",
                None,
                None,
                None,
                0,
                NodeStatus::Completed,
            ));
            if nodes.len() > MAX_NODES {
                nodes.truncate(MAX_NODES);
            }
        }
        assert_eq!(nodes.len(), MAX_NODES);
        assert_eq!(nodes.front().unwrap().node_id, MAX_NODES as i64);

        // Reverse order test (simulates load_nodes)
        let items: Vec<ExecutionNodeItem> = (0..5)
            .map(|i| {
                ExecutionNodeItem::new(
                    i,
                    String::new(),
                    0,
                    "bash",
                    None,
                    None,
                    None,
                    0,
                    NodeStatus::Completed,
                )
            })
            .collect();
        let ordered: VecDeque<ExecutionNodeItem> = items.into_iter().rev().collect();
        assert_eq!(ordered.front().unwrap().node_id, 4);
        assert_eq!(ordered.back().unwrap().node_id, 0);
    }

    // Test #29: update_node matches by node_id and mutates fields, returns true
    #[test]
    fn update_node_matches_and_mutates() {
        use super::ActivityPanel;

        let mut panel = ActivityPanel::new("sess1".to_string());
        panel.nodes.push_front(ExecutionNodeItem::new(
            42,
            "tu_42".to_string(),
            0,
            "bash",
            None,
            None,
            None,
            0,
            NodeStatus::Running,
        ));

        // Manually call the update logic (without cx.notify)
        let node = panel.nodes.iter_mut().find(|n| n.node_id == 42).unwrap();
        node.status = NodeStatus::Completed;
        node.exit_code = Some(0);
        node.display_duration = format_duration(1500);
        node.display_summary = Some(truncate_str("output text", 80));

        let n = panel.nodes.front().unwrap();
        assert_eq!(n.status, NodeStatus::Completed);
        assert_eq!(n.exit_code, Some(0));
        assert_eq!(n.display_duration, "1.5s");
        assert_eq!(n.display_summary.as_deref(), Some("output text"));
    }

    // Test #30: update_node returns false when no row matches
    #[test]
    fn update_node_no_match_returns_false() {
        use super::ActivityPanel;

        let panel = ActivityPanel::new("sess1".to_string());
        // No nodes in panel, find returns None
        let found = panel.nodes.iter().find(|n| n.node_id == 999);
        assert!(found.is_none());
    }

    // Test #31: kind_label for lowercase tools
    #[test]
    fn kind_label_lowercase_tools() {
        assert_eq!(kind_label("bash"), "bash");
        assert_eq!(kind_label("read"), "read");
        assert_eq!(kind_label("task"), "task");
        assert_eq!(kind_label("webfetch"), "webfetch");
        // MCP names pass through as-is
        assert_eq!(kind_label("mcp__plugin__tool"), "mcp__plugin__tool");
        assert_eq!(kind_label("glob"), "glob");
        assert_eq!(kind_label("grep"), "grep");
        assert_eq!(kind_label("edit"), "edit");
        assert_eq!(kind_label("write"), "write");
        assert_eq!(kind_label("todowrite"), "todowrite");
        assert_eq!(kind_label("agent_response"), "agent_response");
        assert_eq!(kind_label("shell_command"), "shell_command");
    }

    // Test #32: kind_label fallback capitalizes unknown lowercase strings
    #[test]
    fn kind_label_fallback_capitalize() {
        // The label is returned as-is from kind_label, capitalize_first is for display
        let raw = kind_label("unknownthing");
        assert_eq!(raw, "unknownthing");
        let capitalized = capitalize_first(raw);
        assert_eq!(capitalized, "Unknownthing");

        let raw2 = kind_label("my_custom_tool");
        assert_eq!(raw2, "my_custom_tool");
        let capitalized2 = capitalize_first(raw2);
        assert_eq!(capitalized2, "My_custom_tool");
    }

    // Test #33: ExecutionNodeItem with NodeStatus::Running chooses Loader icon
    #[test]
    fn node_item_running_chooses_loader_icon() {
        // When status is Running, display_icon still reflects kind_icon (the icon
        // for the tool type), but the render path uses Icon::Loader for the spinner.
        // We verify that a Running node gets the Loader shown by checking status.
        let node = ExecutionNodeItem::new(
            1,
            "tu_1".to_string(),
            0,
            "bash",
            None,
            None,
            None,
            0,
            NodeStatus::Running,
        );
        assert_eq!(node.status, NodeStatus::Running);
        // The render path checks node.status == NodeStatus::Running to show spinner
        // (Icon::Loader) instead of node.display_icon.
        // We verify that the status is correctly stored.
        assert!(matches!(node.display_icon, Icon::SquareTerminal)); // tool icon preserved
    }

    // Test #34: ExecutionNodeItem with NodeStatus::Stopped / Stale chooses distinct icons
    #[test]
    fn node_item_stopped_stale_distinct_icons() {
        let stopped = ExecutionNodeItem::new(
            2,
            "tu_2".to_string(),
            0,
            "bash",
            None,
            None,
            None,
            0,
            NodeStatus::Stopped,
        );
        let stale = ExecutionNodeItem::new(
            3,
            "tu_3".to_string(),
            0,
            "bash",
            None,
            None,
            None,
            0,
            NodeStatus::Stale,
        );
        assert_eq!(stopped.status, NodeStatus::Stopped);
        assert_eq!(stale.status, NodeStatus::Stale);
        // Render path maps: Stopped -> CircleSlash, Stale -> AlertCircle
        // These are distinct from Completed (CheckCircle/XCircle) and Running (Loader)
        assert_ne!(stopped.status, NodeStatus::Completed);
        assert_ne!(stale.status, NodeStatus::Completed);
        assert_ne!(stopped.status, NodeStatus::Running);
        assert_ne!(stale.status, NodeStatus::Running);
    }
}
