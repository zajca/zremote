use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
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
/// Thickness of underline and strikethrough decorations in logical pixels.
const DECORATION_THICKNESS: f32 = 1.0;

/// Number of recent (display_offset, content_generation) pairs to cache cell runs for.
/// Allows back-and-forth scrolling to hit cached runs for recently visited offsets.
const CELL_RUN_CACHE_SLOTS: usize = 8;

/// A run of consecutive cells on the same row sharing the same style.
#[derive(Clone)]
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

/// LRU cache for cell runs, storing the last N (display_offset, content_generation) snapshots.
/// During scrollback, each display_offset produces different cell runs. By caching recent
/// offsets, back-and-forth scrolling hits the cache instead of rebuilding from the grid.
pub struct CellRunCache {
    /// Ring buffer of cached snapshots, ordered by recency (most recent first).
    slots: Vec<CellRunCacheEntry>,
}

struct CellRunCacheEntry {
    display_offset: usize,
    content_generation: u64,
    runs: Vec<CellRun>,
}

impl CellRunCache {
    pub fn new() -> Self {
        Self {
            slots: Vec::with_capacity(CELL_RUN_CACHE_SLOTS),
        }
    }

    /// Look up cached cell runs for the given (display_offset, content_generation).
    /// Returns None on cache miss.
    fn get(&self, display_offset: usize, content_generation: u64) -> Option<&[CellRun]> {
        self.slots
            .iter()
            .find(|e| {
                e.display_offset == display_offset && e.content_generation == content_generation
            })
            .map(|e| e.runs.as_slice())
    }

    /// Insert cell runs for the given key. Evicts the oldest entry if at capacity.
    fn insert(&mut self, display_offset: usize, content_generation: u64, runs: Vec<CellRun>) {
        // Remove existing entry for this key (promote to front).
        self.slots
            .retain(|e| e.display_offset != display_offset || e.content_generation != content_generation);

        // Evict oldest if at capacity.
        if self.slots.len() >= CELL_RUN_CACHE_SLOTS {
            self.slots.pop();
        }

        // Insert at front (most recent).
        self.slots.insert(
            0,
            CellRunCacheEntry {
                display_offset,
                content_generation,
                runs,
            },
        );
    }
}

/// Cache key for per-character glyph cache.
/// Includes color bits so that the same character with different colors gets
/// separate cache entries. Without this, whichever color was shaped first
/// would be used for all occurrences of that character.
type GlyphCacheKey = (char, bool, bool, bool, u32, u32, u32, u32);

/// Convert an Hsla color to cache key components (bit patterns of h, s, l, a).
fn hsla_to_cache_bits(color: Hsla) -> (u32, u32, u32, u32) {
    (
        color.h.to_bits(),
        color.s.to_bits(),
        color.l.to_bits(),
        color.a.to_bits(),
    )
}

/// Per-character glyph cache for monospace terminal rendering.
///
/// In a monospace terminal, each character with a given style (bold, italic, wide)
/// and color always produces the same shaped glyph. Instead of shaping entire text
/// runs (which creates unique cache keys for every distinct sentence/word), we shape
/// individual characters. This results in a bounded cache (~200 chars * ~20 colors
/// * 4 styles = ~16,000 entries max), with nearly 100% hit rate after the first
/// frame. No eviction is needed -- the cache is small.
pub struct GlyphCache {
    entries: HashMap<GlyphCacheKey, ShapedLine>,
}

impl GlyphCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::with_capacity(512),
        }
    }

    /// Look up a shaped single-character line by (char, bold, italic, wide, color).
    fn get(
        &self,
        ch: char,
        bold: bool,
        italic: bool,
        wide: bool,
        color: Hsla,
    ) -> Option<&ShapedLine> {
        let (h, s, l, a) = hsla_to_cache_bits(color);
        self.entries.get(&(ch, bold, italic, wide, h, s, l, a))
    }

    /// Insert a shaped single-character line.
    fn insert(
        &mut self,
        ch: char,
        bold: bool,
        italic: bool,
        wide: bool,
        color: Hsla,
        shaped: ShapedLine,
    ) {
        let (h, s, l, a) = hsla_to_cache_bits(color);
        self.entries
            .insert((ch, bold, italic, wide, h, s, l, a), shaped);
    }
}

/// Custom GPUI Element that renders terminal cells on a fixed monospace pixel grid.
/// Fills available parent space and dynamically resizes the alacritty Term to fit.
///
/// Scrolling follows Zed's approach: pixel deltas from trackpad/mouse are accumulated
/// and converted to whole-line scroll deltas. No sub-pixel rendering offset is used --
/// alacritty's display_offset always moves by whole lines, and the grid is rendered
/// at exact line boundaries. This eliminates constant repaints from fractional offsets
/// and matches how native terminals feel.
pub struct TerminalElement {
    term: Arc<Mutex<alacritty_terminal::Term<VoidListener>>>,
    resize_tx: flume::Sender<(u16, u16)>,
    /// Whether the cursor should be painted (controlled by blink timer in the panel).
    cursor_visible: bool,
    /// Shared layout info written during paint, read by mouse event handlers in the panel.
    layout_info: Rc<Cell<TerminalLayoutInfo>>,
    /// Pending whole-line scroll delta from event handlers (applied in paint under single lock).
    pending_scroll_delta: Arc<AtomicI32>,
    /// Content generation counter (bumped on PTY output) for cache invalidation.
    content_generation: Arc<AtomicU64>,
    /// Cached cell runs from recent frames (LRU over last N display_offsets).
    cell_run_cache: Rc<std::cell::RefCell<CellRunCache>>,
    /// Per-character glyph cache: ~200-500 entries, nearly 100% hit rate after first frame.
    glyph_cache: Rc<std::cell::RefCell<GlyphCache>>,
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
        pending_scroll_delta: Arc<AtomicI32>,
        content_generation: Arc<AtomicU64>,
        cell_run_cache: Rc<std::cell::RefCell<CellRunCache>>,
        glyph_cache: Rc<std::cell::RefCell<GlyphCache>>,
    ) -> Self {
        Self {
            term,
            resize_tx,
            cursor_visible,
            layout_info,
            pending_scroll_delta,
            content_generation,
            cell_run_cache,
            glyph_cache,
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
        let total_rows = rows;
        let bg_default = ansi_to_hsla(AnsiColor::Named(NamedColor::Background));

        // Pre-allocate: a typical row has 3-8 runs due to color/style changes.
        // Use rows * 5 to reduce reallocations for content with frequent style changes.
        let mut runs = Vec::with_capacity(total_rows * 5);

        // The grid's valid range is topmost_line..=bottommost_line.
        // topmost_line = Line(-(history_size as i32))
        // bottommost_line = Line(screen_lines - 1)
        let bottommost = rows as i32 - 1;

        for row in 0..total_rows {
            // Check that the line we're about to access is within the grid bounds.
            let line_idx = row as i32 - display_offset;
            if line_idx > bottommost {
                break; // Past the bottom of the grid; no more rows to render.
            }

            let mut current: Option<CellRun> = None;

            for col in 0..cols {
                // Adjust line index by display_offset to read scrollback content.
                // When display_offset > 0, viewport row 0 maps to Line(-display_offset)
                // in the grid (scrollback history).
                let point = Point::new(Line(line_idx), Column(col));
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

                // Start a new run. Pre-allocate for typical run length (~40 chars).
                // Runs can span many columns, especially for plain text or whitespace.
                let mut text = String::with_capacity(40);
                text.push(ch);
                current = Some(CellRun {
                    text,
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

    /// Paint text using a per-character glyph cache.
    ///
    /// Instead of shaping entire text runs (which creates unique cache keys for every
    /// distinct sentence), we shape individual characters. In a monospace terminal,
    /// each (char, bold, italic, wide) combination always produces the same glyph.
    /// This yields ~200-500 total cache entries with nearly 100% hit rate after the
    /// first frame, eliminating the ~10ms-per-miss `shape_line()` stutter on scroll.
    ///
    /// Underline and strikethrough are painted as simple rectangles, decoupled from
    /// text shaping entirely.
    fn paint_text(
        runs: &[CellRun],
        bounds: &Bounds<Pixels>,
        cell_width: Pixels,
        cell_height: Pixels,
        glyph_cache: &Rc<std::cell::RefCell<GlyphCache>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let font_size = px(FONT_SIZE);
        let normal_font = Self::font();
        let bold_font = Self::bold_font();
        let italic_font = Self::italic_font();
        let bold_italic_font = Self::bold_italic_font();

        // Pre-compute font metrics for decoration positioning.
        // Scope the immutable borrow of window so it doesn't conflict with paint calls.
        let (ascent, descent) = {
            let text_system = window.text_system();
            let normal_font_id = text_system.resolve_font(&normal_font);
            let a = text_system.ascent(normal_font_id, font_size);
            let d = text_system.descent(normal_font_id, font_size);
            (a, d)
        };

        // First pass: shape any missing glyphs into the cache.
        // This borrows window immutably (via text_system) but does not paint.
        for run in runs {
            let glyph_width = if run.wide {
                cell_width * 2.0
            } else {
                cell_width
            };

            let mut color = run.fg;
            if run.dim {
                color.a *= 0.6;
            }

            for ch in run.text.chars() {
                if ch == ' ' {
                    continue;
                }

                let needs_shape = {
                    let cache = glyph_cache.borrow();
                    cache
                        .get(ch, run.bold, run.italic, run.wide, color)
                        .is_none()
                };

                if needs_shape {
                    let font = match (run.bold, run.italic) {
                        (true, true) => bold_italic_font.clone(),
                        (true, false) => bold_font.clone(),
                        (false, true) => italic_font.clone(),
                        (false, false) => normal_font.clone(),
                    };

                    let mut char_buf = [0u8; 4];
                    let char_str = ch.encode_utf8(&mut char_buf);
                    let text_run = TextRun {
                        len: char_str.len(),
                        font,
                        color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };

                    let shaped = window.text_system().shape_line(
                        SharedString::from(char_str.to_owned()),
                        font_size,
                        &[text_run],
                        Some(glyph_width),
                    );

                    glyph_cache
                        .borrow_mut()
                        .insert(ch, run.bold, run.italic, run.wide, color, shaped);
                }
            }
        }

        // Second pass: paint all characters and decorations.
        // Now we only borrow window mutably (via paint), and the glyph_cache immutably.
        for run in runs {
            let mut color = run.fg;
            if run.dim {
                color.a *= 0.6;
            }

            let run_y = bounds.origin.y + cell_height * run.row as f32;

            // Paint each non-space character from the glyph cache.
            let mut col_offset = 0usize;
            for ch in run.text.chars() {
                if ch != ' ' {
                    let x = bounds.origin.x + cell_width * (run.col_start + col_offset) as f32;
                    let origin = point(x, run_y);

                    let cache = glyph_cache.borrow();
                    if let Some(shaped) =
                        cache.get(ch, run.bold, run.italic, run.wide, color)
                    {
                        let _ = shaped.paint(origin, cell_height, window, cx);
                    }
                }

                col_offset += 1;
            }

            // Paint underline as a simple rectangle at baseline + descent.
            if run.underline {
                let underline_x =
                    bounds.origin.x + cell_width * run.col_start as f32;
                // Position underline just below the text baseline.
                let underline_y = run_y + ascent + descent.abs() - px(DECORATION_THICKNESS);
                let underline_w = cell_width * run.col_count as f32;
                let underline_bounds = Bounds::new(
                    point(underline_x, underline_y),
                    size(underline_w, px(DECORATION_THICKNESS)),
                );
                window.paint_quad(fill(underline_bounds, color));
            }

            // Paint strikethrough as a simple rectangle at vertical midpoint of text.
            if run.strikethrough {
                let strike_x =
                    bounds.origin.x + cell_width * run.col_start as f32;
                let strike_y = run_y + ascent * 0.5;
                let strike_w = cell_width * run.col_count as f32;
                let strike_bounds = Bounds::new(
                    point(strike_x, strike_y),
                    size(strike_w, px(DECORATION_THICKNESS)),
                );
                window.paint_quad(fill(strike_bounds, color));
            }
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

        // Read the current content generation for cache checks.
        let content_gen = self.content_generation.load(Ordering::Relaxed);

        // Apply deferred scroll delta under a single lock. This is the key optimization:
        // scroll event handlers no longer lock the mutex -- they atomically accumulate
        // a line delta which we drain here, reducing lock contention with the PTY output thread.
        let pending_delta = self.pending_scroll_delta.swap(0, Ordering::Relaxed);

        let mut term = self.term.lock().unwrap();

        if pending_delta != 0 {
            term.scroll_display(Scroll::Delta(pending_delta));
        }

        let display_offset = term.grid().display_offset();

        // Use cached cell runs if display_offset and content generation match a recent entry.
        let runs = {
            let cache = self.cell_run_cache.borrow();
            cache
                .get(display_offset, content_gen)
                .map(|runs| runs.to_vec())
        };

        let runs = runs.unwrap_or_else(|| {
            let new_runs = Self::build_cell_runs(&term);
            self.cell_run_cache
                .borrow_mut()
                .insert(display_offset, content_gen, new_runs.clone());
            new_runs
        });

        // Paint cell backgrounds
        Self::paint_backgrounds(&runs, &bounds, cell_width, cell_height, window);

        // Paint selection highlight (between backgrounds and cursor)
        Self::paint_selection(&term, &bounds, cell_width, cell_height, window);

        // Paint text using per-character glyph cache (~200-500 entries, nearly 100% hit rate).
        Self::paint_text(
            &runs,
            &bounds,
            cell_width,
            cell_height,
            &self.glyph_cache.clone(),
            window,
            cx,
        );

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
