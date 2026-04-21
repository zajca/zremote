#![allow(clippy::wildcard_imports)]

//! "Add Host" 3-step enrollment wizard modal.
//!
//! Step 1 — Create enrollment code: hostname input + expiry dropdown →
//!   POST /api/admin/enroll/create → show code + countdown + install snippet.
//! Step 2 — Install command: shows the one-liner; polls GET /api/hosts every
//!   3 s to detect when the new host appears.
//! Step 3 — Success: host enrolled, show hostname + "Connect" / "Done".

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use zeroize::Zeroize;

use crate::app_state::AppState;
use crate::icons::{Icon, icon};
use crate::theme;
use zremote_client::{CreateEnrollmentRequest, EnrollmentCodeResponse, Host};

/// Event emitted by `AddHostModal`.
#[derive(Debug, Clone)]
pub enum AddHostModalEvent {
    Close,
    /// A host was enrolled. Caller may want to refresh the host list.
    Enrolled {
        host_id: String,
        hostname: String,
    },
}

impl EventEmitter<AddHostModalEvent> for AddHostModal {}

/// Hard deadline for enrollment polling (10 minutes). After this the wizard
/// transitions to `PollState::Expired` so the poll loop never runs indefinitely.
const POLL_DEADLINE_SECS: u64 = 600;

/// Hostname validation regex: RFC-952 / RFC-1123 labels.
fn is_valid_hostname(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let first = s.chars().next().unwrap_or('\0');
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Create,
    Install,
    Success,
}

/// State of the polling phase in Step 2.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PollState {
    Waiting,
    Error(String),
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpiryChoice {
    TenMin,
    ThirtyMin,
    OneHour,
    SixHours,
}

impl ExpiryChoice {
    fn secs(self) -> u64 {
        match self {
            Self::TenMin => 600,
            Self::ThirtyMin => 1800,
            Self::OneHour => 3600,
            Self::SixHours => 21600,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::TenMin => "10 min",
            Self::ThirtyMin => "30 min",
            Self::OneHour => "1 hour",
            Self::SixHours => "6 hours",
        }
    }
}

pub struct AddHostModal {
    app_state: Arc<AppState>,
    focus_handle: FocusHandle,

    step: Step,
    hostname_input: String,
    hostname_error: Option<String>,
    expiry: ExpiryChoice,

    /// Set after successful code generation.
    enrollment: Option<EnrollmentCodeResponse>,
    /// Elapsed since last countdown update, used by ticker.
    countdown_secs: u64,
    /// Elapsed seconds of polling — hard deadline enforced at POLL_DEADLINE_SECS.
    poll_elapsed_secs: u64,

    /// Server URL cached for the install snippet.
    server_url: String,

    /// Polling phase state (step 2).
    poll_state: PollState,

    /// Host that just enrolled (Step 3).
    enrolled_host: Option<Host>,

    submitting: bool,
    error: Option<String>,

    /// In-flight create-code task.
    create_task: Option<Task<()>>,
    /// Polling task for host detection (Step 2).
    poll_task: Option<Task<()>>,
    /// Countdown + poll deadline tick task.
    tick_task: Option<Task<()>>,
}

impl AddHostModal {
    pub fn new(app_state: Arc<AppState>, cx: &mut Context<Self>) -> Self {
        let server_url = app_state.api.base_url().to_string();
        Self {
            app_state,
            focus_handle: cx.focus_handle(),
            step: Step::Create,
            hostname_input: String::new(),
            hostname_error: None,
            expiry: ExpiryChoice::TenMin,
            enrollment: None,
            countdown_secs: 0,
            poll_elapsed_secs: 0,
            server_url,
            poll_state: PollState::Waiting,
            enrolled_host: None,
            submitting: false,
            error: None,
            create_task: None,
            poll_task: None,
            tick_task: None,
        }
    }

    fn generate_code(&mut self, cx: &mut Context<Self>) {
        let hostname = self.hostname_input.trim().to_string();
        if self.submitting {
            return;
        }
        // Validate hostname before submitting.
        if !is_valid_hostname(&hostname) {
            self.hostname_error = Some(
                "Invalid hostname. Use letters, digits, hyphens, dots (max 64 chars).".to_string(),
            );
            cx.notify();
            return;
        }
        self.hostname_error = None;
        self.submitting = true;
        self.error = None;
        cx.notify();

        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        let expires_in_secs = self.expiry.secs();

        let req = CreateEnrollmentRequest {
            hostname: Some(hostname),
            expires_in_secs: Some(expires_in_secs),
        };

        self.create_task = Some(cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let result = handle
                    .spawn(async move { api.admin_enroll_create(&req).await })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.submitting = false;
                    match result {
                        Ok(Ok(code_resp)) => {
                            let expires_in = code_resp
                                .expires_at
                                .parse::<chrono::DateTime<chrono::Utc>>()
                                .ok()
                                .map(|t| {
                                    let diff = t - chrono::Utc::now();
                                    diff.num_seconds().max(0) as u64
                                })
                                .unwrap_or(600);
                            this.countdown_secs = expires_in;
                            this.poll_elapsed_secs = 0;
                            this.poll_state = PollState::Waiting;
                            this.enrollment = Some(code_resp);
                            this.step = Step::Install;
                            this.tick_task = Some(this.spawn_tick(cx));
                            this.poll_task = Some(this.spawn_host_poll(cx));
                        }
                        Ok(Err(err)) => {
                            this.error = Some(format!("Failed to generate code: {err}"));
                        }
                        Err(_) => {
                            this.error = Some("Internal error. Please try again.".to_string());
                        }
                    }
                    cx.notify();
                });
            },
        ));
    }

    fn spawn_tick(&self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                Timer::after(Duration::from_secs(1)).await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        if this.countdown_secs > 0 {
                            this.countdown_secs -= 1;
                        }
                        this.poll_elapsed_secs += 1;
                        // Hard deadline: stop polling after POLL_DEADLINE_SECS.
                        if this.poll_elapsed_secs >= POLL_DEADLINE_SECS
                            && this.poll_state == PollState::Waiting
                        {
                            this.poll_state = PollState::Expired;
                        }
                        cx.notify();
                        // Stop when code expired, step changed, or hard deadline hit
                        this.step == Step::Install
                            && this.countdown_secs > 0
                            && this.poll_state == PollState::Waiting
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
    }

    fn spawn_host_poll(&self, cx: &mut Context<Self>) -> Task<()> {
        let api = self.app_state.api.clone();
        let handle = self.app_state.tokio_handle.clone();
        let hostname = self.hostname_input.trim().to_string();

        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // Remember hosts we knew about before issuing the code so we can
            // detect the newly enrolled one.
            let baseline: Vec<String> = {
                let api = api.clone();
                handle
                    .spawn(async move {
                        api.list_hosts_authed()
                            .await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|h| h.id)
                            .collect()
                    })
                    .await
                    .unwrap_or_default()
            };

            loop {
                Timer::after(Duration::from_secs(3)).await;

                let api = api.clone();
                let baseline = baseline.clone();
                let hn = hostname.clone();
                let poll_result = handle
                    .spawn(async move { api.list_hosts_authed().await })
                    .await;

                let keep_going = this
                    .update(cx, |this, cx| {
                        // Hard deadline or step change — stop silently.
                        if this.step != Step::Install
                            || this.poll_state == PollState::Expired
                            || this.countdown_secs == 0
                        {
                            return false;
                        }
                        match poll_result {
                            Ok(Ok(hosts)) => {
                                let new_host = hosts
                                    .into_iter()
                                    .find(|h| !baseline.contains(&h.id) && h.hostname == hn);
                                if let Some(host) = new_host {
                                    this.enrolled_host = Some(host.clone());
                                    this.step = Step::Success;
                                    this.tick_task = None;
                                    cx.emit(AddHostModalEvent::Enrolled {
                                        host_id: host.id,
                                        hostname: host.hostname,
                                    });
                                    cx.notify();
                                    return false;
                                }
                                true
                            }
                            Ok(Err(err)) => {
                                this.poll_state = PollState::Error(format!("Poll failed: {err}"));
                                this.tick_task = None;
                                cx.notify();
                                false
                            }
                            Err(_) => {
                                this.poll_state =
                                    PollState::Error("Poll task panicked.".to_string());
                                this.tick_task = None;
                                cx.notify();
                                false
                            }
                        }
                    })
                    .unwrap_or(false);

                if !keep_going {
                    break;
                }
            }
        })
    }

    /// Reset wizard back to step 1 (used from Expired / Error states).
    fn restart(&mut self, cx: &mut Context<Self>) {
        self.step = Step::Create;
        self.poll_state = PollState::Waiting;
        self.poll_elapsed_secs = 0;
        self.countdown_secs = 0;
        if let Some(enroll) = &mut self.enrollment {
            enroll.code.zeroize();
        }
        self.enrollment = None;
        self.tick_task = None;
        self.poll_task = None;
        self.error = None;
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(enroll) = &mut self.enrollment {
            enroll.code.zeroize();
        }
        self.enrollment = None;
        cx.emit(AddHostModalEvent::Close);
    }

    fn copy_code(&self, cx: &mut App) {
        if let Some(enroll) = &self.enrollment {
            cx.write_to_clipboard(ClipboardItem::new_string(enroll.code.clone()));
        }
    }

    fn copy_install_command(&self, cx: &mut App) {
        if let Some(enroll) = &self.enrollment {
            let snippet = install_snippet(&self.server_url, &enroll.code);
            cx.write_to_clipboard(ClipboardItem::new_string(snippet));
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        match self.step {
            Step::Create => match event.keystroke.key.as_str() {
                "backspace" => {
                    self.hostname_input.pop();
                    cx.notify();
                    true
                }
                "return" | "enter" => {
                    self.generate_code(cx);
                    true
                }
                "escape" => {
                    self.close(cx);
                    true
                }
                ch => {
                    if ch.len() == 1 && !event.keystroke.modifiers.platform {
                        self.hostname_input.push_str(ch);
                        cx.notify();
                        true
                    } else {
                        false
                    }
                }
            },
            Step::Install | Step::Success => {
                if event.keystroke.key == "escape" {
                    self.close(cx);
                    true
                } else {
                    false
                }
            }
        }
    }

    // ---- render helpers ----

    fn render_step_indicator(&self) -> impl IntoElement {
        let steps = [
            (Step::Create, "1 Create"),
            (Step::Install, "2 Install"),
            (Step::Success, "3 Ready"),
        ];
        div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(16.0))
            .mb(px(16.0))
            .children(steps.iter().map(|(s, label)| {
                let active = *s == self.step;
                let passed = matches!(
                    (*s, self.step),
                    (Step::Create, Step::Install | Step::Success) | (Step::Install, Step::Success)
                );
                div()
                    .text_size(px(11.0))
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if active {
                        theme::accent()
                    } else if passed {
                        theme::success()
                    } else {
                        theme::text_tertiary()
                    })
                    .child(*label)
                    .into_any_element()
            }))
    }

    fn render_step_create(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let trimmed = self.hostname_input.trim();
        let has_valid_hostname = !trimmed.is_empty() && is_valid_hostname(trimmed);
        let display = if self.hostname_input.is_empty() {
            div()
                .text_color(theme::text_tertiary())
                .child("e.g. my-server")
        } else {
            div()
                .text_color(theme::text_primary())
                .child(self.hostname_input.clone())
        };

        let expiry_options = [
            ExpiryChoice::TenMin,
            ExpiryChoice::ThirtyMin,
            ExpiryChoice::OneHour,
            ExpiryChoice::SixHours,
        ];

        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_secondary())
                            .child("Hostname"),
                    )
                    .child(
                        div()
                            .id("ah-hostname-input")
                            .px(px(10.0))
                            .py(px(8.0))
                            .rounded(px(6.0))
                            .bg(theme::bg_tertiary())
                            .border_1()
                            .border_color(if self.hostname_error.is_some() {
                                theme::error()
                            } else {
                                theme::border()
                            })
                            .text_size(px(13.0))
                            .min_h(px(34.0))
                            .child(display),
                    )
                    .when_some(self.hostname_error.as_deref(), |d, msg| {
                        d.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme::error())
                                .child(msg.to_string()),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_secondary())
                            .child("Code expiry"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.0))
                            .children(expiry_options.iter().map(|opt| {
                                let is_selected = *opt == self.expiry;
                                let opt = *opt;
                                div()
                                    .id(SharedString::from(format!("expiry-{}", opt.secs())))
                                    .px(px(10.0))
                                    .py(px(5.0))
                                    .rounded(px(4.0))
                                    .border_1()
                                    .border_color(if is_selected {
                                        theme::accent()
                                    } else {
                                        theme::border()
                                    })
                                    .bg(if is_selected {
                                        theme::accent_subtle()
                                    } else {
                                        theme::bg_tertiary()
                                    })
                                    .text_size(px(12.0))
                                    .text_color(if is_selected {
                                        theme::accent()
                                    } else {
                                        theme::text_secondary()
                                    })
                                    .cursor_pointer()
                                    .child(opt.label())
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                        this.expiry = opt;
                                        cx.notify();
                                    }))
                                    .into_any_element()
                            })),
                    ),
            )
            .when_some(self.error.as_deref(), |d, err| {
                d.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme::error())
                        .child(err.to_string()),
                )
            })
            .child(
                div()
                    .id("ah-generate-btn")
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .px(px(12.0))
                    .py(px(8.0))
                    .rounded(px(6.0))
                    .bg(if has_valid_hostname && !self.submitting {
                        theme::accent()
                    } else {
                        theme::bg_tertiary()
                    })
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(13.0))
                    .text_color(if has_valid_hostname && !self.submitting {
                        theme::text_primary()
                    } else {
                        theme::text_secondary()
                    })
                    .cursor_pointer()
                    .when(self.submitting, |d| {
                        d.child(
                            icon(Icon::Loader)
                                .size(px(13.0))
                                .text_color(theme::accent()),
                        )
                    })
                    .child(if self.submitting {
                        "Generating..."
                    } else {
                        "Generate Code"
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.generate_code(cx);
                    })),
            )
    }

    fn render_step_install(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let code = self
            .enrollment
            .as_ref()
            .map(|e| e.code.as_str())
            .unwrap_or("...");
        let mm = self.countdown_secs / 60;
        let ss = self.countdown_secs % 60;
        let countdown = format!("{mm:02}:{ss:02}");
        let expired = self.countdown_secs == 0;
        let snippet = install_snippet(&self.server_url, code);

        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(self.render_install_code_section(code, expired, &countdown, cx))
            .child(self.render_install_snippet_section(&snippet, cx))
            .child(self.render_poll_status(cx))
    }

    fn render_install_code_section(
        &self,
        code: &str,
        expired: bool,
        countdown: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_secondary())
                    .child("Enrollment code"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .px(px(10.0))
                            .py(px(8.0))
                            .rounded(px(6.0))
                            .bg(theme::bg_tertiary())
                            .border_1()
                            .border_color(if expired {
                                theme::error()
                            } else {
                                theme::border()
                            })
                            .text_size(px(16.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if expired {
                                theme::error()
                            } else {
                                theme::text_primary()
                            })
                            .child(code.to_string()),
                    )
                    .child(
                        div()
                            .id("ah-copy-code-btn")
                            .px(px(10.0))
                            .py(px(8.0))
                            .rounded(px(6.0))
                            .bg(theme::bg_tertiary())
                            .border_1()
                            .border_color(theme::border())
                            .text_size(px(12.0))
                            .text_color(theme::text_secondary())
                            .cursor_pointer()
                            .child("Copy")
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.copy_code(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(icon(Icon::Clock).size(px(11.0)).text_color(if expired {
                        theme::error()
                    } else {
                        theme::text_tertiary()
                    }))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(if expired {
                                theme::error()
                            } else {
                                theme::text_tertiary()
                            })
                            .child(if expired {
                                "Code expired. Generate a new one.".to_string()
                            } else {
                                format!("Expires in {countdown}")
                            }),
                    ),
            )
    }

    fn render_install_snippet_section(
        &self,
        snippet: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_secondary())
                    .child("Install command"),
            )
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme::text_tertiary())
                    .child("Run on the target machine:"),
            )
            .child(
                div()
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(px(6.0))
                    .bg(theme::bg_tertiary())
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(11.0))
                    .text_color(theme::text_secondary())
                    .font_family("monospace")
                    .child(snippet.to_string()),
            )
            .child(
                div()
                    .id("ah-copy-install-btn")
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(12.0))
                    .py(px(7.0))
                    .rounded(px(6.0))
                    .bg(theme::bg_tertiary())
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(12.0))
                    .text_color(theme::text_secondary())
                    .cursor_pointer()
                    .child("Copy install command")
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.copy_install_command(cx);
                    })),
            )
    }

    fn render_poll_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.poll_state {
            PollState::Waiting => div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    icon(Icon::Loader)
                        .size(px(13.0))
                        .text_color(theme::accent()),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme::text_secondary())
                        .child("Waiting for host to enroll..."),
                )
                .into_any_element(),
            PollState::Error(msg) => div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(6.0))
                        .bg(theme::error_bg())
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
                                .child(msg.clone()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(
                            div()
                                .id("ah-poll-retry-btn")
                                .px(px(12.0))
                                .py(px(7.0))
                                .rounded(px(6.0))
                                .bg(theme::accent())
                                .border_1()
                                .border_color(theme::border())
                                .text_size(px(12.0))
                                .text_color(theme::text_primary())
                                .cursor_pointer()
                                .child("Retry")
                                .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                    this.poll_state = PollState::Waiting;
                                    this.poll_task = Some(this.spawn_host_poll(cx));
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("ah-poll-back-btn")
                                .px(px(12.0))
                                .py(px(7.0))
                                .rounded(px(6.0))
                                .bg(theme::bg_tertiary())
                                .border_1()
                                .border_color(theme::border())
                                .text_size(px(12.0))
                                .text_color(theme::text_secondary())
                                .cursor_pointer()
                                .child("Start over")
                                .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                    this.restart(cx);
                                })),
                        ),
                )
                .into_any_element(),
            PollState::Expired => div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme::warning())
                        .child(
                            "Enrollment window expired (10 min). Regenerate a code to continue.",
                        ),
                )
                .child(
                    div()
                        .id("ah-regenerate-btn")
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(12.0))
                        .py(px(7.0))
                        .rounded(px(6.0))
                        .bg(theme::accent())
                        .border_1()
                        .border_color(theme::border())
                        .text_size(px(12.0))
                        .text_color(theme::text_primary())
                        .cursor_pointer()
                        .child("Regenerate code")
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.restart(cx);
                        })),
                )
                .into_any_element(),
        }
    }

    fn render_step_success(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hostname = self
            .enrolled_host
            .as_ref()
            .map(|h| h.hostname.as_str())
            .unwrap_or(&self.hostname_input);
        let host_id = self
            .enrolled_host
            .as_ref()
            .map(|h| h.id.clone())
            .unwrap_or_default();

        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(16.0))
            .child(
                icon(Icon::CheckCircle)
                    .size(px(40.0))
                    .text_color(theme::success()),
            )
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child(format!("Host '{hostname}' enrolled successfully!")),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        div()
                            .id("ah-done-btn")
                            .px(px(14.0))
                            .py(px(8.0))
                            .rounded(px(6.0))
                            .bg(theme::bg_tertiary())
                            .border_1()
                            .border_color(theme::border())
                            .text_size(px(13.0))
                            .text_color(theme::text_secondary())
                            .cursor_pointer()
                            .child("Done")
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                this.close(cx);
                            })),
                    )
                    .when(!host_id.is_empty(), |d| {
                        d.child(
                            div()
                                .id("ah-connect-btn")
                                .px(px(14.0))
                                .py(px(8.0))
                                .rounded(px(6.0))
                                .bg(theme::accent())
                                .border_1()
                                .border_color(theme::border())
                                .text_size(px(13.0))
                                .text_color(theme::text_primary())
                                .cursor_pointer()
                                .child("Connect now")
                                .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                    this.close(cx);
                                })),
                        )
                    }),
            )
    }
}

impl AddHostModal {
    fn render_modal_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(20.0))
            .py(px(14.0))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_primary())
                    .child("Add Host"),
            )
            .child(
                div()
                    .id("ah-close-btn")
                    .p(px(4.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .child(
                        icon(Icon::X)
                            .size(px(14.0))
                            .text_color(theme::text_secondary()),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.close(cx);
                    })),
            )
    }

    fn render_modal_body(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let step_content = match self.step {
            Step::Create => self.render_step_create(cx).into_any_element(),
            Step::Install => self.render_step_install(cx).into_any_element(),
            Step::Success => self.render_step_success(cx).into_any_element(),
        };
        let header = self.render_modal_header(cx).into_any_element();
        let step_indicator = self.render_step_indicator().into_any_element();
        div()
            .occlude()
            .w(px(480.0))
            .flex()
            .flex_col()
            .rounded(px(12.0))
            .bg(theme::bg_secondary())
            .border_1()
            .border_color(theme::border())
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _w, cx| {
                cx.stop_propagation();
            })
            .child(header)
            .child(div().px(px(20.0)).pt(px(16.0)).child(step_indicator))
            .child(div().px(px(20.0)).py(px(16.0)).child(step_content))
    }
}

impl Render for AddHostModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus_handle.contains_focused(window, cx) {
            self.focus_handle.focus(window);
        }
        div()
            .id("add-host-modal-root")
            .track_focus(&self.focus_handle)
            .absolute()
            .size_full()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .bg(theme::modal_backdrop())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _w, cx| {
                if this.handle_key(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    this.close(cx);
                }),
            )
            .child(self.render_modal_body(cx))
    }
}

fn install_snippet(server_url: &str, code: &str) -> String {
    let safe_url: Cow<str> = shell_escape::unix::escape(server_url.into());
    let safe_code: Cow<str> = shell_escape::unix::escape(code.into());
    // Two-step: download to a temp file, then execute. Avoids piping an
    // unknown script directly into bash and lets the user inspect the file.
    format!(
        "export ZREMOTE_SERVER_URL={safe_url}\nexport ZREMOTE_ENROLLMENT_CODE={safe_code}\ncurl -fsSL \"$ZREMOTE_SERVER_URL/enroll.sh\" -o /tmp/zremote-enroll.sh\nbash /tmp/zremote-enroll.sh"
    )
}

#[cfg(test)]
mod tests {
    use super::{ExpiryChoice, install_snippet, is_valid_hostname};

    #[test]
    fn expiry_choice_secs() {
        assert_eq!(ExpiryChoice::TenMin.secs(), 600);
        assert_eq!(ExpiryChoice::ThirtyMin.secs(), 1800);
        assert_eq!(ExpiryChoice::OneHour.secs(), 3600);
        assert_eq!(ExpiryChoice::SixHours.secs(), 21600);
    }

    #[test]
    fn install_snippet_contains_code_and_url() {
        let snippet = install_snippet("https://my.server", "AB12-CD34");
        assert!(snippet.contains("my.server"), "missing server url");
        assert!(snippet.contains("AB12-CD34"), "missing code");
        assert!(snippet.contains("enroll.sh"), "missing script path");
    }

    #[test]
    fn install_snippet_escapes_injection() {
        let url_with_injection = "https://evil.com\"; rm -rf /";
        let code_with_injection = "code\"; rm -rf /";
        let snippet = install_snippet(url_with_injection, code_with_injection);
        // shell_escape::unix::escape wraps dangerous values in single quotes.
        // The assignment must be `export VAR='...'` not `export VAR="..."`,
        // so the double-quote in the value cannot break out of quoting.
        //
        // If double-quoted assignment were used, `export VAR="evil.com"; rm`
        // would execute `rm`.  With single-quote escaping the assignment is
        // `export VAR='evil.com"; rm -rf /'` which is safe.
        //
        // Verify: `export ZREMOTE_SERVER_URL="` (double-quote wrap) must NOT
        // appear in the output.
        assert!(
            !snippet.contains("ZREMOTE_SERVER_URL=\""),
            "url value must not be wrapped in double quotes"
        );
        assert!(
            !snippet.contains("ZREMOTE_ENROLLMENT_CODE=\""),
            "code value must not be wrapped in double quotes"
        );
        // The values must still be present (correctly single-quoted).
        assert!(snippet.contains("evil.com"), "url not present in snippet");
        assert!(snippet.contains("code"), "code not present in snippet");
    }

    #[test]
    fn expiry_choice_labels() {
        assert_eq!(ExpiryChoice::TenMin.label(), "10 min");
        assert_eq!(ExpiryChoice::OneHour.label(), "1 hour");
    }

    #[test]
    fn hostname_validation_accepts_valid() {
        assert!(is_valid_hostname("my-server"));
        assert!(is_valid_hostname("host01"));
        assert!(is_valid_hostname("a.b.c"));
    }

    #[test]
    fn hostname_validation_rejects_invalid() {
        assert!(!is_valid_hostname(""));
        assert!(!is_valid_hostname("-starts-with-dash"));
        assert!(!is_valid_hostname("has spaces"));
        assert!(!is_valid_hostname("has\"quote"));
        let long = "a".repeat(65);
        assert!(!is_valid_hostname(&long));
    }

    #[test]
    fn install_snippet_two_step() {
        let snippet = install_snippet("https://my.server", "CODE");
        assert!(
            snippet.contains("-o /tmp/zremote-enroll.sh"),
            "missing download step"
        );
        assert!(
            snippet.contains("bash /tmp/zremote-enroll.sh"),
            "missing execute step"
        );
    }
}
