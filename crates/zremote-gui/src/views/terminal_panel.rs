use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::Processor;
use gpui::*;

use crate::terminal_ws::{self, TerminalWsHandle};
use crate::theme;
use crate::types::TerminalEvent;
use crate::views::terminal_element::TerminalElement;

/// Cursor blink interval (standard terminal blink rate).
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// Default terminal dimensions (used until first resize fits to container).
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

/// Lines to scroll per mouse wheel tick.
const SCROLL_LINES_PER_TICK: i32 = 3;

pub struct TerminalPanel {
    session_id: String,
    term: Arc<Mutex<alacritty_terminal::Term<VoidListener>>>,
    ws_handle: TerminalWsHandle,
    focus_handle: FocusHandle,
    closed: bool,
    reader_started: bool,
    /// Whether the cursor is currently visible (toggled by blink timer).
    cursor_visible: bool,
    /// Whether the blink timer task has been spawned.
    blink_started: bool,
    /// Subscription handle for keystroke observation (reset cursor blink on input).
    _keystroke_subscription: Subscription,
}

impl TerminalPanel {
    pub fn new(
        session_id: String,
        ws_url: String,
        tokio_handle: &tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = TermConfig::default();
        let size = TermSize::new(usize::from(DEFAULT_COLS), usize::from(DEFAULT_ROWS));
        let term = alacritty_terminal::Term::new(config, &size, VoidListener);
        let term = Arc::new(Mutex::new(term));

        let ws_handle = terminal_ws::connect(ws_url, tokio_handle);
        let focus_handle = cx.focus_handle();

        // Reset cursor to visible on any keystroke so it doesn't blink while typing.
        let keystroke_subscription = cx.observe_keystrokes(
            |this: &mut Self, _event: &KeystrokeEvent, _window: &mut Window, cx: &mut Context<Self>| {
                this.reset_cursor_blink(cx);
            },
        );

        Self {
            session_id,
            term,
            ws_handle,
            focus_handle,
            closed: false,
            reader_started: false,
            cursor_visible: true,
            blink_started: false,
            _keystroke_subscription: keystroke_subscription,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    fn start_output_reader(&mut self, cx: &mut Context<Self>) {
        if self.reader_started {
            return;
        }
        self.reader_started = true;

        let output_rx = self.ws_handle.output_rx.clone();
        let term = self.term.clone();

        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut processor: Processor = Processor::new();

            loop {
                match output_rx.recv_async().await {
                    Ok(TerminalEvent::Output(bytes)) => {
                        if let Ok(mut term) = term.lock() {
                            processor.advance(&mut *term, &bytes);
                        }
                        let _ = this.update(cx, |_this: &mut Self, cx: &mut Context<Self>| {
                            cx.notify();
                        });
                    }
                    Ok(TerminalEvent::SessionClosed { .. }) => {
                        let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                            this.closed = true;
                            cx.notify();
                        });
                        break;
                    }
                    Ok(TerminalEvent::ScrollbackStart) => {
                        if let Ok(mut term) = term.lock() {
                            let size =
                                TermSize::new(usize::from(DEFAULT_COLS), usize::from(DEFAULT_ROWS));
                            *term = alacritty_terminal::Term::new(
                                TermConfig::default(),
                                &size,
                                VoidListener,
                            );
                        }
                    }
                    Ok(TerminalEvent::ScrollbackEnd) => {
                        let _ = this.update(cx, |_this: &mut Self, cx: &mut Context<Self>| {
                            cx.notify();
                        });
                    }
                    Err(_) => break,
                }
            }
        })
        .detach();
    }

    /// Start the cursor blink timer. Spawns an async loop that toggles
    /// `cursor_visible` every 500ms and triggers a repaint.
    fn start_cursor_blink(&mut self, cx: &mut Context<Self>) {
        if self.blink_started {
            return;
        }
        self.blink_started = true;

        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                Timer::after(CURSOR_BLINK_INTERVAL).await;
                let Ok(()) = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                    this.cursor_visible = !this.cursor_visible;
                    cx.notify();
                }) else {
                    break;
                };
            }
        })
        .detach();
    }

    /// Reset cursor to visible (called on keyboard input so the cursor
    /// doesn't blink while typing).
    fn reset_cursor_blink(&mut self, cx: &mut Context<Self>) {
        if !self.cursor_visible {
            self.cursor_visible = true;
            cx.notify();
        }
    }

    fn encode_keystroke(keystroke: &Keystroke) -> Option<Vec<u8>> {
        let key = keystroke.key.as_str();
        let modifiers = &keystroke.modifiers;

        if modifiers.control {
            return match key {
                "c" => Some(vec![0x03]),
                "d" => Some(vec![0x04]),
                "z" => Some(vec![0x1a]),
                "l" => Some(vec![0x0c]),
                "a" => Some(vec![0x01]),
                "e" => Some(vec![0x05]),
                "k" => Some(vec![0x0b]),
                "u" => Some(vec![0x15]),
                "w" => Some(vec![0x17]),
                "r" => Some(vec![0x12]),
                "p" => Some(vec![0x10]),
                "n" => Some(vec![0x0e]),
                _ => None,
            };
        }

        match key {
            "enter" => Some(vec![b'\r']),
            "tab" => Some(vec![b'\t']),
            "backspace" => Some(vec![0x7f]),
            "escape" => Some(vec![0x1b]),
            "space" => Some(vec![b' ']),
            "up" => Some(b"\x1b[A".to_vec()),
            "down" => Some(b"\x1b[B".to_vec()),
            "right" => Some(b"\x1b[C".to_vec()),
            "left" => Some(b"\x1b[D".to_vec()),
            "home" => Some(b"\x1b[H".to_vec()),
            "end" => Some(b"\x1b[F".to_vec()),
            "pageup" => Some(b"\x1b[5~".to_vec()),
            "pagedown" => Some(b"\x1b[6~".to_vec()),
            "delete" => Some(b"\x1b[3~".to_vec()),
            _ => {
                if let Some(ch) = &keystroke.key_char {
                    Some(ch.as_bytes().to_vec())
                } else if key.len() == 1 {
                    Some(key.as_bytes().to_vec())
                } else {
                    None
                }
            }
        }
    }
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_output_reader(cx);
        self.start_cursor_blink(cx);

        // Auto-focus on first render
        if !self.focus_handle.is_focused(window) && !self.closed {
            self.focus_handle.focus(window);
        }

        let terminal_el = TerminalElement::new(
            self.term.clone(),
            self.ws_handle.resize_tx.clone(),
            self.cursor_visible,
        );

        let mut content = div()
            .id("terminal-content")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::terminal_bg())
            .p(px(4.0))
            .overflow_hidden()
            .on_key_down({
                let input_tx = self.ws_handle.input_tx.clone();
                move |event: &KeyDownEvent, _window: &mut Window, _cx: &mut App| {
                    if let Some(bytes) = TerminalPanel::encode_keystroke(&event.keystroke) {
                        let _ = input_tx.send(bytes);
                    }
                }
            })
            .on_any_mouse_down({
                let focus = self.focus_handle.clone();
                move |_event: &MouseDownEvent, window: &mut Window, _cx: &mut App| {
                    focus.focus(window);
                }
            })
            .on_scroll_wheel({
                let term = self.term.clone();
                let entity_id = cx.entity_id();
                move |event: &ScrollWheelEvent, _window: &mut Window, cx: &mut App| {
                    let delta_lines = match event.delta {
                        ScrollDelta::Lines(delta) => {
                            // Negative y = scroll up (show older content).
                            // Negate because alacritty Scroll::Delta positive = scroll up.
                            (-delta.y * SCROLL_LINES_PER_TICK as f32).round() as i32
                        }
                        ScrollDelta::Pixels(delta) => {
                            // Convert pixel delta to lines (approximate with 18px line height).
                            let dy: f32 = delta.y.into();
                            (-dy / 18.0).round() as i32
                        }
                    };

                    if delta_lines != 0 {
                        if let Ok(mut term) = term.lock() {
                            term.scroll_display(Scroll::Delta(delta_lines));
                        }
                        cx.notify(entity_id);
                    }
                }
            })
            .focus(|style| style.border_color(theme::accent()).border_1())
            .child(terminal_el);

        if self.closed {
            content = content.child(
                div()
                    .pt(px(8.0))
                    .text_color(theme::text_tertiary())
                    .text_size(px(12.0))
                    .child("[Session closed]"),
            );
        }

        content
    }
}
