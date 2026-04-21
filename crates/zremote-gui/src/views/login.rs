#![allow(clippy::wildcard_imports)]

//! Login screen: admin token field + optional SSO/OIDC button.
//!
//! Emits `LoginEvent::LoggedIn(SessionEntry)` on successful authentication.
//! The OIDC button is hidden unless the server reports `oidc_status.configured = true`.

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::app_state::AppState;
use crate::auth_state::SessionEntry;
use crate::icons::{Icon, icon};
use crate::theme;
use zremote_client::SessionTokenResponse;

/// Event emitted when the user successfully authenticates.
#[derive(Debug, Clone)]
pub enum LoginEvent {
    LoggedIn(SessionEntry),
}

impl EventEmitter<LoginEvent> for LoginView {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loading {
    Idle,
    Token,
    Oidc,
}

pub struct LoginView {
    app_state: Arc<AppState>,
    focus_handle: FocusHandle,

    token_input: String,
    loading: Loading,
    error: Option<String>,

    /// `None` = probe in flight, `Some(false)` = not configured, `Some(true)` = configured.
    oidc_configured: Option<bool>,
    issuer_domain: Option<String>,

    oidc_probe_task: Option<Task<()>>,
    login_task: Option<Task<()>>,
}

impl LoginView {
    pub fn new(app_state: Arc<AppState>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            app_state,
            focus_handle: cx.focus_handle(),
            token_input: String::new(),
            loading: Loading::Idle,
            error: None,
            oidc_configured: None,
            issuer_domain: None,
            oidc_probe_task: None,
            login_task: None,
        };
        this.oidc_probe_task = Some(this.spawn_oidc_probe(cx));
        this
    }

    fn spawn_oidc_probe(&self, cx: &mut Context<Self>) -> Task<()> {
        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = handle.spawn(async move { api.oidc_status().await }).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(status)) => {
                        this.oidc_configured = Some(status.configured);
                        this.issuer_domain = status.issuer;
                    }
                    _ => {
                        this.oidc_configured = Some(false);
                    }
                }
                cx.notify();
            });
        })
    }

    fn submit_admin_token(&mut self, cx: &mut Context<Self>) {
        let token = self.token_input.trim().to_string();
        if token.is_empty() || self.loading != Loading::Idle {
            return;
        }
        self.loading = Loading::Token;
        self.error = None;
        cx.notify();

        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        self.login_task = Some(
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let result = handle
                    .spawn(async move { api.login_admin_token(token).await })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.loading = Loading::Idle;
                    match result {
                        Ok(Ok(resp)) => {
                            cx.emit(LoginEvent::LoggedIn(session_entry_from_response(resp)));
                        }
                        Ok(Err(err)) => {
                            this.error = Some(format!("Login failed: {err}"));
                            cx.notify();
                        }
                        Err(_) => {
                            this.error = Some("Internal error. Please try again.".to_string());
                            cx.notify();
                        }
                    }
                });
            }),
        );
    }

    fn open_oidc_browser(&mut self, cx: &mut Context<Self>) {
        if self.loading != Loading::Idle {
            return;
        }
        self.loading = Loading::Oidc;
        self.error = None;
        cx.notify();

        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        self.login_task = Some(
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                // Phase 4: minimal OIDC — just open the browser at the auth URL.
                // Full loopback callback + token exchange is deferred to Phase 7.
                let result = handle
                    .spawn(async move {
                        api.login_oidc_init("http://127.0.0.1:9876/oidc/callback")
                            .await
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.loading = Loading::Idle;
                    match result {
                        Ok(Ok(init)) => {
                            if let Err(e) = open::that(&init.auth_url) {
                                this.error = Some(format!("Could not open browser: {e}"));
                            }
                            cx.notify();
                        }
                        Ok(Err(err)) => {
                            this.error = Some(format!("OIDC init failed: {err}"));
                            cx.notify();
                        }
                        Err(_) => {
                            this.error = Some("Internal error. Please try again.".to_string());
                            cx.notify();
                        }
                    }
                });
            }),
        );
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        match event.keystroke.key.as_str() {
            "backspace" => {
                self.token_input.pop();
                cx.notify();
                true
            }
            "return" | "enter" => {
                self.submit_admin_token(cx);
                true
            }
            ch => {
                if ch.len() == 1 && !event.keystroke.modifiers.platform {
                    self.token_input.push_str(ch);
                    cx.notify();
                    true
                } else {
                    false
                }
            }
        }
    }

    // ---- render helpers ----

    fn render_token_input(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let bullet_display: String = "\u{2022}".repeat(self.token_input.len());
        let placeholder = div()
            .text_color(theme::text_tertiary())
            .child("Admin token");
        let value_view = if self.token_input.is_empty() {
            placeholder.into_any_element()
        } else {
            div()
                .text_color(theme::text_primary())
                .child(bullet_display)
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_secondary())
                    .child("Admin Token"),
            )
            .child(
                div()
                    .id("login-token-input")
                    .px(px(12.0))
                    .py(px(9.0))
                    .rounded(px(6.0))
                    .bg(theme::bg_tertiary())
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(13.0))
                    .min_h(px(36.0))
                    .child(value_view)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        window.focus(&this.focus_handle);
                        cx.notify();
                    })),
            )
    }

    fn render_submit_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = !self.token_input.trim().is_empty() && self.loading == Loading::Idle;
        let is_loading = self.loading == Loading::Token;
        div()
            .id("login-submit-btn")
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .w_full()
            .py(px(9.0))
            .rounded(px(6.0))
            .bg(if enabled {
                theme::accent()
            } else {
                theme::bg_tertiary()
            })
            .border_1()
            .border_color(theme::border())
            .text_size(px(13.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if enabled {
                theme::text_primary()
            } else {
                theme::text_secondary()
            })
            .cursor_pointer()
            .when(is_loading, |d| {
                d.child(
                    icon(Icon::Loader)
                        .size(px(13.0))
                        .text_color(theme::accent()),
                )
            })
            .child(if is_loading {
                "Signing in..."
            } else {
                "Sign in"
            })
            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                this.submit_admin_token(cx);
            }))
    }

    fn render_oidc_section(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        // Only shown when OIDC is confirmed configured.
        if self.oidc_configured != Some(true) {
            return None;
        }
        let label = if let Some(domain) = &self.issuer_domain {
            format!("Continue with {domain}")
        } else {
            "Continue with SSO".to_string()
        };
        let is_loading = self.loading == Loading::Oidc;
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().flex_1().h(px(1.0)).bg(theme::border()))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::text_tertiary())
                                .child("or"),
                        )
                        .child(div().flex_1().h(px(1.0)).bg(theme::border())),
                )
                .child(
                    div()
                        .id("login-oidc-btn")
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(6.0))
                        .w_full()
                        .py(px(9.0))
                        .rounded(px(6.0))
                        .bg(theme::bg_tertiary())
                        .border_1()
                        .border_color(theme::border())
                        .text_size(px(13.0))
                        .text_color(theme::text_secondary())
                        .cursor_pointer()
                        .when(is_loading, |d| {
                            d.child(
                                icon(Icon::Loader)
                                    .size(px(13.0))
                                    .text_color(theme::accent()),
                            )
                        })
                        .child(label)
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.open_oidc_browser(cx);
                        })),
                ),
        )
    }
}

impl Render for LoginView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("login-root")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme::bg_primary())
            .flex()
            .items_center()
            .justify_center()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _w, cx| {
                if this.handle_key(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, _cx| {
                    window.focus(&this.focus_handle);
                }),
            )
            .child(
                // Card
                div()
                    .w(px(400.0))
                    .flex()
                    .flex_col()
                    .gap(px(0.0))
                    .rounded(px(12.0))
                    .bg(theme::bg_secondary())
                    .border_1()
                    .border_color(theme::border())
                    // Header
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(28.0))
                            .pt(px(32.0))
                            .pb(px(20.0))
                            .border_b_1()
                            .border_color(theme::border())
                            .child(
                                div()
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child("Sign in to ZRemote"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme::text_tertiary())
                                    .child(self.app_state.api.base_url().to_string()),
                            ),
                    )
                    // Body
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(16.0))
                            .px(px(28.0))
                            .py(px(24.0))
                            .child(self.render_token_input(cx))
                            .when_some(self.error.as_deref(), |d, err| {
                                d.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.0))
                                        .px(px(10.0))
                                        .py(px(8.0))
                                        .rounded(px(6.0))
                                        .bg(Rgba {
                                            r: 0.87,
                                            g: 0.27,
                                            b: 0.27,
                                            a: 0.12,
                                        })
                                        .border_1()
                                        .border_color(theme::error())
                                        .child(
                                            icon(Icon::AlertTriangle)
                                                .size(px(12.0))
                                                .text_color(theme::error()),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.0))
                                                .text_color(theme::error())
                                                .child(err.to_string()),
                                        ),
                                )
                            })
                            .child(self.render_submit_button(cx))
                            .when_some(self.render_oidc_section(cx), |d, oidc| d.child(oidc)),
                    ),
            )
    }
}

fn session_entry_from_response(resp: SessionTokenResponse) -> SessionEntry {
    let expires_at = resp
        .expires_at
        .parse::<chrono::DateTime<chrono::Utc>>()
        .ok();
    SessionEntry {
        session_token: resp.session_token,
        expires_at,
    }
}

#[cfg(test)]
mod tests {
    use super::session_entry_from_response;
    use zremote_client::SessionTokenResponse;

    #[test]
    fn session_entry_from_response_no_expiry() {
        let resp = SessionTokenResponse {
            session_token: "abc123".into(),
            expires_at: "invalid-date".into(),
        };
        let entry = session_entry_from_response(resp);
        assert_eq!(entry.session_token, "abc123");
        assert!(entry.expires_at.is_none());
    }

    #[test]
    fn session_entry_from_response_valid_expiry() {
        let resp = SessionTokenResponse {
            session_token: "tok".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
        };
        let entry = session_entry_from_response(resp);
        assert!(entry.expires_at.is_some());
        assert!(!entry.is_expired());
    }
}
