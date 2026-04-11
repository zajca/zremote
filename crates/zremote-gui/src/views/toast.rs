use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::*;

use crate::icons::{Icon, icon};
use crate::theme;

/// Severity level for a toast notification.
#[derive(Debug, Clone, Copy)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// Classification of a toast that determines persistence and display priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Normal auto-dismiss toast.
    Transient,
    /// Persistent: Claude Code is waiting for user input (lower priority).
    WaitingForInput,
    /// Persistent: Claude Code needs explicit permission (highest priority).
    RequiresAction,
}

impl ToastKind {
    pub fn is_persistent(self) -> bool {
        self != Self::Transient
    }

    /// Sorting key: higher = more urgent. Used for priority ordering.
    fn priority(self) -> u8 {
        match self {
            Self::Transient => 0,
            Self::WaitingForInput => 1,
            Self::RequiresAction => 2,
        }
    }
}

/// Contextual metadata displayed in a toast subtitle and used for click-to-navigate.
#[derive(Debug, Clone, Default)]
pub struct ToastContext {
    /// Human-readable host name.
    pub host_name: Option<String>,
    /// Human-readable project name.
    pub project_name: Option<String>,
    /// Session display name.
    pub session_name: Option<String>,
    /// Task name from agentic loop.
    pub task_name: Option<String>,
    /// Session ID for click-to-navigate.
    pub session_id: Option<String>,
    /// Host ID for click-to-navigate.
    pub host_id: Option<String>,
}

impl ToastContext {
    /// Format a compact subtitle string for display.
    pub fn subtitle(&self) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(h) = &self.host_name {
            parts.push(h.as_str());
        }
        if let Some(p) = &self.project_name {
            parts.push(p.as_str());
        }
        if let Some(s) = &self.session_name {
            parts.push(s.as_str());
        }
        if let Some(t) = &self.task_name {
            parts.push(t.as_str());
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" / "))
        }
    }

    /// Whether this toast can navigate to a session on click.
    pub fn is_navigable(&self) -> bool {
        self.session_id.is_some() && self.host_id.is_some()
    }
}

/// Event emitted by [`ToastContainer`] when a navigable toast is clicked.
#[derive(Debug)]
pub enum ToastContainerEvent {
    Navigate { session_id: String, host_id: String },
}

impl EventEmitter<ToastContainerEvent> for ToastContainer {}

type ActionCallback = Rc<Cell<Option<Box<dyn FnOnce(&mut Window, &mut App)>>>>;

/// An action button that can be attached to a toast.
pub struct ToastAction {
    pub label: String,
    pub icon: Option<Icon>,
    callback: ActionCallback,
}

impl ToastAction {
    pub fn new(
        label: impl Into<String>,
        icon: Option<Icon>,
        callback: impl FnOnce(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            icon,
            callback: Rc::new(Cell::new(Some(Box::new(callback)))),
        }
    }
}

/// A single toast notification.
pub struct Toast {
    pub id: u64,
    pub message: String,
    pub level: ToastLevel,
    pub icon: Option<Icon>,
    pub actions: Vec<ToastAction>,
    pub kind: ToastKind,
    pub created_at: Instant,
    pub context: ToastContext,
}

impl Toast {
    /// Whether this toast stays until explicitly dismissed.
    pub fn is_persistent(&self) -> bool {
        self.kind.is_persistent()
    }

    /// How long this toast should remain visible before auto-dismissing.
    pub fn ttl(&self) -> Duration {
        if self.is_persistent() {
            return Duration::from_secs(86400);
        }
        match self.level {
            ToastLevel::Error | ToastLevel::Warning => Duration::from_secs(8),
            ToastLevel::Info | ToastLevel::Success => Duration::from_secs(4),
        }
    }

    fn border_color(&self) -> Rgba {
        match self.level {
            ToastLevel::Info => theme::accent(),
            ToastLevel::Success => theme::success(),
            ToastLevel::Warning => theme::warning(),
            ToastLevel::Error => theme::error(),
        }
    }

    fn default_icon(&self) -> Icon {
        match self.level {
            ToastLevel::Info => Icon::Info,
            ToastLevel::Success => Icon::CheckCircle,
            ToastLevel::Warning => Icon::AlertTriangle,
            ToastLevel::Error => Icon::XCircle,
        }
    }
}

/// Max persistent toasts shown inline before collapsing.
const MAX_VISIBLE_PERSISTENT: usize = 2;
/// Max transient toasts shown simultaneously.
const MAX_VISIBLE_TRANSIENT: usize = 3;
/// Safety cap to prevent unbounded memory growth.
const MAX_TOTAL: usize = 50;

/// Container entity that manages a stack of toast notifications.
pub struct ToastContainer {
    toasts: Vec<Toast>,
    next_id: u64,
    expanded: bool,
}

impl ToastContainer {
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            next_id: 0,
            expanded: false,
        }
    }

    /// Add a new toast notification (transient).
    pub fn push(
        &mut self,
        message: impl Into<String>,
        level: ToastLevel,
        toast_icon: Option<Icon>,
        context: ToastContext,
    ) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.toasts.push(Toast {
            id,
            message: message.into(),
            level,
            icon: toast_icon,
            actions: Vec::new(),
            kind: ToastKind::Transient,
            created_at: Instant::now(),
            context,
        });
        self.enforce_cap();
    }

    /// Add a new toast with action buttons and a classification kind.
    pub fn push_actionable(
        &mut self,
        message: impl Into<String>,
        level: ToastLevel,
        toast_icon: Option<Icon>,
        actions: Vec<ToastAction>,
        kind: ToastKind,
        context: ToastContext,
    ) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.toasts.push(Toast {
            id,
            message: message.into(),
            level,
            icon: toast_icon,
            actions,
            kind,
            created_at: Instant::now(),
            context,
        });
        self.enforce_cap();
        id
    }

    /// Dismiss a toast by its id.
    pub fn dismiss(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
        // Auto-collapse when few enough persistent toasts remain visible.
        if self.persistent_count() <= MAX_VISIBLE_PERSISTENT {
            self.expanded = false;
        }
    }

    /// Remove expired toasts. Returns `true` if any were removed.
    pub fn tick(&mut self) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.created_at.elapsed() < t.ttl());
        if self.persistent_count() <= MAX_VISIBLE_PERSISTENT {
            self.expanded = false;
        }
        self.toasts.len() != before
    }

    fn persistent_count(&self) -> usize {
        self.toasts.iter().filter(|t| t.is_persistent()).count()
    }

    fn hidden_persistent_count(&self) -> usize {
        self.persistent_count()
            .saturating_sub(MAX_VISIBLE_PERSISTENT)
    }

    /// Indices of persistent toasts sorted by priority (RequiresAction first), then newest first.
    fn sorted_persistent_indices(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = self
            .toasts
            .iter()
            .enumerate()
            .filter(|(_, t)| t.is_persistent())
            .map(|(i, _)| i)
            .collect();
        indices.sort_by(|&a, &b| {
            let ta = &self.toasts[a];
            let tb = &self.toasts[b];
            // Higher priority first, then newer first.
            tb.kind
                .priority()
                .cmp(&ta.kind.priority())
                .then(tb.created_at.cmp(&ta.created_at))
        });
        indices
    }

    /// Remove oldest transient toast if over the safety cap.
    fn enforce_cap(&mut self) {
        while self.toasts.len() > MAX_TOTAL {
            // Remove oldest transient first; if none, remove oldest overall.
            if let Some(pos) = self.toasts.iter().position(|t| !t.is_persistent()) {
                self.toasts.remove(pos);
            } else {
                self.toasts.remove(0);
            }
        }
    }
}

impl Render for ToastContainer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut container = div()
            .absolute()
            .bottom(px(16.0))
            .right(px(16.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .max_w(px(360.0));

        // --- Transient toasts (top section) ---
        let transient_indices: Vec<usize> = self
            .toasts
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.is_persistent())
            .map(|(i, _)| i)
            .collect();
        let transient_start = transient_indices
            .len()
            .saturating_sub(MAX_VISIBLE_TRANSIENT);
        for &idx in &transient_indices[transient_start..] {
            container = container.child(self.render_toast(idx, cx));
        }

        // --- Persistent toasts (bottom section) ---
        let sorted_persistent = self.sorted_persistent_indices();
        let hidden_count = self.hidden_persistent_count();

        if self.expanded && hidden_count > 0 {
            // Expanded mode: scrollable panel with all persistent toasts + collapse bar.
            let mut panel = div()
                .id("toast-expanded-panel")
                .flex()
                .flex_col()
                .gap(px(6.0))
                .max_h(px(400.0))
                .overflow_y_scroll();

            // Collapse bar at top
            panel = panel.child(
                div()
                    .id("toast-collapse")
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .px(px(12.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .bg(theme::bg_secondary())
                    .hover(|s| s.bg(theme::border()))
                    .text_size(px(11.0))
                    .text_color(theme::text_secondary())
                    .child(
                        icon(Icon::ChevronDown)
                            .size(px(12.0))
                            .text_color(theme::text_secondary()),
                    )
                    .child("Collapse")
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.expanded = false;
                        cx.notify();
                    })),
            );

            for &idx in &sorted_persistent {
                panel = panel.child(self.render_toast(idx, cx));
            }

            container = container.child(panel);
        } else {
            // Collapsed mode: show top MAX_VISIBLE_PERSISTENT persistent toasts.
            let visible_count = sorted_persistent.len().min(MAX_VISIBLE_PERSISTENT);
            for &idx in &sorted_persistent[..visible_count] {
                container = container.child(self.render_toast(idx, cx));
            }

            // "+N more" summary bar
            if hidden_count > 0 {
                let label = if hidden_count == 1 {
                    "+1 session needs attention".to_string()
                } else {
                    format!("+{hidden_count} sessions need attention")
                };
                container = container.child(
                    div()
                        .id("toast-expand")
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .rounded(px(6.0))
                        .bg(theme::bg_tertiary())
                        .border_l(px(3.0))
                        .border_color(theme::warning())
                        .hover(|s| s.bg(theme::bg_secondary()))
                        .text_size(px(12.0))
                        .text_color(theme::text_secondary())
                        .child(
                            icon(Icon::MessageCircle)
                                .size(px(14.0))
                                .flex_shrink_0()
                                .text_color(theme::warning()),
                        )
                        .child(div().flex_1().child(label))
                        .child(
                            icon(Icon::ChevronUp)
                                .size(px(12.0))
                                .text_color(theme::text_tertiary()),
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.expanded = true;
                            cx.notify();
                        })),
                );
            }
        }

        container
    }
}

impl ToastContainer {
    /// Render a single toast card by index.
    fn render_toast(&self, idx: usize, cx: &mut Context<Self>) -> AnyElement {
        let toast = &self.toasts[idx];
        let icon_to_use = toast.icon.unwrap_or_else(|| toast.default_icon());
        let border = toast.border_color();
        let icon_color = border;
        let toast_id = toast.id;

        // Build the header row: icon + message + optional close button
        let mut header_row = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .w_full()
            .child(
                icon(icon_to_use)
                    .size(px(14.0))
                    .flex_shrink_0()
                    .text_color(icon_color),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.0))
                    .text_color(theme::text_primary())
                    .child(toast.message.clone()),
            );

        // Close button for persistent toasts
        if toast.is_persistent() {
            header_row = header_row.child(
                div()
                    .id(("toast-close", toast_id))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .rounded(px(4.0))
                    .p(px(2.0))
                    .hover(|s| s.bg(theme::bg_secondary()))
                    .child(
                        icon(Icon::X)
                            .size(px(12.0))
                            .text_color(theme::text_secondary()),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.dismiss(toast_id);
                        cx.notify();
                    })),
            );
        }

        // Build the action buttons row
        let mut actions_row: Option<Div> = None;
        if !toast.actions.is_empty() {
            let mut row = div().flex().gap(px(6.0)).pl(px(22.0)); // align with message text (14px icon + 8px gap)

            for (i, action) in toast.actions.iter().enumerate() {
                let cb = Rc::clone(&action.callback);
                let action_toast_id = toast_id;
                let is_primary = i == 0;

                let mut btn = div()
                    .id((
                        "toast-action",
                        toast_id.wrapping_mul(100).wrapping_add(i as u64),
                    ))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .py(px(4.0))
                    .rounded(px(4.0))
                    .text_size(px(11.0));

                if is_primary {
                    btn = btn
                        .bg(theme::accent())
                        .text_color(theme::text_primary())
                        .hover(|s| s.bg(theme::bg_secondary()));
                } else {
                    btn = btn
                        .bg(theme::bg_secondary())
                        .text_color(theme::text_secondary())
                        .hover(|s| s.bg(theme::border()));
                }

                if let Some(action_icon) = action.icon {
                    btn = btn.child(icon(action_icon).size(px(12.0)).flex_shrink_0());
                }

                btn = btn.child(action.label.clone());

                btn = btn.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    // Stop propagation so the nav handler on the parent toast is not triggered.
                    cx.stop_propagation();
                    if let Some(f) = cb.take() {
                        f(window, &mut *cx);
                    }
                    this.dismiss(action_toast_id);
                    cx.notify();
                }));

                row = row.child(btn);
            }

            actions_row = Some(row);
        }

        // Build inner content for the toast
        let mut inner = div().flex().flex_col().gap(px(6.0));

        inner = inner.child(header_row);

        // Subtitle line below message (host / project / session / task context)
        if let Some(subtitle) = toast.context.subtitle() {
            inner = inner.child(
                div()
                    .pl(px(22.0)) // align with message text (14px icon + 8px gap)
                    .text_size(px(10.0))
                    .text_color(theme::text_tertiary())
                    .overflow_x_hidden()
                    .text_ellipsis()
                    .child(subtitle),
            );
        }

        if let Some(row) = actions_row {
            inner = inner.child(row);
        }

        // Shared toast card styling
        let base = div()
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(6.0))
            .bg(theme::bg_tertiary())
            .border_l(px(3.0))
            .border_color(border);

        // Click-to-navigate for toasts with session context
        if let (Some(sid), Some(hid)) = (
            toast.context.session_id.clone(),
            toast.context.host_id.clone(),
        ) {
            base.id(("toast-nav", toast_id))
                .cursor_pointer()
                .hover(|s| s.bg(theme::bg_secondary()))
                .child(inner)
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.emit(ToastContainerEvent::Navigate {
                        session_id: sid.clone(),
                        host_id: hid.clone(),
                    });
                    this.dismiss(toast_id);
                    cx.notify();
                }))
                .into_any_element()
        } else {
            base.child(inner).into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use fully qualified `test` to avoid conflict with gpui::test from wildcard import.
    #[core::prelude::rust_2021::test]
    fn subtitle_all_fields() {
        let ctx = ToastContext {
            host_name: Some("server1".into()),
            project_name: Some("myproject".into()),
            session_name: Some("main-shell".into()),
            task_name: Some("fix bug".into()),
            ..Default::default()
        };
        assert_eq!(
            ctx.subtitle().as_deref(),
            Some("server1 / myproject / main-shell / fix bug")
        );
    }

    #[core::prelude::rust_2021::test]
    fn subtitle_host_and_project_only() {
        let ctx = ToastContext {
            host_name: Some("server1".into()),
            project_name: Some("myproject".into()),
            ..Default::default()
        };
        assert_eq!(ctx.subtitle().as_deref(), Some("server1 / myproject"));
    }

    #[core::prelude::rust_2021::test]
    fn subtitle_host_only() {
        let ctx = ToastContext {
            host_name: Some("server1".into()),
            ..Default::default()
        };
        assert_eq!(ctx.subtitle().as_deref(), Some("server1"));
    }

    #[core::prelude::rust_2021::test]
    fn subtitle_none_when_empty() {
        let ctx = ToastContext::default();
        assert!(ctx.subtitle().is_none());
    }

    #[core::prelude::rust_2021::test]
    fn navigable_when_both_ids_present() {
        let ctx = ToastContext {
            session_id: Some("s1".into()),
            host_id: Some("h1".into()),
            ..Default::default()
        };
        assert!(ctx.is_navigable());
    }

    #[core::prelude::rust_2021::test]
    fn not_navigable_when_missing_session() {
        let ctx = ToastContext {
            host_id: Some("h1".into()),
            ..Default::default()
        };
        assert!(!ctx.is_navigable());
    }

    #[core::prelude::rust_2021::test]
    fn not_navigable_when_missing_host() {
        let ctx = ToastContext {
            session_id: Some("s1".into()),
            ..Default::default()
        };
        assert!(!ctx.is_navigable());
    }

    #[core::prelude::rust_2021::test]
    fn not_navigable_when_empty() {
        assert!(!ToastContext::default().is_navigable());
    }

    #[core::prelude::rust_2021::test]
    fn persistent_toasts_priority_ordering() {
        let mut container = ToastContainer::new();
        container.push_actionable(
            "waiting 1",
            ToastLevel::Warning,
            None,
            vec![],
            ToastKind::WaitingForInput,
            ToastContext::default(),
        );
        container.push_actionable(
            "requires action",
            ToastLevel::Warning,
            None,
            vec![],
            ToastKind::RequiresAction,
            ToastContext::default(),
        );
        container.push_actionable(
            "waiting 2",
            ToastLevel::Warning,
            None,
            vec![],
            ToastKind::WaitingForInput,
            ToastContext::default(),
        );

        let indices = container.sorted_persistent_indices();
        assert_eq!(indices.len(), 3);
        // RequiresAction should be first.
        assert_eq!(container.toasts[indices[0]].kind, ToastKind::RequiresAction);
    }

    #[core::prelude::rust_2021::test]
    fn hidden_persistent_count_calculation() {
        let mut container = ToastContainer::new();
        // 0 persistent -> 0 hidden
        assert_eq!(container.hidden_persistent_count(), 0);

        // 2 persistent -> 0 hidden
        for _ in 0..2 {
            container.push_actionable(
                "msg",
                ToastLevel::Warning,
                None,
                vec![],
                ToastKind::WaitingForInput,
                ToastContext::default(),
            );
        }
        assert_eq!(container.hidden_persistent_count(), 0);

        // 5 persistent -> 3 hidden
        for _ in 0..3 {
            container.push_actionable(
                "msg",
                ToastLevel::Warning,
                None,
                vec![],
                ToastKind::WaitingForInput,
                ToastContext::default(),
            );
        }
        assert_eq!(container.hidden_persistent_count(), 3);
    }

    #[core::prelude::rust_2021::test]
    fn auto_collapse_on_dismiss() {
        let mut container = ToastContainer::new();
        let id1 = container.push_actionable(
            "msg1",
            ToastLevel::Warning,
            None,
            vec![],
            ToastKind::WaitingForInput,
            ToastContext::default(),
        );
        let _id2 = container.push_actionable(
            "msg2",
            ToastLevel::Warning,
            None,
            vec![],
            ToastKind::WaitingForInput,
            ToastContext::default(),
        );
        let _id3 = container.push_actionable(
            "msg3",
            ToastLevel::Warning,
            None,
            vec![],
            ToastKind::RequiresAction,
            ToastContext::default(),
        );

        container.expanded = true;
        assert!(container.expanded);

        // Dismiss one: still 2 persistent -> auto-collapse.
        container.dismiss(id1);
        assert!(!container.expanded);
    }

    #[core::prelude::rust_2021::test]
    fn max_total_safety_cap() {
        let mut container = ToastContainer::new();
        for i in 0..MAX_TOTAL + 10 {
            container.push(
                format!("msg {i}"),
                ToastLevel::Info,
                None,
                ToastContext::default(),
            );
        }
        assert!(container.toasts.len() <= MAX_TOTAL);
    }

    #[core::prelude::rust_2021::test]
    fn toast_kind_determines_persistence() {
        let mut container = ToastContainer::new();
        container.push("transient", ToastLevel::Info, None, ToastContext::default());
        container.push_actionable(
            "waiting",
            ToastLevel::Warning,
            None,
            vec![],
            ToastKind::WaitingForInput,
            ToastContext::default(),
        );
        container.push_actionable(
            "action",
            ToastLevel::Warning,
            None,
            vec![],
            ToastKind::RequiresAction,
            ToastContext::default(),
        );

        assert!(!container.toasts[0].is_persistent());
        assert!(container.toasts[1].is_persistent());
        assert!(container.toasts[2].is_persistent());
    }

    #[core::prelude::rust_2021::test]
    fn tick_auto_collapses_when_persistent_drops() {
        let mut container = ToastContainer::new();
        // Push 3 persistent toasts with zero TTL (they'll expire immediately).
        for _ in 0..3 {
            container.push_actionable(
                "msg",
                ToastLevel::Warning,
                None,
                vec![],
                ToastKind::WaitingForInput,
                ToastContext::default(),
            );
        }
        container.expanded = true;

        // Override created_at to force expiry on one toast.
        container.toasts[0].created_at = Instant::now()
            .checked_sub(Duration::from_secs(86401))
            .expect("Instant supports 86401s subtraction");

        let removed = container.tick();
        assert!(removed);
        // 2 remaining <= MAX_VISIBLE_PERSISTENT -> auto-collapse.
        assert!(!container.expanded);
    }

    #[core::prelude::rust_2021::test]
    fn transient_toasts_not_counted_as_persistent() {
        let mut container = ToastContainer::new();
        for _ in 0..5 {
            container.push("msg", ToastLevel::Info, None, ToastContext::default());
        }
        assert_eq!(container.persistent_count(), 0);
        assert_eq!(container.hidden_persistent_count(), 0);
    }
}
