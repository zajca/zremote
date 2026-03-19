use std::sync::{Arc, Mutex};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::Processor;
use gpui::*;

use crate::terminal_ws::{self, TerminalWsHandle};
use crate::theme;
use crate::types::TerminalEvent;

/// Default terminal dimensions (will be recalculated on layout).
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

/// Cell height for JetBrains Mono 14px (approximate).
const CELL_HEIGHT: f32 = 18.0;

/// Terminal panel: renders a terminal session using alacritty_terminal for VT parsing
/// and GPUI for GPU-accelerated rendering.
pub struct TerminalPanel {
    session_id: String,
    term: Arc<Mutex<alacritty_terminal::Term<VoidListener>>>,
    ws_handle: TerminalWsHandle,
    rows: u16,
    closed: bool,
}

impl TerminalPanel {
    pub fn new(session_id: String, ws_url: String, tokio_handle: &tokio::runtime::Handle) -> Self {
        // Create alacritty terminal state machine
        let config = TermConfig::default();
        let size = TermSize::new(usize::from(DEFAULT_COLS), usize::from(DEFAULT_ROWS));
        let term = alacritty_terminal::Term::new(config, &size, VoidListener);
        let term = Arc::new(Mutex::new(term));

        // Connect to terminal WebSocket
        let ws_handle = terminal_ws::connect(ws_url, tokio_handle);

        Self {
            session_id,
            term,
            ws_handle,
            rows: DEFAULT_ROWS,
            closed: false,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Start the background task that reads from the WebSocket and feeds data
    /// to the alacritty terminal state machine.
    pub fn start_output_reader(&self, cx: &mut Context<Self>) {
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

    /// Encode a GPUI keystroke into terminal byte sequences.
    fn encode_keystroke(keystroke: &Keystroke) -> Option<Vec<u8>> {
        let key = keystroke.key.as_str();
        let modifiers = &keystroke.modifiers;

        // Ctrl + key combinations
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

        // Special keys
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
                // Regular characters: use key_char if available, otherwise the key itself
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

    /// Render a single row of terminal cells as styled text.
    fn render_row(&self, row_idx: i32, term: &alacritty_terminal::Term<VoidListener>) -> Div {
        use alacritty_terminal::grid::Dimensions;
        use alacritty_terminal::index::{Column, Line, Point};

        let cols = term.columns();
        let mut line_text = String::with_capacity(cols);

        for col in 0..cols {
            let point = Point::new(Line(row_idx), Column(col));
            let cell = &term.grid()[point];
            let ch = cell.c;
            if ch == '\0' || ch == ' ' {
                line_text.push(' ');
            } else {
                line_text.push(ch);
            }
        }

        // Trim trailing spaces for cleaner rendering
        let trimmed = line_text.trim_end();

        div()
            .w_full()
            .h(px(CELL_HEIGHT))
            .text_color(theme::text_primary())
            .text_size(px(14.0))
            .child(if trimmed.is_empty() {
                SharedString::from(" ")
            } else {
                SharedString::from(trimmed.to_string())
            })
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Start output reader on first render
        self.start_output_reader(cx);

        let term = self.term.lock().unwrap();
        let rows = self.rows;

        let mut content = div()
            .id("terminal-content")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::terminal_bg())
            .font_family("JetBrains Mono")
            .text_size(px(14.0))
            .p(px(4.0))
            .overflow_hidden()
            .on_key_down({
                let input_tx = self.ws_handle.input_tx.clone();
                move |event: &KeyDownEvent, _window: &mut Window, _cx: &mut App| {
                    if let Some(bytes) = TerminalPanel::encode_keystroke(&event.keystroke) {
                        let _ = input_tx.send(bytes);
                    }
                }
            });

        for row in 0..i32::from(rows) {
            content = content.child(self.render_row(row, &term));
        }

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
