//! Terminal panel: input handling, WebSocket I/O, and state management.
//!
//! # Cache ownership
//!
//! Both [`CellRunCache`] and [`GlyphCache`] live here in `TerminalPanel` (which persists
//! across frames) and are shared with [`TerminalElement`] via `Rc<RefCell<>>`. This is
//! critical because GPUI recreates the element on every `render()` call -- if caches
//! lived in the element, they'd be empty every frame (0% hit rate, ~1s stutter from
//! reshaping 200+ text runs). The `Rc<RefCell<>>` pattern avoids mutex overhead since
//! rendering is single-threaded in GPUI.
//!
//! # Scroll event pipeline
//!
//! ```text
//! Trackpad/wheel event
//!   → on_scroll_wheel() handler
//!   → accumulate pixel delta in scroll_px: Rc<Cell<f32>>
//!   → when accumulated >= cell_height, convert to line delta
//!   → add line delta to pending_scroll_delta: Arc<AtomicI32> (lock-free)
//!   → cx.notify() schedules repaint
//!   → paint() drains AtomicI32, locks term once, calls scroll_display()
//! ```
//!
//! This pipeline avoids mutex lock contention between scroll events and the PTY output
//! reader thread (which also locks the term to feed data into the terminal emulator).
//!
//! # Cursor blink
//!
//! A detached async task toggles `cursor_visible` every 500ms using `Timer::after()`.
//! On any keystroke, `observe_keystrokes` resets `cursor_visible = true` so the cursor
//! stays solid while the user is actively typing. The blink timer continues independently
//! and resumes blinking naturally after typing stops.
//!
//! # Selection (mouse handling)
//!
//! Left click creates a selection at the grid position (supporting double-click for word
//! and triple-click for line selection). Mouse drag updates the selection endpoint.
//! Mouse up clears empty selections (plain click without drag) and auto-copies
//! non-empty selections to the system clipboard. Ctrl+C copies selected text
//! (or sends SIGINT when nothing is selected). Ctrl+V pastes from clipboard
//! with bracketed paste support.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::search::Match;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::Processor;
use gpui::*;

use crate::icons::{Icon, icon};
use crate::terminal_handle::TerminalHandle;
use crate::theme;
use crate::types::TerminalEvent;
use crate::views::command_palette::PaletteTab;
use crate::views::double_shift::DoubleShiftDetector;
use crate::views::terminal_element::{CellRunCache, GlyphCache, TerminalElement};
use crate::views::url_detector::UrlDetector;

/// Cursor blink interval (standard terminal blink rate).
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// Default terminal dimensions (used until first resize fits to container).
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

/// Lines to scroll per mouse wheel tick.
const SCROLL_LINES_PER_TICK: i32 = 3;

/// Debounce delay for WebSocket resize messages (ms).
/// Local term.resize() is immediate; only the server message is debounced.
const RESIZE_DEBOUNCE_MS: u64 = 150;

/// Shared layout info set by the element during paint, read by mouse event handlers.
#[derive(Clone, Copy, Default)]
pub struct TerminalLayoutInfo {
    pub cell_width: Pixels,
    pub cell_height: Pixels,
    pub bounds: Bounds<Pixels>,
}

pub struct TerminalPanel {
    session_id: String,
    term: Arc<Mutex<alacritty_terminal::Term<VoidListener>>>,
    handle: TerminalHandle,
    focus_handle: FocusHandle,
    closed: bool,
    reader_started: bool,
    /// Whether the cursor is currently visible (toggled by blink timer).
    cursor_visible: bool,
    /// Whether the blink timer task has been spawned.
    blink_started: bool,
    /// Layout info from the terminal element, shared via Rc<Cell> for mouse handlers.
    layout_info: Rc<Cell<TerminalLayoutInfo>>,
    /// Accumulated pixel scroll from trackpad (used to convert fractional pixels to
    /// whole-line deltas, following Zed's approach). Not used for sub-pixel rendering.
    scroll_px: Rc<Cell<f32>>,
    /// Pending whole-line scroll delta accumulated by event handlers (no mutex lock needed).
    /// Drained by paint() which applies it to the term under a single lock.
    pending_scroll_delta: Arc<AtomicI32>,
    /// Generation counter incremented when terminal content changes (PTY output, resize).
    /// Used by cell run cache to detect staleness.
    content_generation: Arc<AtomicU64>,
    /// Cached cell runs from recent frames (LRU, shared with TerminalElement).
    cell_run_cache: Rc<RefCell<CellRunCache>>,
    /// Per-character glyph cache (persists across renders, shared with TerminalElement).
    glyph_cache: Rc<RefCell<GlyphCache>>,
    /// Debounced resize sender (interposes between element and WS resize channel).
    resize_debounce_tx: flume::Sender<(u16, u16)>,
    /// URL detector for Ctrl+hover URL detection.
    url_detector: Rc<RefCell<UrlDetector>>,
    /// Index of the currently hovered URL match (shared with mouse handlers).
    hovered_url_idx: Rc<Cell<Option<usize>>>,
    /// Whether search overlay is open.
    search_open: bool,
    /// Search overlay view entity.
    search_overlay: Option<Entity<super::search_overlay::SearchOverlay>>,
    /// Current search matches in the terminal.
    search_matches: Vec<Match>,
    /// Index of the currently focused search match.
    search_current_idx: Option<usize>,
    /// Double-shift detection for command palette.
    double_shift: DoubleShiftDetector,
    /// Subscription handle for keystroke observation (reset cursor blink on input).
    _keystroke_subscription: Subscription,
}

impl TerminalPanel {
    pub fn new(
        session_id: String,
        handle: TerminalHandle,
        tokio_handle: &tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = TermConfig::default();
        let size = TermSize::new(usize::from(DEFAULT_COLS), usize::from(DEFAULT_ROWS));
        let term = alacritty_terminal::Term::new(config, &size, VoidListener);
        let term = Arc::new(Mutex::new(term));

        let focus_handle = cx.focus_handle();

        // Resize debouncing: element sends to debounce_tx (immediate local resize),
        // tokio task forwards to real resize_tx after 150ms of inactivity.
        let (resize_debounce_tx, resize_debounce_rx) = flume::bounded::<(u16, u16)>(4);
        let real_resize_tx = handle.resize_tx().clone();
        tokio_handle.spawn(async move {
            let mut first_resize = true;
            loop {
                // Wait for first resize event.
                let Ok(mut dims) = resize_debounce_rx.recv_async().await else {
                    break;
                };
                // Send very first resize immediately (skip debounce) so PTY knows
                // the correct size before any output arrives.
                if first_resize {
                    first_resize = false;
                    let _ = real_resize_tx.send(dims);
                    continue;
                }
                // Debounce: keep replacing dims while new events arrive within the timeout.
                loop {
                    match tokio::time::timeout(
                        Duration::from_millis(RESIZE_DEBOUNCE_MS),
                        resize_debounce_rx.recv_async(),
                    )
                    .await
                    {
                        Ok(Ok(new_dims)) => dims = new_dims,
                        Ok(Err(_)) => {
                            let _ = real_resize_tx.send(dims);
                            return;
                        }
                        Err(_) => {
                            let _ = real_resize_tx.send(dims);
                            break;
                        }
                    }
                }
            }
        });

        // Reset cursor to visible on any keystroke so it doesn't blink while typing.
        let keystroke_subscription = cx.observe_keystrokes(
            |this: &mut Self,
             _event: &KeystrokeEvent,
             _window: &mut Window,
             cx: &mut Context<Self>| {
                this.reset_cursor_blink(cx);
            },
        );

        Self {
            session_id,
            term,
            handle,
            focus_handle,
            closed: false,
            reader_started: false,
            cursor_visible: true,
            blink_started: false,
            layout_info: Rc::new(Cell::new(TerminalLayoutInfo::default())),
            scroll_px: Rc::new(Cell::new(0.0)),
            pending_scroll_delta: Arc::new(AtomicI32::new(0)),
            content_generation: Arc::new(AtomicU64::new(0)),
            cell_run_cache: Rc::new(RefCell::new(CellRunCache::new())),
            glyph_cache: Rc::new(RefCell::new(GlyphCache::new())),
            resize_debounce_tx,
            url_detector: Rc::new(RefCell::new(UrlDetector::new())),
            hovered_url_idx: Rc::new(Cell::new(None)),
            search_open: false,
            search_overlay: None,
            search_matches: Vec::new(),
            search_current_idx: None,
            double_shift: DoubleShiftDetector::new(),
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

        let output_rx = self.handle.output_rx().clone();
        let term = self.term.clone();
        let content_generation = self.content_generation.clone();

        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut processor: Processor = Processor::new();

            loop {
                match output_rx.recv_async().await {
                    Ok(TerminalEvent::Output(bytes)) => {
                        if let Ok(mut term) = term.lock() {
                            processor.advance(&mut *term, &bytes);
                        }
                        // Bump generation so cell run cache invalidates.
                        content_generation.fetch_add(1, Ordering::Relaxed);
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
                            let cols = term.columns();
                            let rows = term.screen_lines();
                            let size = if cols > 0 && rows > 0 {
                                TermSize::new(cols, rows)
                            } else {
                                TermSize::new(usize::from(DEFAULT_COLS), usize::from(DEFAULT_ROWS))
                            };
                            *term = alacritty_terminal::Term::new(
                                TermConfig::default(),
                                &size,
                                VoidListener,
                            );
                        }
                        // Invalidate cell run cache - term was recreated
                        content_generation.fetch_add(1, Ordering::Relaxed);
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

    /// Convert a mouse pixel position (relative to the terminal content origin)
    /// to a grid Point and Side. The padding offset must be subtracted by the caller.
    fn pixel_to_grid(
        position: gpui::Point<Pixels>,
        origin: gpui::Point<Pixels>,
        cell_width: Pixels,
        cell_height: Pixels,
        term_cols: usize,
        term_rows: usize,
        display_offset: usize,
    ) -> (Point, Side) {
        let rel_x = position.x - origin.x;
        let rel_y = position.y - origin.y;

        let col = (rel_x / cell_width).floor() as i32;
        let row = (rel_y / cell_height).floor() as i32;

        // Clamp to valid grid range
        let col = col.clamp(0, term_cols.saturating_sub(1) as i32) as usize;
        let row = row.clamp(0, term_rows.saturating_sub(1) as i32) as usize;

        // Determine which side of the cell the click is on
        let cell_x = rel_x - cell_width * col as f32;
        let side = if cell_x < cell_width / 2.0 {
            Side::Left
        } else {
            Side::Right
        };

        // Convert viewport row to grid line, accounting for scrollback offset.
        // When display_offset > 0, viewport row 0 maps to Line(-display_offset).
        let line = Line(row as i32 - display_offset as i32);
        let point = Point::new(line, Column(col));

        (point, side)
    }

    fn encode_keystroke(keystroke: &Keystroke) -> Option<Vec<u8>> {
        let key = keystroke.key.as_str();
        let modifiers = &keystroke.modifiers;

        // Shift+Tab (backtab) must be handled before the Ctrl branch.
        if key == "tab" && modifiers.shift {
            return Some(b"\x1b[Z".to_vec());
        }

        // Ctrl+letter: send the corresponding control character (ASCII 0x01-0x1a).
        if modifiers.control && !modifiers.shift && !modifiers.alt {
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
                "b" => Some(vec![0x02]),
                "f" => Some(vec![0x06]),
                "g" => Some(vec![0x07]),
                "h" => Some(vec![0x08]),
                "i" => Some(vec![0x09]),
                "j" => Some(vec![0x0a]),
                "o" => Some(vec![0x0f]),
                "q" => Some(vec![0x11]),
                "s" => Some(vec![0x13]),
                "t" => Some(vec![0x14]),
                "v" => Some(vec![0x16]),
                "x" => Some(vec![0x18]),
                "y" => Some(vec![0x19]),
                _ => None,
            };
        }

        // Compute xterm modifier parameter for special keys.
        // 1=none, 2=Shift, 3=Alt, 5=Ctrl, and combinations thereof.
        let modifier_param = {
            let mut m = 1u8;
            if modifiers.shift {
                m += 1;
            }
            if modifiers.alt {
                m += 2;
            }
            if modifiers.control {
                m += 4;
            }
            m
        };
        let has_modifiers = modifier_param > 1;

        // Special keys with CSI sequences that support modifier parameters.
        // Format: \x1b[1;{mod}{letter} for arrow/home/end, \x1b[{num};{mod}~ for others.
        match key {
            "enter" => Some(vec![b'\r']),
            "tab" => Some(vec![b'\t']),
            "backspace" => {
                if modifiers.alt {
                    Some(b"\x1b\x7f".to_vec()) // Alt+Backspace: ESC + DEL
                } else {
                    Some(vec![0x7f])
                }
            }
            "escape" => Some(vec![0x1b]),
            "space" => {
                if modifiers.control {
                    Some(vec![0x00]) // Ctrl+Space: NUL
                } else {
                    Some(vec![b' '])
                }
            }
            "up" if has_modifiers => Some(format!("\x1b[1;{modifier_param}A").into_bytes()),
            "up" => Some(b"\x1b[A".to_vec()),
            "down" if has_modifiers => Some(format!("\x1b[1;{modifier_param}B").into_bytes()),
            "down" => Some(b"\x1b[B".to_vec()),
            "right" if has_modifiers => Some(format!("\x1b[1;{modifier_param}C").into_bytes()),
            "right" => Some(b"\x1b[C".to_vec()),
            "left" if has_modifiers => Some(format!("\x1b[1;{modifier_param}D").into_bytes()),
            "left" => Some(b"\x1b[D".to_vec()),
            "home" if has_modifiers => Some(format!("\x1b[1;{modifier_param}H").into_bytes()),
            "home" => Some(b"\x1b[H".to_vec()),
            "end" if has_modifiers => Some(format!("\x1b[1;{modifier_param}F").into_bytes()),
            "end" => Some(b"\x1b[F".to_vec()),
            "insert" if has_modifiers => Some(format!("\x1b[2;{modifier_param}~").into_bytes()),
            "insert" => Some(b"\x1b[2~".to_vec()),
            "delete" if has_modifiers => Some(format!("\x1b[3;{modifier_param}~").into_bytes()),
            "delete" => Some(b"\x1b[3~".to_vec()),
            "pageup" if has_modifiers => Some(format!("\x1b[5;{modifier_param}~").into_bytes()),
            "pageup" => Some(b"\x1b[5~".to_vec()),
            "pagedown" if has_modifiers => Some(format!("\x1b[6;{modifier_param}~").into_bytes()),
            "pagedown" => Some(b"\x1b[6~".to_vec()),
            "f1" if has_modifiers => Some(format!("\x1b[1;{modifier_param}P").into_bytes()),
            "f1" => Some(b"\x1bOP".to_vec()),
            "f2" if has_modifiers => Some(format!("\x1b[1;{modifier_param}Q").into_bytes()),
            "f2" => Some(b"\x1bOQ".to_vec()),
            "f3" if has_modifiers => Some(format!("\x1b[1;{modifier_param}R").into_bytes()),
            "f3" => Some(b"\x1bOR".to_vec()),
            "f4" if has_modifiers => Some(format!("\x1b[1;{modifier_param}S").into_bytes()),
            "f4" => Some(b"\x1bOS".to_vec()),
            "f5" if has_modifiers => Some(format!("\x1b[15;{modifier_param}~").into_bytes()),
            "f5" => Some(b"\x1b[15~".to_vec()),
            "f6" if has_modifiers => Some(format!("\x1b[17;{modifier_param}~").into_bytes()),
            "f6" => Some(b"\x1b[17~".to_vec()),
            "f7" if has_modifiers => Some(format!("\x1b[18;{modifier_param}~").into_bytes()),
            "f7" => Some(b"\x1b[18~".to_vec()),
            "f8" if has_modifiers => Some(format!("\x1b[19;{modifier_param}~").into_bytes()),
            "f8" => Some(b"\x1b[19~".to_vec()),
            "f9" if has_modifiers => Some(format!("\x1b[20;{modifier_param}~").into_bytes()),
            "f9" => Some(b"\x1b[20~".to_vec()),
            "f10" if has_modifiers => Some(format!("\x1b[21;{modifier_param}~").into_bytes()),
            "f10" => Some(b"\x1b[21~".to_vec()),
            "f11" if has_modifiers => Some(format!("\x1b[23;{modifier_param}~").into_bytes()),
            "f11" => Some(b"\x1b[23~".to_vec()),
            "f12" if has_modifiers => Some(format!("\x1b[24;{modifier_param}~").into_bytes()),
            "f12" => Some(b"\x1b[24~".to_vec()),
            _ => {
                // Alt+key: prefix with ESC
                if modifiers.alt {
                    if let Some(ch) = &keystroke.key_char {
                        let mut bytes = vec![0x1b];
                        bytes.extend_from_slice(ch.as_bytes());
                        return Some(bytes);
                    } else if key.len() == 1 {
                        let mut bytes = vec![0x1b];
                        bytes.extend_from_slice(key.as_bytes());
                        return Some(bytes);
                    }
                }

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

pub enum TerminalPanelEvent {
    OpenCommandPalette { tab: PaletteTab },
}

impl EventEmitter<TerminalPanelEvent> for TerminalPanel {}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalPanel {
    /// Open the search overlay.
    pub fn open_search(&mut self, cx: &mut Context<Self>) {
        if self.search_open {
            return;
        }
        self.search_open = true;
        let overlay = cx.new(super::search_overlay::SearchOverlay::new);
        cx.subscribe(&overlay, Self::on_search_event).detach();
        self.search_overlay = Some(overlay);
        cx.notify();
    }

    /// Close the search overlay and clear search state.
    fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_open = false;
        self.search_overlay = None;
        self.search_matches.clear();
        self.search_current_idx = None;
        // Focus will be restored to terminal on next render via auto-focus.
        cx.notify();
    }

    /// Handle events from the search overlay.
    fn on_search_event(
        &mut self,
        _emitter: Entity<super::search_overlay::SearchOverlay>,
        event: &super::search_overlay::SearchOverlayEvent,
        cx: &mut Context<Self>,
    ) {
        use super::search_overlay::SearchOverlayEvent;
        match event {
            SearchOverlayEvent::QueryChanged(query) => {
                self.update_search_matches(query, cx);
            }
            SearchOverlayEvent::NextMatch => {
                self.navigate_search_match(true, cx);
            }
            SearchOverlayEvent::PrevMatch => {
                self.navigate_search_match(false, cx);
            }
            SearchOverlayEvent::Close => {
                self.close_search(cx);
            }
        }
    }

    /// Recompute search matches for the given query using alacritty's RegexIter.
    fn update_search_matches(&mut self, query: &str, cx: &mut Context<Self>) {
        use alacritty_terminal::grid::Dimensions;
        use alacritty_terminal::index::{Column, Direction, Line, Point};
        use alacritty_terminal::term::search::{RegexIter, RegexSearch};

        self.search_matches.clear();
        self.search_current_idx = None;

        if query.is_empty() {
            if let Some(overlay) = &self.search_overlay {
                overlay.update(cx, |o, cx| o.set_match_info(0, 0, cx));
            }
            cx.notify();
            return;
        }

        // Escape special regex characters for literal search.
        let escaped = escape_regex(query);
        let Ok(mut regex) = RegexSearch::new(&escaped) else {
            if let Some(overlay) = &self.search_overlay {
                overlay.update(cx, |o, cx| o.set_match_info(0, 0, cx));
            }
            cx.notify();
            return;
        };

        if let Ok(term) = self.term.lock() {
            let rows = term.screen_lines();
            let cols = term.columns();
            if rows > 0 && cols > 0 {
                let topmost = term.topmost_line();
                let start = Point::new(topmost, Column(0));
                let end = Point::new(Line(rows as i32 - 1), Column(cols - 1));
                let iter = RegexIter::new(start, end, Direction::Right, &term, &mut regex);
                for m in iter {
                    self.search_matches.push(m);
                    if self.search_matches.len() >= 10_000 {
                        break;
                    }
                }
            }
        }

        // Select the match nearest to viewport center.
        if !self.search_matches.is_empty()
            && let Ok(term) = self.term.lock()
        {
            let display_offset = term.grid().display_offset() as i32;
            let rows = term.screen_lines() as i32;
            let center_line = Line(-display_offset + rows / 2);
            let mut best = 0usize;
            let mut best_dist = i32::MAX;
            for (i, m) in self.search_matches.iter().enumerate() {
                let d = (m.start().line.0 - center_line.0).abs();
                if d < best_dist {
                    best_dist = d;
                    best = i;
                }
            }
            self.search_current_idx = Some(best);
        }

        let total = self.search_matches.len();
        let current = self.search_current_idx.map_or(0, |i| i + 1);
        if let Some(overlay) = &self.search_overlay {
            overlay.update(cx, |o, cx| o.set_match_info(current, total, cx));
        }

        self.scroll_to_current_match();
        cx.notify();
    }

    /// Navigate to the next or previous search match.
    fn navigate_search_match(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.search_matches.is_empty() {
            return;
        }
        let total = self.search_matches.len();
        let idx = match self.search_current_idx {
            Some(i) => {
                if forward {
                    (i + 1) % total
                } else {
                    (i + total - 1) % total
                }
            }
            None => 0,
        };
        self.search_current_idx = Some(idx);

        if let Some(overlay) = &self.search_overlay {
            overlay.update(cx, |o, cx| o.set_match_info(idx + 1, total, cx));
        }

        self.scroll_to_current_match();
        cx.notify();
    }

    /// Scroll the terminal to show the current search match.
    fn scroll_to_current_match(&self) {
        use alacritty_terminal::grid::Scroll;
        let Some(idx) = self.search_current_idx else {
            return;
        };
        let Some(m) = self.search_matches.get(idx) else {
            return;
        };
        if let Ok(mut term) = self.term.lock() {
            let display_offset = term.grid().display_offset() as i32;
            let rows = term.screen_lines() as i32;
            let match_line = m.start().line.0;
            let viewport_top = -display_offset;
            let viewport_bottom = viewport_top + rows - 1;
            if match_line < viewport_top || match_line > viewport_bottom {
                // Scroll so match is in the middle of the viewport.
                let target_offset = -(match_line - rows / 2);
                let delta = target_offset - display_offset;
                if delta != 0 {
                    term.scroll_display(Scroll::Delta(delta));
                }
            }
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_output_reader(cx);
        self.start_cursor_blink(cx);

        // Auto-focus on first render (unless search overlay is open).
        if !self.focus_handle.is_focused(window) && !self.closed && !self.search_open {
            self.focus_handle.focus(window);
        }

        // Compute hovered URL match for the element.
        let hovered_url_match: Option<Match> = self.hovered_url_idx.get().and_then(|idx| {
            let detector = self.url_detector.borrow();
            detector.cached_match(idx)
        });

        // Clone search state for the element.
        let search_matches = if self.search_open {
            self.search_matches.clone()
        } else {
            Vec::new()
        };
        let search_current_idx = if self.search_open {
            self.search_current_idx
        } else {
            None
        };

        let terminal_el = TerminalElement::new(
            self.term.clone(),
            self.resize_debounce_tx.clone(),
            self.cursor_visible,
            self.layout_info.clone(),
            self.pending_scroll_delta.clone(),
            self.content_generation.clone(),
            self.cell_run_cache.clone(),
            self.glyph_cache.clone(),
            hovered_url_match,
            search_matches,
            search_current_idx,
        );

        let has_hovered_url = self.hovered_url_idx.get().is_some();

        let mut content = div()
            .id("terminal-content")
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::terminal_bg())
            .p(px(4.0))
            .overflow_hidden()
            .on_key_down({
                let input_tx = self.handle.input_tx().clone();
                let term = self.term.clone();
                let search_open = self.search_open;
                let entity = cx.entity().downgrade();
                let entity_id = cx.entity_id();
                let double_shift_kd = self.double_shift.clone();
                move |event: &KeyDownEvent, _window: &mut Window, cx: &mut App| {
                    let key = event.keystroke.key.as_str();
                    let mods = &event.keystroke.modifiers;

                    // Track key presses during shift hold for double-shift detection.
                    // GPUI on_key_down only fires for non-modifier keys, so every
                    // event here means a real key was pressed (not bare shift).
                    double_shift_kd.on_key_down_during_shift();

                    // Ctrl+F: open search
                    if mods.control && !mods.shift && key == "f" {
                        let _ = entity.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                            this.open_search(cx);
                        });
                        return;
                    }

                    // Ctrl+K: open command palette (All tab)
                    if mods.control && !mods.shift && !mods.alt && key == "k" {
                        let _ = entity.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                            if this.search_open {
                                this.close_search(cx);
                            }
                            cx.emit(TerminalPanelEvent::OpenCommandPalette {
                                tab: PaletteTab::All,
                            });
                        });
                        return;
                    }

                    // Ctrl+Shift+E: open command palette (Sessions tab)
                    if mods.control && mods.shift && !mods.alt && key == "e" {
                        let _ = entity.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                            if this.search_open {
                                this.close_search(cx);
                            }
                            cx.emit(TerminalPanelEvent::OpenCommandPalette {
                                tab: PaletteTab::Sessions,
                            });
                        });
                        return;
                    }

                    // Ctrl+Shift+P: open command palette (Projects tab)
                    if mods.control && mods.shift && !mods.alt && key == "p" {
                        let _ = entity.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                            if this.search_open {
                                this.close_search(cx);
                            }
                            cx.emit(TerminalPanelEvent::OpenCommandPalette {
                                tab: PaletteTab::Projects,
                            });
                        });
                        return;
                    }

                    // Ctrl+Shift+A: open command palette (Actions tab)
                    if mods.control && mods.shift && !mods.alt && key == "a" {
                        let _ = entity.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                            if this.search_open {
                                this.close_search(cx);
                            }
                            cx.emit(TerminalPanelEvent::OpenCommandPalette {
                                tab: PaletteTab::Actions,
                            });
                        });
                        return;
                    }

                    // Don't send keys to PTY while search is open
                    if search_open {
                        return;
                    }

                    // Ctrl+C: copy selection if any, else send SIGINT
                    // Ctrl+Shift+C: also copy selection
                    if mods.control
                        && !mods.alt
                        && (key == "c" || key.eq_ignore_ascii_case("c") && mods.shift)
                    {
                        if let Ok(mut t) = term.lock()
                            && let Some(text) = t.selection_to_string()
                        {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                            t.selection = None;
                            cx.notify(entity_id);
                            return;
                        }
                        // No selection: Ctrl+C sends SIGINT, Ctrl+Shift+C does nothing
                        if !mods.shift {
                            let _ = input_tx.send(vec![0x03]);
                        }
                        return;
                    }

                    // Ctrl+V / Ctrl+Shift+V: paste from clipboard
                    if mods.control
                        && !mods.alt
                        && (key == "v" || key.eq_ignore_ascii_case("v") && mods.shift)
                    {
                        if let Some(item) = cx.read_from_clipboard()
                            && let Some(text) = item.text()
                            && !text.is_empty()
                        {
                            let bracketed = term
                                .lock()
                                .ok()
                                .is_some_and(|t| t.mode().contains(TermMode::BRACKETED_PASTE));
                            let mut bytes = Vec::with_capacity(text.len() + 12);
                            if bracketed {
                                bytes.extend_from_slice(b"\x1b[200~");
                            }
                            bytes.extend_from_slice(text.as_bytes());
                            if bracketed {
                                bytes.extend_from_slice(b"\x1b[201~");
                            }
                            let _ = input_tx.send(bytes);
                        }
                        return;
                    }

                    if let Some(bytes) = TerminalPanel::encode_keystroke(&event.keystroke) {
                        let _ = input_tx.send(bytes);
                    }
                }
            })
            // Modifier key tracking: double-shift detection + URL hover on ctrl release.
            // GPUI does NOT fire on_key_down/on_key_up for bare modifier keys (X11 and
            // Wayland both filter them with keysym.is_modifier_key()). The only event
            // for modifier state changes is ModifiersChangedEvent.
            .on_modifiers_changed({
                let hovered_url_idx = self.hovered_url_idx.clone();
                let entity_id = cx.entity_id();
                let entity_mc = cx.entity().downgrade();
                let double_shift_mc = self.double_shift.clone();
                move |event: &ModifiersChangedEvent, _window: &mut Window, cx: &mut App| {
                    let mods = &event.modifiers;

                    // Clear URL hover when control is released
                    if !mods.control && hovered_url_idx.get().is_some() {
                        hovered_url_idx.set(None);
                        cx.notify(entity_id);
                    }

                    // Double-shift detection via modifier transitions
                    if double_shift_mc.on_modifiers_changed(
                        mods.shift,
                        mods.control,
                        mods.alt,
                        mods.platform,
                    ) {
                        let _ = entity_mc.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                            if this.search_open {
                                this.close_search(cx);
                            }
                            cx.emit(TerminalPanelEvent::OpenCommandPalette {
                                tab: PaletteTab::All,
                            });
                        });
                    }
                }
            })
            // Focus on any mouse button press
            .on_any_mouse_down({
                let focus = self.focus_handle.clone();
                move |_event: &MouseDownEvent, window: &mut Window, _cx: &mut App| {
                    focus.focus(window);
                }
            })
            // Left mouse button: start text selection or open URL
            .on_mouse_down(MouseButton::Left, {
                let term = self.term.clone();
                let layout_info = self.layout_info.clone();
                let entity_id = cx.entity_id();
                let url_detector = self.url_detector.clone();
                let hovered_url_idx = self.hovered_url_idx.clone();
                move |event: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
                    // Ctrl+click: open hovered URL
                    if event.modifiers.control
                        && let Some(idx) = hovered_url_idx.get()
                    {
                        if let Ok(t) = term.lock() {
                            let detector = url_detector.borrow();
                            let url = detector.url_text(&t, idx);
                            drop(t);
                            drop(detector);
                            if !url.is_empty() {
                                open::that_in_background(&url);
                            }
                        }
                        return;
                    }

                    let info = layout_info.get();
                    if info.cell_width == px(0.0) {
                        return;
                    }

                    if let Ok(mut t) = term.lock() {
                        let display_offset = t.grid().display_offset();
                        let cols = t.columns();
                        let rows = t.screen_lines();

                        let (grid_point, side) = TerminalPanel::pixel_to_grid(
                            event.position,
                            info.bounds.origin,
                            info.cell_width,
                            info.cell_height,
                            cols,
                            rows,
                            display_offset,
                        );

                        // Double-click: word (semantic) selection
                        // Triple-click: line selection
                        let selection_type = match event.click_count {
                            2 => SelectionType::Semantic,
                            3 => SelectionType::Lines,
                            _ => SelectionType::Simple,
                        };

                        let selection = Selection::new(selection_type, grid_point, side);
                        t.selection = Some(selection);
                    }

                    cx.notify(entity_id);
                }
            })
            // Mouse move: selection drag + URL hover detection
            .on_mouse_move({
                let term = self.term.clone();
                let layout_info = self.layout_info.clone();
                let entity_id = cx.entity_id();
                let url_detector = self.url_detector.clone();
                let hovered_url_idx = self.hovered_url_idx.clone();
                let content_generation = self.content_generation.clone();
                move |event: &MouseMoveEvent, _window: &mut Window, cx: &mut App| {
                    // Selection drag takes priority
                    if event.pressed_button == Some(MouseButton::Left) {
                        let info = layout_info.get();
                        if info.cell_width == px(0.0) {
                            return;
                        }

                        if let Ok(mut t) = term.lock() {
                            if t.selection.is_none() {
                                return;
                            }

                            let display_offset = t.grid().display_offset();
                            let cols = t.columns();
                            let rows = t.screen_lines();

                            let (grid_point, side) = TerminalPanel::pixel_to_grid(
                                event.position,
                                info.bounds.origin,
                                info.cell_width,
                                info.cell_height,
                                cols,
                                rows,
                                display_offset,
                            );

                            if let Some(ref mut selection) = t.selection {
                                selection.update(grid_point, side);
                            }
                        }

                        cx.notify(entity_id);
                        return;
                    }

                    // URL hover detection (Ctrl+hover)
                    if event.modifiers.control {
                        let info = layout_info.get();
                        if info.cell_width == px(0.0) {
                            return;
                        }

                        if let Ok(t) = term.lock() {
                            let display_offset = t.grid().display_offset();
                            let content_gen = content_generation.load(Ordering::Relaxed);
                            let cols = t.columns();
                            let rows = t.screen_lines();

                            let (grid_point, _) = TerminalPanel::pixel_to_grid(
                                event.position,
                                info.bounds.origin,
                                info.cell_width,
                                info.cell_height,
                                cols,
                                rows,
                                display_offset,
                            );

                            let mut detector = url_detector.borrow_mut();
                            detector.detect(&t, display_offset, content_gen);
                            let new_idx = detector.match_at_point(grid_point).map(|(i, _)| i);
                            if hovered_url_idx.get() != new_idx {
                                hovered_url_idx.set(new_idx);
                                cx.notify(entity_id);
                            }
                        }
                    } else if hovered_url_idx.get().is_some() {
                        hovered_url_idx.set(None);
                        cx.notify(entity_id);
                    }
                }
            })
            // Left mouse up: finalize selection, auto-copy to clipboard
            .on_mouse_up(MouseButton::Left, {
                let entity_id = cx.entity_id();
                let term = self.term.clone();
                move |_event: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
                    if let Ok(mut t) = term.lock() {
                        if t.selection.as_ref().is_some_and(|s| s.is_empty()) {
                            // Clear empty selections (single click without drag)
                            t.selection = None;
                        } else if let Some(text) = t.selection_to_string() {
                            // Auto-copy non-empty selection to clipboard
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                    }
                    cx.notify(entity_id);
                }
            })
            .on_scroll_wheel({
                let entity_id = cx.entity_id();
                let layout_info = self.layout_info.clone();
                let scroll_px = self.scroll_px.clone();
                let pending_scroll_delta = self.pending_scroll_delta.clone();
                move |event: &ScrollWheelEvent, _window: &mut Window, cx: &mut App| {
                    let info = layout_info.get();
                    let cell_h = if info.cell_height > px(0.0) {
                        info.cell_height
                    } else {
                        px(18.0)
                    };

                    // Scroll multiplier: for discrete mouse wheels (Lines delta), each
                    // tick reports 1 line, so multiply to get SCROLL_LINES_PER_TICK.
                    // For trackpad (Pixels delta), the OS already provides proportional
                    // pixel values, so use 1.0 to avoid double-scaling.
                    let multiplier = match event.delta {
                        ScrollDelta::Lines(_) => SCROLL_LINES_PER_TICK as f32,
                        ScrollDelta::Pixels(_) => 1.0,
                    };

                    // Following Zed's approach: accumulate pixel deltas and convert to
                    // whole-line scroll deltas. No sub-pixel rendering offset -- alacritty's
                    // display_offset always moves by whole lines.
                    let line_delta = match event.touch_phase {
                        TouchPhase::Started => {
                            // Reset accumulator at gesture start.
                            scroll_px.set(0.0);
                            None
                        }
                        TouchPhase::Moved => {
                            let pixel_delta = event.delta.pixel_delta(cell_h);
                            let old_offset = (px(scroll_px.get()) / cell_h) as i32;
                            let new_scroll_px =
                                scroll_px.get() + f32::from(pixel_delta.y) * multiplier;
                            let new_offset = (px(new_scroll_px) / cell_h) as i32;
                            // Keep accumulator bounded to viewport height to avoid overflow.
                            let viewport_h =
                                f32::from(info.bounds.size.height).max(f32::from(cell_h));
                            scroll_px.set(new_scroll_px % viewport_h);
                            let delta = new_offset - old_offset;
                            if delta != 0 { Some(delta) } else { None }
                        }
                        TouchPhase::Ended => None,
                    };

                    if let Some(lines) = line_delta {
                        // Negate: positive pixel_delta.y = scroll content down (show newer),
                        // but Scroll::Delta(positive) = scroll up (show older/history).
                        pending_scroll_delta.fetch_add(-lines, Ordering::Relaxed);
                        cx.notify(entity_id);
                    }
                }
            })
            .focus(|style: StyleRefinement| style.border_color(theme::accent()).border_1())
            .child(terminal_el);

        if has_hovered_url {
            content = content.cursor(CursorStyle::PointingHand);
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

        // Connection type indicator (bottom-right pill badge).
        let is_direct = self.handle.is_direct();
        let tooltip_text: &'static str = if is_direct {
            "Direct tmux connection (FIFO bypass, lower latency)"
        } else {
            "WebSocket relay through server/agent"
        };
        content = content.child(
            div()
                .id("connection-indicator")
                .absolute()
                .bottom(px(8.0))
                .right(px(8.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(8.0))
                .bg(gpui::rgba(0x1111_1399))
                .child(
                    icon(if is_direct { Icon::Zap } else { Icon::Wifi })
                        .size(px(10.0))
                        .text_color(if is_direct {
                            theme::success()
                        } else {
                            theme::text_tertiary()
                        }),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme::text_tertiary())
                        .child(if is_direct { "Direct" } else { "WS" }),
                )
                .tooltip(move |_window, cx| cx.new(|_| ConnectionTooltip(tooltip_text)).into()),
        );

        // Wrap in a vertical container with optional search overlay on top.
        let mut wrapper = div().flex().flex_col().size_full();
        if let Some(overlay) = &self.search_overlay {
            wrapper = wrapper.child(overlay.clone());
        }
        wrapper = wrapper.child(content);
        wrapper
    }
}

/// Minimal view for tooltip rendering (GPUI tooltips require `AnyView`).
struct ConnectionTooltip(&'static str);

impl Render for ConnectionTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(theme::bg_tertiary())
            .border_1()
            .border_color(theme::border())
            .text_size(px(11.0))
            .text_color(theme::text_secondary())
            .child(self.0)
    }
}

/// Escape special regex characters for literal string matching.
fn escape_regex(input: &str) -> String {
    let mut result = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }
    result
}
