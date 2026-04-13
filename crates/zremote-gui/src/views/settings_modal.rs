#![allow(clippy::wildcard_imports)]

//! Top-level Settings modal for the GUI.
//!
//! Mirrors `help_modal.rs` architecturally: a struct with a focus handle, an
//! `Event` enum for parent communication, and a `Render` impl that draws a
//! tabbed container. Today the modal only hosts the Agent Profiles tab, but
//! the tab-bar scaffolding is in place so future phases can add more tabs
//! (appearance, notifications, advanced) without reshaping this file.

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::key_bindings::{KeyAction, dispatch_modal_key};
use crate::views::settings::agent_profiles_tab::{AgentProfilesTab, AgentProfilesTabEvent};
use zremote_client::{AgentKindInfo, AgentProfile};

/// Which tab is currently active in the settings modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    AgentProfiles,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::AgentProfiles => "Agent Profiles",
        }
    }

    fn all() -> &'static [Self] {
        &[Self::General, Self::AgentProfiles]
    }
}

pub struct SettingsModal {
    focus_handle: FocusHandle,
    active_tab: SettingsTab,
    agent_profiles_tab: Entity<AgentProfilesTab>,
}

/// Events emitted by the settings modal.
pub enum SettingsModalEvent {
    /// Dismiss the modal (Escape / backdrop click).
    Close,
    /// A CRUD mutation succeeded in one of the tabs. `MainView` re-fetches
    /// the shared sidebar caches in response.
    ProfilesChanged,
    /// User clicked "Clear Recent Actions" in the General tab.
    ClearRecentActions,
}

impl EventEmitter<SettingsModalEvent> for SettingsModal {}

impl SettingsModal {
    pub fn new(
        app_state: Arc<AppState>,
        profiles: Rc<Vec<AgentProfile>>,
        kinds: Rc<Vec<AgentKindInfo>>,
        initial_tab: SettingsTab,
        cx: &mut Context<Self>,
    ) -> Self {
        let agent_profiles_tab = cx.new(|cx| AgentProfilesTab::new(app_state, profiles, kinds, cx));

        // Bubble tab events up to SettingsModal subscribers.
        cx.subscribe(
            &agent_profiles_tab,
            |_this, _entity, event: &AgentProfilesTabEvent, cx| match event {
                AgentProfilesTabEvent::ProfilesChanged => {
                    cx.emit(SettingsModalEvent::ProfilesChanged);
                }
            },
        )
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            active_tab: initial_tab,
            agent_profiles_tab,
        }
    }

    /// Push fresh profile/kind snapshots into the active tab. Called from
    /// `MainView::render` on every frame so the modal stays in sync with
    /// the sidebar cache after CRUD refreshes.
    pub fn set_profiles(
        &mut self,
        profiles: Rc<Vec<AgentProfile>>,
        kinds: Rc<Vec<AgentKindInfo>>,
        cx: &mut Context<Self>,
    ) {
        self.agent_profiles_tab.update(cx, |tab, cx| {
            tab.set_profiles(profiles, kinds, cx);
        });
    }

    fn render_general_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_primary())
                            .child("Command Palette"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_secondary())
                            .child(
                                "Recently used actions are tracked and boosted in palette results.",
                            ),
                    )
                    .child(
                        div()
                            .id("clear-recent-actions")
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(4.0))
                            .bg(theme::bg_tertiary())
                            .hover(|s| s.bg(theme::border()))
                            .child(
                                icon(Icon::Clock)
                                    .size(px(14.0))
                                    .text_color(theme::text_secondary()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme::text_primary())
                                    .child("Clear Recent Actions"),
                            )
                            .on_click(cx.listener(|_this, _: &ClickEvent, _window, cx| {
                                cx.emit(SettingsModalEvent::ClearRecentActions);
                            })),
                    ),
            )
    }

    fn render_tab_button(&self, tab: SettingsTab, cx: &mut Context<Self>) -> impl IntoElement {
        let is_active = self.active_tab == tab;
        let id = SharedString::from(format!("settings-tab-{}", tab.label()));
        div()
            .id(id)
            .px(px(12.0))
            .py(px(6.0))
            .cursor_pointer()
            .text_size(px(12.0))
            .when(is_active, |s| {
                s.bg(theme::bg_tertiary())
                    .text_color(theme::text_primary())
                    .border_b_1()
                    .border_color(theme::accent())
            })
            .when(!is_active, |s| {
                s.text_color(theme::text_secondary())
                    .hover(|h| h.text_color(theme::text_primary()))
            })
            .child(tab.label())
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.active_tab = tab;
                cx.notify();
            }))
    }
}

impl Focusable for SettingsModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Delegate focus to the active tab's focus handle so keyboard routing
        // reaches text fields. For tabs without their own focus (General), the
        // modal keeps focus itself.
        match self.active_tab {
            SettingsTab::AgentProfiles => {
                let tab_focus = self.agent_profiles_tab.read(cx).focus_handle(cx);
                if !tab_focus.contains_focused(window, cx)
                    && !self.focus_handle.contains_focused(window, cx)
                {
                    tab_focus.focus(window);
                }
            }
            SettingsTab::General => {
                if !self.focus_handle.contains_focused(window, cx) {
                    self.focus_handle.focus(window);
                }
            }
        }

        let header = div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .px(px(16.0))
                    .py(px(10.0))
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        icon(Icon::Settings)
                            .size(px(14.0))
                            .text_color(theme::text_secondary()),
                    )
                    .child("Settings"),
            )
            .child(
                div().flex().items_center().px(px(8.0)).children(
                    SettingsTab::all()
                        .iter()
                        .map(|&tab| self.render_tab_button(tab, cx).into_any_element()),
                ),
            );

        // `min_h_0` on the body is the first link in a chain that lets the
        // profile editor's scrollable form clip against the modal's fixed
        // 560px height. Without it, the body grows to fit its child's
        // intrinsic height and the scroll region never gets a bounded frame.
        let body = match self.active_tab {
            SettingsTab::General => self.render_general_tab(cx).into_any_element(),
            SettingsTab::AgentProfiles => div()
                .flex_1()
                .min_h_0()
                .child(self.agent_profiles_tab.clone())
                .into_any_element(),
        };

        div()
            .id("settings-modal")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .on_key_down(cx.listener(|_this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                let mods = &event.keystroke.modifiers;
                if let Some(KeyAction::CloseOverlay) =
                    dispatch_modal_key(key, mods.control, mods.shift, mods.alt)
                {
                    cx.emit(SettingsModalEvent::Close);
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .child(body)
    }
}
