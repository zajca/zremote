use std::sync::{Arc, Mutex};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::term::cell::Flags as CellFlags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor};
use gpui::*;

use crate::terminal_ws::{self, TerminalWsHandle};
use crate::theme;
use crate::types::TerminalEvent;

/// Default terminal dimensions.
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

/// Cell height for monospace font at 14px.
const CELL_HEIGHT: f32 = 18.0;

pub struct TerminalPanel {
    session_id: String,
    term: Arc<Mutex<alacritty_terminal::Term<VoidListener>>>,
    ws_handle: TerminalWsHandle,
    rows: u16,
    closed: bool,
    reader_started: bool,
}

impl TerminalPanel {
    pub fn new(session_id: String, ws_url: String, tokio_handle: &tokio::runtime::Handle) -> Self {
        let config = TermConfig::default();
        let size = TermSize::new(usize::from(DEFAULT_COLS), usize::from(DEFAULT_ROWS));
        let term = alacritty_terminal::Term::new(config, &size, VoidListener);
        let term = Arc::new(Mutex::new(term));

        let ws_handle = terminal_ws::connect(ws_url, tokio_handle);

        Self {
            session_id,
            term,
            ws_handle,
            rows: DEFAULT_ROWS,
            closed: false,
            reader_started: false,
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

    /// Build styled row elements from the terminal grid.
    /// Groups consecutive cells with the same style into spans for efficiency.
    fn render_row(&self, row_idx: i32, term: &alacritty_terminal::Term<VoidListener>) -> Div {
        let cols = term.columns();
        let mut spans: Vec<AnyElement> = Vec::new();
        let mut current_text = String::new();
        let mut current_fg = AnsiColor::Named(NamedColor::Foreground);
        let mut current_flags = CellFlags::empty();
        let mut first = true;

        for col in 0..cols {
            let point = Point::new(Line(row_idx), Column(col));
            let cell = &term.grid()[point];
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            let fg = cell.fg;
            let flags = cell.flags;

            if first {
                current_fg = fg;
                current_flags = flags;
                first = false;
            }

            // When style changes, flush the current span
            if fg != current_fg || flags != current_flags {
                if !current_text.is_empty() {
                    spans.push(self.make_span(&current_text, current_fg, current_flags));
                    current_text.clear();
                }
                current_fg = fg;
                current_flags = flags;
            }

            current_text.push(ch);
        }

        // Flush remaining text (trim trailing spaces)
        let trimmed = current_text.trim_end();
        if !trimmed.is_empty() {
            spans.push(self.make_span(trimmed, current_fg, current_flags));
        }

        // If the entire row is empty, render a space to maintain height
        if spans.is_empty() {
            spans.push(div().child(SharedString::from(" ")).into_any_element());
        }

        div()
            .w_full()
            .h(px(CELL_HEIGHT))
            .flex()
            .text_size(px(14.0))
            .children(spans)
    }

    fn make_span(&self, text: &str, fg: AnsiColor, flags: CellFlags) -> AnyElement {
        let color = ansi_to_gpui_color(fg);
        let mut el = div()
            .text_color(color)
            .child(SharedString::from(text.to_string()));

        if flags.contains(CellFlags::BOLD) {
            el = el.font_weight(FontWeight::BOLD);
        }

        if flags.contains(CellFlags::DIM) {
            el = el.opacity(0.6);
        }

        el.into_any_element()
    }
}

/// Convert alacritty ANSI color to GPUI Rgba.
fn ansi_to_gpui_color(color: AnsiColor) -> Rgba {
    match color {
        AnsiColor::Named(name) => named_color_to_rgba(name),
        AnsiColor::Spec(rgb) => Rgba {
            r: f32::from(rgb.r) / 255.0,
            g: f32::from(rgb.g) / 255.0,
            b: f32::from(rgb.b) / 255.0,
            a: 1.0,
        },
        AnsiColor::Indexed(idx) => indexed_color_to_rgba(idx),
    }
}

fn named_color_to_rgba(name: NamedColor) -> Rgba {
    match name {
        NamedColor::Black => rgb(0x1a1a1e),
        NamedColor::Red => rgb(0xef4444),
        NamedColor::Green => rgb(0x4ade80),
        NamedColor::Yellow => rgb(0xfacc15),
        NamedColor::Blue => rgb(0x60a5fa),
        NamedColor::Magenta => rgb(0xc084fc),
        NamedColor::Cyan => rgb(0x22d3ee),
        NamedColor::White => rgb(0xcccccc),
        NamedColor::BrightBlack => rgb(0x555555),
        NamedColor::BrightRed => rgb(0xf87171),
        NamedColor::BrightGreen => rgb(0x86efac),
        NamedColor::BrightYellow => rgb(0xfde68a),
        NamedColor::BrightBlue => rgb(0x93c5fd),
        NamedColor::BrightMagenta => rgb(0xd8b4fe),
        NamedColor::BrightCyan => rgb(0x67e8f9),
        NamedColor::BrightWhite => rgb(0xffffff),
        NamedColor::Foreground | NamedColor::BrightForeground => rgb(0xeeeeee),
        NamedColor::Background => rgb(0x0a0a0b),
        NamedColor::Cursor => rgb(0xcccccc),
        NamedColor::DimBlack => rgb(0x111111),
        NamedColor::DimRed => rgb(0xb91c1c),
        NamedColor::DimGreen => rgb(0x16a34a),
        NamedColor::DimYellow => rgb(0xca8a04),
        NamedColor::DimBlue => rgb(0x2563eb),
        NamedColor::DimMagenta => rgb(0x9333ea),
        NamedColor::DimCyan => rgb(0x0891b2),
        NamedColor::DimWhite => rgb(0x888888),
        NamedColor::DimForeground => rgb(0x888888),
    }
}

fn indexed_color_to_rgba(idx: u8) -> Rgba {
    // 16 standard colors -> delegate to named
    if idx < 16 {
        // Safety: NamedColor maps 0-15 to the 16 standard colors
        let named = match idx {
            0 => NamedColor::Black,
            1 => NamedColor::Red,
            2 => NamedColor::Green,
            3 => NamedColor::Yellow,
            4 => NamedColor::Blue,
            5 => NamedColor::Magenta,
            6 => NamedColor::Cyan,
            7 => NamedColor::White,
            8 => NamedColor::BrightBlack,
            9 => NamedColor::BrightRed,
            10 => NamedColor::BrightGreen,
            11 => NamedColor::BrightYellow,
            12 => NamedColor::BrightBlue,
            13 => NamedColor::BrightMagenta,
            14 => NamedColor::BrightCyan,
            _ => NamedColor::BrightWhite,
        };
        return named_color_to_rgba(named);
    }

    // 216 color cube (indices 16-231)
    if idx < 232 {
        let idx = idx - 16;
        let r = (idx / 36) * 51;
        let g = ((idx / 6) % 6) * 51;
        let b = (idx % 6) * 51;
        return Rgba {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        };
    }

    // 24 grayscale (indices 232-255)
    let gray = 8 + (idx - 232) * 10;
    Rgba {
        r: f32::from(gray) / 255.0,
        g: f32::from(gray) / 255.0,
        b: f32::from(gray) / 255.0,
        a: 1.0,
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
