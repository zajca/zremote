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
    pub persistent: bool,
    pub created_at: Instant,
    pub context: ToastContext,
}

impl Toast {
    /// How long this toast should remain visible before auto-dismissing.
    pub fn ttl(&self) -> Duration {
        if self.persistent {
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

const MAX_VISIBLE: usize = 5;

/// Container entity that manages a stack of toast notifications.
pub struct ToastContainer {
    toasts: Vec<Toast>,
    next_id: u64,
}

impl ToastContainer {
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            next_id: 0,
        }
    }

    /// Add a new toast notification.
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
            persistent: false,
            created_at: Instant::now(),
            context,
        });
        // Keep only the most recent MAX_VISIBLE toasts
        if self.toasts.len() > MAX_VISIBLE {
            self.toasts.remove(0);
        }
    }

    /// Add a new toast with action buttons and optional persistence.
    pub fn push_actionable(
        &mut self,
        message: impl Into<String>,
        level: ToastLevel,
        toast_icon: Option<Icon>,
        actions: Vec<ToastAction>,
        persistent: bool,
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
            persistent,
            created_at: Instant::now(),
            context,
        });
        if self.toasts.len() > MAX_VISIBLE {
            self.toasts.remove(0);
        }
        id
    }

    /// Dismiss a toast by its id.
    pub fn dismiss(&mut self, id: u64) {
        self.toasts.retain(|t| t.id != id);
    }

    /// Remove expired toasts. Returns `true` if any were removed.
    pub fn tick(&mut self) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.created_at.elapsed() < t.ttl());
        self.toasts.len() != before
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

        for toast in &self.toasts {
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
            if toast.persistent {
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
                let toast_el = base
                    .id(("toast-nav", toast_id))
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
                    }));
                container = container.child(toast_el);
            } else {
                container = container.child(base.child(inner));
            }
        }

        container
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
}
