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

/// A single toast notification.
pub struct Toast {
    pub id: u64,
    pub message: String,
    pub level: ToastLevel,
    pub icon: Option<Icon>,
    pub created_at: Instant,
}

impl Toast {
    /// How long this toast should remain visible before auto-dismissing.
    pub fn ttl(&self) -> Duration {
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
    ) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.toasts.push(Toast {
            id,
            message: message.into(),
            level,
            icon: toast_icon,
            created_at: Instant::now(),
        });
        // Keep only the most recent MAX_VISIBLE toasts
        if self.toasts.len() > MAX_VISIBLE {
            self.toasts.remove(0);
        }
    }

    /// Remove expired toasts. Returns `true` if any were removed.
    pub fn tick(&mut self) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.created_at.elapsed() < t.ttl());
        self.toasts.len() != before
    }
}

impl Render for ToastContainer {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
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

            container = container.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(6.0))
                    .bg(theme::bg_tertiary())
                    .border_l(px(3.0))
                    .border_color(border)
                    .child(
                        icon(icon_to_use)
                            .size(px(14.0))
                            .flex_shrink_0()
                            .text_color(icon_color),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_primary())
                            .child(toast.message.clone()),
                    ),
            );
        }

        container
    }
}
