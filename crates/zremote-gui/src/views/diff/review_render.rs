//! Review drawer + composer overlay render helpers.
//!
//! Extends `DiffView` (in `mod.rs`) with the render helpers that paint the
//! review drawer, the composer overlay, and the local tooltip component.
//! Keeping these here lets `mod.rs` stay a thin orchestrator.

use gpui::*;

use super::DiffView;
use crate::theme;

impl DiffView {
    pub(super) fn render_review_drawer(&self) -> impl IntoElement {
        // Always render the panel entity — it chooses pill vs. expanded
        // internally based on its `state.expanded` flag.
        self.review_panel.clone()
    }

    pub(super) fn render_composer_overlay(&self) -> Option<AnyElement> {
        let composer = self.active_composer.as_ref()?.clone();
        Some(
            div()
                .flex()
                .flex_col()
                .items_center()
                .px(px(12.0))
                .py(px(8.0))
                .bg(theme::modal_backdrop())
                .border_t_1()
                .border_color(theme::border())
                .child(div().max_w(px(720.0)).w_full().child(composer))
                .into_any_element(),
        )
    }
}

/// Local text tooltip used by the close button. Duplicated intentionally from
/// `sidebar::SidebarTextTooltip` since that type is private to the sidebar
/// module; a shared component would require wider refactoring out of P3 scope.
pub(super) struct DiffTextTooltip(pub String);

impl Render for DiffTextTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(theme::bg_tertiary())
            .border_1()
            .border_color(theme::border())
            .text_size(px(11.0))
            .text_color(theme::text_secondary())
            .child(self.0.clone())
    }
}
