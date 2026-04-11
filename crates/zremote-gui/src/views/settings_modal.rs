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
use crate::views::settings::agent_profiles_tab::{AgentProfilesTab, AgentProfilesTabEvent};
use zremote_client::{AgentKindInfo, AgentProfile};

/// Which tab is currently active in the settings modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    AgentProfiles,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            Self::AgentProfiles => "Agent Profiles",
        }
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
        // Delegate focus to the active tab so its keyboard routing receives
        // printable chars for the text fields. Only take focus if nothing in
        // the modal's focus tree currently holds it -- otherwise we would
        // steal focus back from the tab every frame and break typing.
        //
        // FIXME: when a second tab is introduced, this check must inspect
        // **every** tab's focus handle (not just the active one). Otherwise,
        // switching tabs while an inactive tab's input still has focus will
        // cause the active tab to steal it on the next frame, breaking typing
        // in the background tab. Today only `AgentProfiles` exists, so the
        // single-tab check is sufficient.
        let tab_focus = self.agent_profiles_tab.read(cx).focus_handle(cx);
        if !tab_focus.contains_focused(window, cx)
            && !self.focus_handle.contains_focused(window, cx)
        {
            tab_focus.focus(window);
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
                div()
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .child(self.render_tab_button(SettingsTab::AgentProfiles, cx)),
            );

        // `min_h_0` on the body is the first link in a chain that lets the
        // profile editor's scrollable form clip against the modal's fixed
        // 560px height. Without it, the body grows to fit its child's
        // intrinsic height and the scroll region never gets a bounded frame.
        let body = match self.active_tab {
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
                if event.keystroke.key.as_str() == "escape" {
                    cx.emit(SettingsModalEvent::Close);
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .child(body)
    }
}
