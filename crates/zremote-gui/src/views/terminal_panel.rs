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
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::Processor;
use gpui::*;

use crate::terminal_ws::{self, TerminalWsHandle};
use crate::theme;
use crate::types::TerminalEvent;
use crate::views::terminal_element::{CellRunCache, ShapedLineCache, TerminalElement};

/// Cursor blink interval (standard terminal blink rate).
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// Default terminal dimensions (used until first resize fits to container).
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

/// Lines to scroll per mouse wheel tick.
const SCROLL_LINES_PER_TICK: i32 = 3;

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
    ws_handle: TerminalWsHandle,
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
    /// Cached cell runs from previous frame (persists across renders, shared with TerminalElement).
    cell_run_cache: Rc<RefCell<Option<CellRunCache>>>,
    /// Cached shaped text lines (persists across renders, shared with TerminalElement).
    shaped_cache: Rc<RefCell<ShapedLineCache>>,
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
            layout_info: Rc::new(Cell::new(TerminalLayoutInfo::default())),
            scroll_px: Rc::new(Cell::new(0.0)),
            pending_scroll_delta: Arc::new(AtomicI32::new(0)),
            content_generation: Arc::new(AtomicU64::new(0)),
            cell_run_cache: Rc::new(RefCell::new(None)),
            shaped_cache: Rc::new(RefCell::new(ShapedLineCache::new())),
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
            self.layout_info.clone(),
            self.pending_scroll_delta.clone(),
            self.content_generation.clone(),
            self.cell_run_cache.clone(),
            self.shaped_cache.clone(),
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
                let term = self.term.clone();
                move |event: &KeyDownEvent, _window: &mut Window, cx: &mut App| {
                    // Ctrl+Shift+C: copy selection to clipboard
                    let key = event.keystroke.key.as_str();
                    let mods = &event.keystroke.modifiers;
                    if mods.control && mods.shift && key.eq_ignore_ascii_case("c") {
                        if let Ok(t) = term.lock() {
                            if let Some(text) = t.selection_to_string() {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            }
                        }
                        return;
                    }

                    if let Some(bytes) = TerminalPanel::encode_keystroke(&event.keystroke) {
                        let _ = input_tx.send(bytes);
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
            // Left mouse button: start text selection
            .on_mouse_down(MouseButton::Left, {
                let term = self.term.clone();
                let layout_info = self.layout_info.clone();
                let entity_id = cx.entity_id();
                move |event: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
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
            // Mouse move while dragging: update selection end point
            .on_mouse_move({
                let term = self.term.clone();
                let layout_info = self.layout_info.clone();
                let entity_id = cx.entity_id();
                move |event: &MouseMoveEvent, _window: &mut Window, cx: &mut App| {
                    // Only update selection while left button is held
                    if event.pressed_button != Some(MouseButton::Left) {
                        return;
                    }

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
                }
            })
            // Left mouse up: finalize selection (selection stays for copy)
            .on_mouse_up(MouseButton::Left, {
                let entity_id = cx.entity_id();
                let term = self.term.clone();
                move |_event: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
                    // Clear empty selections (single click without drag)
                    if let Ok(mut t) = term.lock() {
                        let should_clear = t
                            .selection
                            .as_ref()
                            .map_or(false, |s| s.is_empty());
                        if should_clear {
                            t.selection = None;
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
