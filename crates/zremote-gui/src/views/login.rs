#![allow(clippy::wildcard_imports)]

//! Login screen: admin token field + optional SSO/OIDC button.
//!
//! Emits `LoginEvent::LoggedIn(SessionEntry)` on successful authentication.
//! The OIDC button is hidden unless the server reports `oidc_status.configured = true`.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
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

    /// CSRF state token from `oidc_init` — validated against callback query param.
    pending_oidc_state: Option<String>,
    /// Loopback listener for the OIDC redirect; held until callback received.
    oidc_listener: Option<TcpListener>,

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
            pending_oidc_state: None,
            oidc_listener: None,
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

        // Bind an OS-assigned loopback port before initiating the flow so we
        // know the redirect URI before we call the server.
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => {
                self.error = Some(format!("Could not bind loopback port: {e}"));
                cx.notify();
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(e) => {
                self.error = Some(format!("Could not get loopback port: {e}"));
                cx.notify();
                return;
            }
        };
        let redirect_uri = format!("http://127.0.0.1:{port}/oidc/callback");

        self.oidc_listener = Some(listener);
        self.loading = Loading::Oidc;
        self.error = None;
        cx.notify();

        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        self.login_task = Some(
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let redirect_uri_clone = redirect_uri.clone();
                let result = handle
                    .spawn(async move { api.login_oidc_init(&redirect_uri_clone).await })
                    .await;

                let (init, listener) = match this.update(cx, |this, cx| {
                    this.loading = Loading::Idle;
                    match result {
                        Ok(Ok(init)) => {
                            this.pending_oidc_state = Some(init.state.clone());
                            let listener = this.oidc_listener.take();
                            cx.notify();
                            Ok((init, listener))
                        }
                        Ok(Err(err)) => {
                            this.error = Some(format!("OIDC init failed: {err}"));
                            this.oidc_listener = None;
                            cx.notify();
                            Err(())
                        }
                        Err(_) => {
                            this.error = Some("Internal error. Please try again.".to_string());
                            this.oidc_listener = None;
                            cx.notify();
                            Err(())
                        }
                    }
                }) {
                    Ok(Ok(pair)) => pair,
                    _ => return,
                };

                let Some(listener) = listener else {
                    return;
                };

                if let Err(e) = open::that(&init.auth_url) {
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some(format!("Could not open browser: {e}"));
                        this.pending_oidc_state = None;
                        cx.notify();
                    });
                    return;
                }

                // Wait for the loopback callback on a blocking thread.
                let expected_state = init.state.clone();
                let api2 = {
                    // Snapshot the api pointer before moving into blocking task.
                    match this.update(cx, |this, _cx| this.app_state.api.clone()) {
                        Ok(a) => a,
                        Err(_) => return,
                    }
                };
                let handle2 = match this.update(cx, |this, _cx| this.app_state.tokio_handle.clone())
                {
                    Ok(h) => h,
                    Err(_) => return,
                };

                let callback_result = handle2
                    .spawn_blocking(move || accept_oidc_callback(listener, &expected_state))
                    .await;

                let code = match callback_result {
                    Ok(Ok(c)) => c,
                    Ok(Err(e)) => {
                        let _ = this.update(cx, |this, cx| {
                            this.error = Some(format!("OIDC callback error: {e}"));
                            this.pending_oidc_state = None;
                            cx.notify();
                        });
                        return;
                    }
                    Err(_) => {
                        let _ = this.update(cx, |this, cx| {
                            this.error = Some("OIDC callback task panicked.".to_string());
                            this.pending_oidc_state = None;
                            cx.notify();
                        });
                        return;
                    }
                };

                // Exchange code for token.
                let exchange_result = handle2
                    .spawn(async move { api2.login_oidc_callback(code, redirect_uri).await })
                    .await;

                let _ = this.update(cx, |this, cx| {
                    this.pending_oidc_state = None;
                    match exchange_result {
                        Ok(Ok(resp)) => {
                            cx.emit(LoginEvent::LoggedIn(session_entry_from_response(resp)));
                        }
                        Ok(Err(err)) => {
                            this.error = Some(format!("OIDC token exchange failed: {err}"));
                            cx.notify();
                        }
                        Err(_) => {
                            this.error = Some("Internal error during OIDC exchange.".to_string());
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

/// Accepts one TCP connection on `listener`, reads the HTTP request line,
/// extracts `code` and `state` from the query string, validates `state` against
/// `expected_state`, writes a minimal 200 HTML response, and returns `code`.
///
/// Runs on a blocking thread (called via `spawn_blocking`).
fn accept_oidc_callback(listener: TcpListener, expected_state: &str) -> Result<String, String> {
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("set_nonblocking: {e}"))?;

    let (stream, _) = listener.accept().map_err(|e| format!("accept: {e}"))?;

    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("read_line: {e}"))?;

    // Drain remaining headers so the browser gets a proper response.
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("drain headers: {e}"))?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    // Parse: "GET /oidc/callback?code=X&state=Y HTTP/1.1"
    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or("malformed request line")?;

    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    for param in query.split('&') {
        if let Some((k, v)) = param.split_once('=') {
            match k {
                "code" => code = Some(percent_decode(v)),
                "state" => state = Some(percent_decode(v)),
                _ => {}
            }
        }
    }

    let code = code.ok_or("missing code parameter")?;
    let state = state.ok_or("missing state parameter")?;

    if state != expected_state {
        write_http_response(&stream, 400, "State mismatch — possible CSRF attack.");
        return Err("OIDC state mismatch".to_string());
    }

    write_http_response(
        &stream,
        200,
        "<html><body><h2>Authentication successful — you may close this tab.</h2></body></html>",
    );

    Ok(code)
}

fn write_http_response(mut stream: impl Write, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hi = chars.next().unwrap_or('0');
            let lo = chars.next().unwrap_or('0');
            if let Ok(byte) = u8::from_str_radix(&format!("{hi}{lo}"), 16) {
                out.push(byte as char);
                continue;
            }
        }
        out.push(c);
    }
    out
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
