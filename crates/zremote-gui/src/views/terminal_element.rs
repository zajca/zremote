use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags as CellFlags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};
use gpui::*;

use crate::theme;
use crate::views::terminal_panel::TerminalLayoutInfo;

const FONT_SIZE: f32 = 14.0;
const FONT_FAMILY: &str = "JetBrainsMono Nerd Font Mono";
const CURSOR_BAR_WIDTH: f32 = 2.0;
const CURSOR_UNDERLINE_HEIGHT: f32 = 2.0;
/// Extra vertical padding per cell for comfortable line spacing (logical pixels).
const CELL_PADDING_Y: f32 = 0.0;

/// A run of consecutive cells on the same row sharing the same style.
struct CellRun {
    text: String,
    fg: Hsla,
    bg: Option<Hsla>,
    bold: bool,
    italic: bool,
    dim: bool,
    underline: bool,
    wavy_underline: bool,
    strikethrough: bool,
    /// All characters in this run are double-width (CJK / emoji).
    wide: bool,
    col_start: usize,
    col_count: usize,
    row: usize,
}

/// Custom GPUI Element that renders terminal cells on a fixed monospace pixel grid.
/// Fills available parent space and dynamically resizes the alacritty Term to fit.
pub struct TerminalElement {
    term: Arc<Mutex<alacritty_terminal::Term<VoidListener>>>,
    resize_tx: flume::Sender<(u16, u16)>,
    /// Whether the cursor should be painted (controlled by blink timer in the panel).
    cursor_visible: bool,
    /// Shared layout info written during paint, read by mouse event handlers in the panel.
    layout_info: Rc<Cell<TerminalLayoutInfo>>,
}

pub struct TerminalElementLayoutState {
    cell_width: Pixels,
    cell_height: Pixels,
}

impl TerminalElement {
    pub fn new(
        term: Arc<Mutex<alacritty_terminal::Term<VoidListener>>>,
        resize_tx: flume::Sender<(u16, u16)>,
        cursor_visible: bool,
        layout_info: Rc<Cell<TerminalLayoutInfo>>,
    ) -> Self {
        Self {
            term,
            resize_tx,
            cursor_visible,
            layout_info,
        }
    }

    fn font() -> Font {
        Font {
            family: SharedString::from(FONT_FAMILY),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::default(),
            style: FontStyle::default(),
        }
    }

    fn bold_font() -> Font {
        Font {
            family: SharedString::from(FONT_FAMILY),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::BOLD,
            style: FontStyle::default(),
        }
    }

    fn italic_font() -> Font {
        Font {
            family: SharedString::from(FONT_FAMILY),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::default(),
            style: FontStyle::Italic,
        }
    }

    fn bold_italic_font() -> Font {
        Font {
            family: SharedString::from(FONT_FAMILY),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::BOLD,
            style: FontStyle::Italic,
        }
    }

    /// Measure monospace cell dimensions using font metrics.
    /// Uses advance() for precise glyph advance width instead of shape_line layout width.
    /// Font metrics (advance, ascent, descent) are computed from font_size in logical pixels,
    /// so no scale factor correction is needed.
    /// Adds CELL_PADDING_Y for comfortable line spacing.
    pub fn measure_cell(window: &mut Window) -> Option<(Pixels, Pixels)> {
        let text_system = window.text_system();
        let font = Self::font();
        let font_size = px(FONT_SIZE);
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system.advance(font_id, font_size, 'M').ok()?.width;
        let ascent = text_system.ascent(font_id, font_size);
        let descent = text_system.descent(font_id, font_size);
        let cell_height = ascent + descent.abs() + px(CELL_PADDING_Y);
        Some((cell_width, cell_height))
    }

    /// Internal cell measurement returning (width, height).
    fn measure_cell_internal(window: &mut Window) -> (Pixels, Pixels) {
        Self::measure_cell(window).unwrap_or((px(8.4), px(18.0)))
    }

    /// Extract cell runs from the terminal grid, batching adjacent cells with the same style.
    /// Accounts for `display_offset` so scrolled-back content is rendered correctly.
    fn build_cell_runs(
        term: &alacritty_terminal::Term<VoidListener>,
    ) -> Vec<CellRun> {
        let cols = term.columns();
        let rows = term.screen_lines();
        let display_offset = term.grid().display_offset() as i32;
        let bg_default = ansi_to_hsla(AnsiColor::Named(NamedColor::Background));
        let mut runs = Vec::new();

        for row in 0..rows {
            let mut current: Option<CellRun> = None;

            for col in 0..cols {
                // Adjust line index by display_offset to read scrollback content.
                // When display_offset > 0, viewport row 0 maps to Line(-display_offset)
                // in the grid (scrollback history).
                let point = Point::new(Line(row as i32 - display_offset), Column(col));
                let cell = &term.grid()[point];
                let flags = cell.flags;

                // Skip spacer cells for wide characters - extend previous run
                if flags.contains(CellFlags::WIDE_CHAR_SPACER) {
                    if let Some(ref mut run) = current {
                        run.col_count += 1;
                    }
                    continue;
                }

                let ch = if cell.c == '\0' { ' ' } else { cell.c };
                let bold = flags.contains(CellFlags::BOLD);
                let italic = flags.contains(CellFlags::ITALIC);
                let dim = flags.contains(CellFlags::DIM);
                let wide = flags.contains(CellFlags::WIDE_CHAR);
                let underline = flags.intersects(
                    CellFlags::UNDERLINE
                        | CellFlags::DOUBLE_UNDERLINE
                        | CellFlags::UNDERCURL
                        | CellFlags::DOTTED_UNDERLINE
                        | CellFlags::DASHED_UNDERLINE,
                );
                let wavy_underline = flags.contains(CellFlags::UNDERCURL);
                let strikethrough = flags.contains(CellFlags::STRIKEOUT);
                let inverse = flags.contains(CellFlags::INVERSE);
                let hidden = flags.contains(CellFlags::HIDDEN);

                // Resolve colors, handling INVERSE (swap fg/bg)
                let (mut fg, bg_color) = if inverse {
                    // Reverse video: swap foreground and background colors
                    (ansi_to_hsla(cell.bg), ansi_to_hsla(cell.fg))
                } else {
                    (ansi_to_hsla(cell.fg), ansi_to_hsla(cell.bg))
                };

                // Hidden text: make fg match bg so text is invisible
                if hidden {
                    fg = bg_color;
                }

                let bg = if color_eq(bg_color, bg_default) {
                    None
                } else {
                    Some(bg_color)
                };

                // Check if current run can be extended
                if let Some(ref mut run) = current {
                    if color_eq(fg, run.fg)
                        && run.bg == bg
                        && run.bold == bold
                        && run.italic == italic
                        && run.dim == dim
                        && run.underline == underline
                        && run.wavy_underline == wavy_underline
                        && run.strikethrough == strikethrough
                        && run.wide == wide
                    {
                        run.text.push(ch);
                        run.col_count += 1;
                        continue;
                    }
                    let finished = current.take().unwrap();
                    runs.push(finished);
                }

                current = Some(CellRun {
                    text: String::from(ch),
                    fg,
                    bg,
                    bold,
                    italic,
                    dim,
                    underline,
                    wavy_underline,
                    strikethrough,
                    wide,
                    col_start: col,
                    col_count: 1,
                    row,
                });
            }

            if let Some(run) = current.take() {
                runs.push(run);
            }
        }

        runs
    }

    fn paint_backgrounds(
        runs: &[CellRun],
        bounds: &Bounds<Pixels>,
        cell_width: Pixels,
        cell_height: Pixels,
        window: &mut Window,
    ) {
        for run in runs {
            if let Some(bg) = run.bg {
                let x = bounds.origin.x + cell_width * run.col_start as f32;
                let y = bounds.origin.y + cell_height * run.row as f32;
                let w = cell_width * run.col_count as f32;
                let quad_bounds = Bounds::new(point(x, y), size(w, cell_height));
                window.paint_quad(fill(quad_bounds, bg));
            }
        }
    }

    fn paint_text(
        runs: &[CellRun],
        bounds: &Bounds<Pixels>,
        cell_width: Pixels,
        cell_height: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) {
        let font_size = px(FONT_SIZE);
        let normal_font = Self::font();
        let bold_font = Self::bold_font();
        let italic_font = Self::italic_font();
        let bold_italic_font = Self::bold_italic_font();

        for run in runs {
            if run.text.chars().all(|c| c == ' ') {
                continue;
            }

            let font = match (run.bold, run.italic) {
                (true, true) => bold_italic_font.clone(),
                (true, false) => bold_font.clone(),
                (false, true) => italic_font.clone(),
                (false, false) => normal_font.clone(),
            };

            let mut color = run.fg;
            if run.dim {
                color.a *= 0.6;
            }

            let underline = if run.underline {
                Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(color),
                    wavy: run.wavy_underline,
                })
            } else {
                None
            };

            let strikethrough = if run.strikethrough {
                Some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(color),
                })
            } else {
                None
            };

            let text_run = TextRun {
                len: run.text.len(),
                font,
                color,
                background_color: None,
                underline,
                strikethrough,
            };

            // Wide characters need 2x cell width for glyph positioning
            let glyph_width = if run.wide {
                cell_width * 2.0
            } else {
                cell_width
            };

            let shaped = window.text_system().shape_line(
                SharedString::from(run.text.clone()),
                font_size,
                &[text_run],
                Some(glyph_width),
            );

            let x = bounds.origin.x + cell_width * run.col_start as f32;
            let y = bounds.origin.y + cell_height * run.row as f32;
            let origin = point(x, y);

            let _ = shaped.paint(origin, cell_height, window, cx);
        }
    }

    /// Paint semi-transparent highlight rectangles over selected cells.
    fn paint_selection(
        term: &alacritty_terminal::Term<VoidListener>,
        bounds: &Bounds<Pixels>,
        cell_width: Pixels,
        cell_height: Pixels,
        window: &mut Window,
    ) {
        let content = term.renderable_content();
        let selection = match content.selection {
            Some(sel) => sel,
            None => return,
        };

        let cols = term.columns();
        let rows = term.screen_lines();
        let display_offset = term.grid().display_offset() as i32;

        // Selection highlight color: semi-transparent white
        let highlight = hsla(0.6, 0.5, 0.5, 0.35);

        // The selection range is in absolute grid coordinates.
        // Convert to viewport-relative rows for painting.
        for viewport_row in 0..rows {
            let grid_line = Line(viewport_row as i32 - display_offset);

            // Determine the column range selected on this line
            let (col_start, col_end) = if selection.is_block {
                // Block selection: same column range on every line within the selection
                if grid_line < selection.start.line || grid_line > selection.end.line {
                    continue;
                }
                (selection.start.column.0, selection.end.column.0)
            } else {
                // Simple/semantic/lines selection
                if grid_line < selection.start.line || grid_line > selection.end.line {
                    continue;
                }

                let start_col = if grid_line == selection.start.line {
                    selection.start.column.0
                } else {
                    0
                };

                let end_col = if grid_line == selection.end.line {
                    selection.end.column.0
                } else {
                    cols.saturating_sub(1)
                };

                (start_col, end_col)
            };

            if col_start > col_end {
                continue;
            }

            let x = bounds.origin.x + cell_width * col_start as f32;
            let y = bounds.origin.y + cell_height * viewport_row as f32;
            let w = cell_width * (col_end - col_start + 1) as f32;
            let rect = Bounds::new(point(x, y), size(w, cell_height));
            window.paint_quad(fill(rect, highlight));
        }
    }

    fn paint_cursor(
        term: &alacritty_terminal::Term<VoidListener>,
        bounds: &Bounds<Pixels>,
        cell_width: Pixels,
        cell_height: Pixels,
        window: &mut Window,
    ) {
        // Hide cursor when scrolled back into history.
        if term.grid().display_offset() > 0 {
            return;
        }

        let content = term.renderable_content();
        let cursor = &content.cursor;

        if cursor.shape == CursorShape::Hidden {
            return;
        }

        let cursor_color: Hsla = theme::terminal_cursor().into();
        let col = cursor.point.column.0;
        let row = cursor.point.line.0;

        let x = bounds.origin.x + cell_width * col as f32;
        let y = bounds.origin.y + cell_height * row as f32;

        match cursor.shape {
            CursorShape::Block | CursorShape::HollowBlock => {
                let cursor_bounds = Bounds::new(point(x, y), size(cell_width, cell_height));
                if cursor.shape == CursorShape::Block {
                    let mut bg = cursor_color;
                    bg.a = 0.5;
                    window.paint_quad(fill(cursor_bounds, bg));
                } else {
                    window.paint_quad(outline(cursor_bounds, cursor_color, BorderStyle::default()));
                }
            }
            CursorShape::Beam => {
                let bar_bounds = Bounds::new(
                    point(x, y),
                    size(px(CURSOR_BAR_WIDTH), cell_height),
                );
                window.paint_quad(fill(bar_bounds, cursor_color));
            }
            CursorShape::Underline => {
                let underline_y = y + cell_height - px(CURSOR_UNDERLINE_HEIGHT);
                let underline_bounds = Bounds::new(
                    point(x, underline_y),
                    size(cell_width, px(CURSOR_UNDERLINE_HEIGHT)),
                );
                window.paint_quad(fill(underline_bounds, cursor_color));
            }
            CursorShape::Hidden => {}
        }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = TerminalElementLayoutState;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (cell_width, cell_height) = Self::measure_cell_internal(window);

        // Fill available parent space
        let style = Style {
            flex_grow: 1.0,
            size: Size {
                width: Length::Auto,
                height: Length::Auto,
            },
            overflow: gpui::Point {
                x: Overflow::Hidden,
                y: Overflow::Hidden,
            },
            ..Style::default()
        };

        let layout_id = window.request_layout(style, [], cx);

        let state = TerminalElementLayoutState {
            cell_width,
            cell_height,
        };

        (layout_id, state)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        // Calculate how many cells fit in the available bounds
        let new_cols = (bounds.size.width / state.cell_width).floor() as u16;
        let new_rows = (bounds.size.height / state.cell_height).floor() as u16;

        if new_cols > 0 && new_rows > 0 && let Ok(mut term) = self.term.lock() {
            let current_cols = term.columns() as u16;
            let current_rows = term.screen_lines() as u16;

            if new_cols != current_cols || new_rows != current_rows {
                let size = TermSize::new(
                    usize::from(new_cols),
                    usize::from(new_rows),
                );
                term.resize(size);
                let _ = self.resize_tx.send((new_cols, new_rows));
            }
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let cell_width = state.cell_width;
        let cell_height = state.cell_height;

        // Store layout info for mouse event handlers in the panel.
        self.layout_info.set(TerminalLayoutInfo {
            cell_width,
            cell_height,
            bounds,
        });

        // Paint terminal background
        let bg: Hsla = theme::terminal_bg().into();
        window.paint_quad(fill(bounds, bg));

        let term = self.term.lock().unwrap();

        // Build batched cell runs (reads rows/cols from term directly)
        let runs = Self::build_cell_runs(&term);

        // Paint cell backgrounds
        Self::paint_backgrounds(&runs, &bounds, cell_width, cell_height, window);

        // Paint selection highlight (between backgrounds and cursor)
        Self::paint_selection(&term, &bounds, cell_width, cell_height, window);

        // Paint text
        Self::paint_text(&runs, &bounds, cell_width, cell_height, window, cx);

        // Paint cursor (skip when blink timer has it hidden)
        if self.cursor_visible {
            Self::paint_cursor(&term, &bounds, cell_width, cell_height, window);
        }
    }
}

/// Convert alacritty ANSI color to GPUI Hsla.
fn ansi_to_hsla(color: AnsiColor) -> Hsla {
    match color {
        AnsiColor::Named(name) => named_color_to_hsla(name),
        AnsiColor::Spec(rgb) => {
            let rgba = Rgba {
                r: f32::from(rgb.r) / 255.0,
                g: f32::from(rgb.g) / 255.0,
                b: f32::from(rgb.b) / 255.0,
                a: 1.0,
            };
            rgba.into()
        }
        AnsiColor::Indexed(idx) => indexed_color_to_hsla(idx),
    }
}

fn named_color_to_hsla(name: NamedColor) -> Hsla {
    let rgba = match name {
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
    };
    rgba.into()
}

fn indexed_color_to_hsla(idx: u8) -> Hsla {
    if idx < 16 {
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
        return named_color_to_hsla(named);
    }

    if idx < 232 {
        let i = idx - 16;
        let r = (i / 36) * 51;
        let g = ((i / 6) % 6) * 51;
        let b = (i % 6) * 51;
        let rgba = Rgba {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        };
        return rgba.into();
    }

    let gray = 8 + (idx - 232) * 10;
    let rgba = Rgba {
        r: f32::from(gray) / 255.0,
        g: f32::from(gray) / 255.0,
        b: f32::from(gray) / 255.0,
        a: 1.0,
    };
    rgba.into()
}

/// Compare two Hsla colors for approximate equality.
fn color_eq(a: Hsla, b: Hsla) -> bool {
    (a.h - b.h).abs() < 0.001
        && (a.s - b.s).abs() < 0.001
        && (a.l - b.l).abs() < 0.001
        && (a.a - b.a).abs() < 0.001
}
