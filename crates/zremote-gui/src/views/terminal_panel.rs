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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use crate::terminal_handle::{InputSender, TerminalHandle};
use crate::theme;
use crate::views::command_palette::PaletteTab;
use crate::views::double_shift::DoubleShiftDetector;
use crate::views::key_bindings::{KeyAction, dispatch_global_key};
use crate::views::terminal_element::{CellRunCache, GlyphCache, TerminalElement};
use crate::views::url_detector::UrlDetector;
use zremote_client::{AgenticStatus, TerminalEvent};

use crate::views::cc_widgets;
use crate::views::sidebar::CcMetrics;

/// Cursor blink interval (standard terminal blink rate).
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// Default terminal dimensions (used until first resize fits to container).
const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

/// Debounce delay for WebSocket resize messages (ms).
/// Local term.resize() is immediate; only the server message is debounced.
const RESIZE_DEBOUNCE_MS: u64 = 150;

/// Read image data from the system clipboard (bypassing GPUI's text-only API)
/// and return it as a base64-encoded PNG string.
fn read_clipboard_image_base64() -> Option<String> {
    use base64::Engine;

    let mut clipboard = arboard::Clipboard::new().ok()?;
    let img = clipboard.get_image().ok()?;
    if img.width == 0 || img.height == 0 {
        return None;
    }

    let mut png_buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_buf, img.width as u32, img.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&img.bytes).ok()?;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(&png_buf))
}

/// Shared layout info set by the element during paint, read by mouse event handlers.
#[derive(Clone, Copy, Default)]
pub struct TerminalLayoutInfo {
    pub cell_width: Pixels,
    pub cell_height: Pixels,
    pub bounds: Bounds<Pixels>,
}

/// A pending terminal title change from OSC escape sequences.
enum TitleChange {
    /// Program set a new title via OSC 0/2.
    Set(String),
    /// Program reset the title via OSC reset.
    Reset,
}

/// Captures `Event::PtyWrite` and `Event::Title` from alacritty_terminal.
/// `PtyWrite` sends DSR responses back through the terminal input channel.
/// `Title` captures OSC title changes for display in the sidebar.
#[derive(Clone)]
pub(crate) struct PtyWriteListener {
    /// Swappable sender so reconnect can update without recreating the Term.
    inner: Arc<std::sync::Mutex<InputSender>>,
    /// Pending title change from OSC escape sequences, consumed by the reader task.
    pending_title: Arc<std::sync::Mutex<Option<TitleChange>>>,
    /// When true, PtyWrite events (e.g. DSR responses) are silently dropped.
    /// Set during scrollback replay to prevent alacritty from sending stale
    /// cursor position reports back to the remote shell.
    replaying: Arc<AtomicBool>,
}

impl PtyWriteListener {
    fn new(input_tx: InputSender) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(input_tx)),
            pending_title: Arc::new(std::sync::Mutex::new(None)),
            replaying: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Suppress or re-enable PtyWrite forwarding (used during scrollback replay).
    fn set_replaying(&self, value: bool) {
        // Relaxed: single-bit signal flag, no dependent data to synchronize.
        self.replaying.store(value, Ordering::Relaxed);
    }

    /// Replace the input sender (called on reconnect).
    fn update_sender(&self, input_tx: InputSender) {
        match self.inner.lock() {
            Ok(mut guard) => *guard = input_tx,
            Err(poisoned) => {
                tracing::warn!("PtyWriteListener mutex poisoned during update_sender, recovering");
                *poisoned.into_inner() = input_tx;
            }
        }
    }

    /// Take the pending title change (if any) set by OSC sequences since last check.
    fn take_pending_title(&self) -> Option<TitleChange> {
        self.pending_title
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }
}

impl alacritty_terminal::event::EventListener for PtyWriteListener {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        match event {
            alacritty_terminal::event::Event::PtyWrite(text) => {
                if self.replaying.load(Ordering::Relaxed) {
                    return;
                }
                match self.inner.lock() {
                    Ok(guard) => guard.try_send(text.into_bytes()),
                    Err(poisoned) => poisoned.into_inner().try_send(text.into_bytes()),
                }
            }
            alacritty_terminal::event::Event::Title(title) => {
                *self.pending_title.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(TitleChange::Set(title));
            }
            alacritty_terminal::event::Event::ResetTitle => {
                *self.pending_title.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(TitleChange::Reset);
            }
            _ => {}
        }
    }
}

/// Alacritty terminal type parameterized with our `PtyWriteListener`.
pub(crate) type TerminalTerm = alacritty_terminal::Term<PtyWriteListener>;

pub struct TerminalPanel {
    session_id: String,
    term: Arc<Mutex<TerminalTerm>>,
    pty_write_listener: PtyWriteListener,
    handle: TerminalHandle,
    focus_handle: FocusHandle,
    closed: bool,
    /// Error message from server (shown in UI when set).
    error_message: Option<String>,
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
    /// Connection mode: "local" or "server".
    mode: String,
    /// Whether the terminal WebSocket connection has been lost
    /// (session may still be alive on server, waiting for reconnect).
    disconnected: bool,
    /// Terminal title set by OSC escape sequences (e.g. `\e]0;title\a`).
    terminal_title: Option<String>,
    /// Claude Code session metrics for the current session.
    cc_metrics: Option<CcMetrics>,
    /// Claude Code agentic status for the current session.
    cc_status: Option<AgenticStatus>,
    /// Tokio runtime handle for spawning async tasks (coalescing, etc.).
    tokio_handle: tokio::runtime::Handle,
    /// Owned resize debounce task — cancelled on drop or reconnect.
    resize_debounce_task: Option<Task<()>>,
    /// Owned PTY reader task — cancelled on drop or reconnect.
    pty_reader_task: Option<Task<()>>,
}

impl TerminalPanel {
    pub fn new(
        session_id: String,
        handle: TerminalHandle,
        tokio_handle: &tokio::runtime::Handle,
        mode: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = TermConfig::default();
        let size = TermSize::new(usize::from(DEFAULT_COLS), usize::from(DEFAULT_ROWS));
        let pty_write_listener = PtyWriteListener::new(handle.input_sender());
        let term = alacritty_terminal::Term::new(config, &size, pty_write_listener.clone());
        let term = Arc::new(Mutex::new(term));

        let focus_handle = cx.focus_handle();

        // Resize debouncing: element sends to debounce_tx (immediate local resize),
        // tokio task forwards to real resize_tx after 150ms of inactivity.
        let (resize_debounce_tx, resize_debounce_rx) = flume::bounded::<(u16, u16)>(4);
        let real_resize_tx = handle.resize_tx().clone();
        let resize_debounce_task = cx.background_spawn(async move {
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
            pty_write_listener,
            handle,
            focus_handle,
            closed: false,
            error_message: None,
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
            mode,
            disconnected: false,
            terminal_title: None,
            cc_metrics: None,
            cc_status: None,
            tokio_handle: tokio_handle.clone(),
            resize_debounce_task: Some(resize_debounce_task),
            pty_reader_task: None, // Set when start_output_reader() is called
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Extract the last N visible lines from the terminal for preview rendering.
    /// Returns `(lines, cols, rows)` with `PreviewLine`-compatible data (text + color spans).
    pub fn extract_preview_lines(
        &self,
        max_lines: usize,
    ) -> (Vec<zremote_client::PreviewLine>, u16, u16) {
        let Ok(term) = self.term.lock() else {
            return (Vec::new(), 0, 0);
        };

        let cols = term.columns();
        let rows = term.screen_lines();
        let start_row = rows.saturating_sub(max_lines);
        let mut lines = Vec::with_capacity(max_lines);

        for row_idx in start_row..rows {
            let line_idx = row_idx as i32;
            let mut text = String::with_capacity(cols);
            let mut spans: Vec<zremote_client::PreviewColorSpan> = Vec::new();
            let mut current_fg: Option<String> = None;
            let mut span_start: u16 = 0;

            for col in 0..cols {
                let point = alacritty_terminal::index::Point::new(
                    alacritty_terminal::index::Line(line_idx),
                    alacritty_terminal::index::Column(col),
                );
                let cell = &term.grid()[point];
                let ch = if cell.c == '\0' { ' ' } else { cell.c };
                text.push(ch);

                let fg_hex = ansi_color_to_hex(cell.fg);

                if fg_hex != current_fg {
                    if let Some(ref fg) = current_fg {
                        spans.push(zremote_client::PreviewColorSpan {
                            start: span_start,
                            end: col as u16,
                            fg: fg.clone(),
                        });
                    }
                    current_fg = fg_hex;
                    span_start = col as u16;
                }
            }
            if let Some(fg) = current_fg {
                spans.push(zremote_client::PreviewColorSpan {
                    start: span_start,
                    end: cols as u16,
                    fg,
                });
            }

            let trimmed = text.trim_end().to_string();
            lines.push(zremote_client::PreviewLine {
                text: trimmed,
                spans,
            });
        }

        while lines.last().is_some_and(|l| l.text.is_empty()) {
            lines.pop();
        }

        (lines, cols as u16, rows as u16)
    }

    /// Get a clonable input sender for writing to this terminal's PTY.
    pub fn input_sender(&self) -> InputSender {
        self.handle.input_sender()
    }

    /// Whether the terminal WebSocket connection has been lost.
    pub fn is_disconnected(&self) -> bool {
        self.disconnected
    }

    /// Update Claude Code metrics for this terminal's session.
    pub fn update_cc_metrics(&mut self, metrics: CcMetrics) {
        self.cc_metrics = Some(metrics);
    }

    /// Update Claude Code agentic status.
    pub fn update_cc_status(&mut self, status: Option<AgenticStatus>) {
        self.cc_status = status;
    }

    /// Clear Claude Code state (e.g., on session close).
    pub fn clear_cc_state(&mut self) {
        self.cc_metrics = None;
        self.cc_status = None;
    }

    /// Reconnect with a new terminal handle after session resume.
    /// Resets disconnect state, replaces the handle, restarts the reader,
    /// and sets up a new resize debounce pipeline.
    pub fn reconnect(
        &mut self,
        handle: TerminalHandle,
        tokio_handle: &tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) {
        self.disconnected = false;
        self.closed = false;
        self.error_message = None;
        self.terminal_title = None;
        self.reader_started = false; // Allow start_output_reader to run again
        self.pty_write_listener.update_sender(handle.input_sender());
        self.handle = handle;
        self.tokio_handle = tokio_handle.clone();

        // Set up new resize debounce pipeline for the new handle.
        // Replacing the task field cancels the previous debounce task.
        let (resize_debounce_tx, resize_debounce_rx) = flume::bounded::<(u16, u16)>(4);
        let real_resize_tx = self.handle.resize_tx().clone();
        self.resize_debounce_task = Some(cx.background_spawn(async move {
            let mut first_resize = true;
            loop {
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
        }));
        self.resize_debounce_tx = resize_debounce_tx;

        // Trigger immediate resize to sync the new connection with current terminal size.
        if let Ok(term) = self.term.lock() {
            let cols = term.columns();
            let rows = term.screen_lines();
            if cols > 0 && rows > 0 {
                let _ = self.resize_debounce_tx.send((cols as u16, rows as u16));
            }
        }

        cx.notify();
    }

    fn start_output_reader(&mut self, cx: &mut Context<Self>) {
        if self.reader_started {
            return;
        }
        self.reader_started = true;
        // Replacing the task field cancels any previous reader (e.g. after reconnect).
        self.pty_reader_task = None;

        let output_rx = self.handle.output_rx().clone();
        let term = self.term.clone();
        let content_generation = self.content_generation.clone();
        let pty_write_listener = self.pty_write_listener.clone();

        /// Coalescing window for GUI repaints (~60 Hz, matches vsync).
        const REPAINT_COALESCE_MS: u64 = 16;

        // Unified channel for GPUI thread: either a repaint signal or a control event.
        enum GuiSignal {
            Repaint,
            TitleChanged(Option<String>), // Some(title) = set, None = reset
            Event(TerminalEvent),
        }
        let (gui_tx, gui_rx) = flume::bounded::<GuiSignal>(32);

        // Tokio task: processes VTE eagerly, coalesces repaints at 16ms.
        let gui_tx_clone = gui_tx.clone();
        self.tokio_handle.spawn(async move {
            let gui_tx = gui_tx_clone;
            let mut processor: Processor = Processor::new();
            let mut needs_repaint = false;
            let mut coalesce_deadline: Option<tokio::time::Instant> = None;

            loop {
                let flush_sleep = async {
                    match coalesce_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                };

                tokio::select! {
                    event = output_rx.recv_async() => {
                        match event {
                            Ok(TerminalEvent::Output(bytes)) => {
                                let clean = strip_cpr_responses(&bytes);
                                if let Ok(mut t) = term.lock() {
                                    processor.advance(&mut *t, &clean);
                                }
                                if let Some(change) = pty_write_listener.take_pending_title() {
                                    let title = match change {
                                        TitleChange::Set(t) => Some(t),
                                        TitleChange::Reset => None,
                                    };
                                    let _ = gui_tx.try_send(GuiSignal::TitleChanged(title));
                                }
                                needs_repaint = true;
                                if coalesce_deadline.is_none() {
                                    coalesce_deadline = Some(
                                        tokio::time::Instant::now()
                                            + std::time::Duration::from_millis(REPAINT_COALESCE_MS),
                                    );
                                }
                            }
                            Ok(TerminalEvent::ScrollbackStart { cols: sb_cols, rows: sb_rows }) => {
                                pty_write_listener.set_replaying(true);
                                if let Ok(mut t) = term.lock() {
                                    let size = if sb_cols > 0 && sb_rows > 0 {
                                        TermSize::new(usize::from(sb_cols), usize::from(sb_rows))
                                    } else {
                                        let cols = t.columns();
                                        let rows = t.screen_lines();
                                        if cols > 0 && rows > 0 {
                                            TermSize::new(cols, rows)
                                        } else {
                                            TermSize::new(
                                                usize::from(DEFAULT_COLS),
                                                usize::from(DEFAULT_ROWS),
                                            )
                                        }
                                    };
                                    *t = alacritty_terminal::Term::new(
                                        TermConfig::default(),
                                        &size,
                                        pty_write_listener.clone(),
                                    );
                                }
                                content_generation.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(other) => {
                                if matches!(other, TerminalEvent::ScrollbackEnd { .. }) {
                                    pty_write_listener.set_replaying(false);
                                }
                                // Non-output events: flush pending repaint, then forward.
                                if needs_repaint {
                                    content_generation.fetch_add(1, Ordering::Relaxed);
                                    let _ = gui_tx.try_send(GuiSignal::Repaint);
                                    needs_repaint = false;
                                    coalesce_deadline = None;
                                }
                                let _ = gui_tx.send_async(GuiSignal::Event(other)).await;
                            }
                            Err(_) => break,
                        }
                    }
                    () = flush_sleep => {
                        if needs_repaint {
                            content_generation.fetch_add(1, Ordering::Relaxed);
                            let _ = gui_tx.try_send(GuiSignal::Repaint);
                            needs_repaint = false;
                        }
                        coalesce_deadline = None;
                    }
                }
            }

            // Final flush
            if needs_repaint {
                content_generation.fetch_add(1, Ordering::Relaxed);
                let _ = gui_tx.try_send(GuiSignal::Repaint);
            }
        });

        // GPUI task: reads unified signal channel and updates UI accordingly.
        self.pty_reader_task = Some(cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                loop {
                    match gui_rx.recv_async().await {
                        Ok(GuiSignal::Repaint) => {
                            let _ = this.update(cx, |_this: &mut Self, cx: &mut Context<Self>| {
                                cx.notify();
                            });
                        }
                        Ok(GuiSignal::TitleChanged(new_title)) => {
                            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                                if this.terminal_title != new_title {
                                    this.terminal_title = new_title.clone();
                                    cx.emit(TerminalPanelEvent::TitleChanged {
                                        session_id: this.session_id.clone(),
                                        title: new_title,
                                    });
                                }
                            });
                        }
                        Ok(GuiSignal::Event(TerminalEvent::SessionClosed { .. })) => {
                            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                                this.closed = true;
                                cx.notify();
                            });
                            break;
                        }
                        Ok(GuiSignal::Event(TerminalEvent::ScrollbackEnd { .. })) => {
                            let _ = this.update(cx, |_this: &mut Self, cx: &mut Context<Self>| {
                                cx.notify();
                            });
                        }
                        Ok(GuiSignal::Event(TerminalEvent::Error { message })) => {
                            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                                if this.handle.is_bridge() {
                                    let sid = this.session_id.clone();
                                    cx.emit(TerminalPanelEvent::BridgeFailed { session_id: sid });
                                } else {
                                    this.error_message = Some(message);
                                    this.closed = true;
                                    cx.notify();
                                }
                            });
                            // Always break: for bridge errors, reconnect() starts a fresh reader;
                            // for other errors, the terminal is closed.
                            break;
                        }
                        Ok(GuiSignal::Event(TerminalEvent::Disconnected)) => {
                            let _ = this.update(cx, |this: &mut Self, cx: &mut Context<Self>| {
                                this.disconnected = true;
                                cx.notify();
                            });
                            break;
                        }
                        Ok(GuiSignal::Event(_)) => {
                            // Pane and suspension events not yet handled
                        }
                        Err(_) => break,
                    }
                }
            },
        ));
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

#[allow(clippy::enum_variant_names)]
pub enum TerminalPanelEvent {
    OpenCommandPalette {
        tab: PaletteTab,
    },
    OpenSessionSwitcher,
    OpenHelp,
    BridgeFailed {
        session_id: String,
    },
    TitleChanged {
        session_id: String,
        title: Option<String>,
    },
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

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

impl TerminalPanel {
    fn render_terminal_element(&self) -> TerminalElement {
        let hovered_url_match: Option<Match> = self.hovered_url_idx.get().and_then(|idx| {
            let detector = self.url_detector.borrow();
            detector.cached_match(idx)
        });

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

        TerminalElement::new(
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
        )
    }

    fn render_status_overlay(&self) -> Option<AnyElement> {
        if self.closed {
            let label = if let Some(ref msg) = self.error_message {
                format!("[Error: {msg}]")
            } else {
                "[Session closed]".to_string()
            };
            let color = if self.error_message.is_some() {
                theme::error()
            } else {
                theme::text_tertiary()
            };
            Some(
                div()
                    .pt(px(8.0))
                    .text_color(color)
                    .text_size(px(12.0))
                    .child(label)
                    .into_any_element(),
            )
        } else if self.disconnected {
            Some(
                div()
                    .pt(px(8.0))
                    .text_color(theme::warning())
                    .text_size(px(12.0))
                    .child("[Disconnected - reconnecting...]")
                    .into_any_element(),
            )
        } else {
            None
        }
    }

    fn render_connection_badge(&self) -> impl IntoElement {
        let is_bridge = self.handle.is_bridge();
        let tooltip_text = if is_bridge {
            "Direct bridge to agent (bypasses server relay)".to_string()
        } else if self.mode == "local" {
            "Local WebSocket connection".to_string()
        } else {
            "WebSocket relay through server/agent".to_string()
        };
        let badge_label = if is_bridge { "Bridge" } else { "WS" };
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
                icon(if is_bridge { Icon::Zap } else { Icon::Wifi })
                    .size(px(10.0))
                    .text_color(if is_bridge {
                        theme::success()
                    } else {
                        theme::text_tertiary()
                    }),
            )
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme::text_tertiary())
                    .child(badge_label),
            )
            .child(div().size(px(6.0)).rounded_full().bg(if self.disconnected {
                theme::error()
            } else {
                theme::success()
            }))
            .tooltip(move |_window, cx| cx.new(|_| ConnectionTooltip(tooltip_text.clone())).into())
    }

    fn render_cc_metrics_badge(&self) -> Option<AnyElement> {
        let metrics = self.cc_metrics.as_ref()?;
        let status = self.cc_status?;

        let mut badge = div()
            .id("cc-metrics-badge")
            .absolute()
            .bottom(px(28.0))
            .right(px(8.0))
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(6.0))
            .py(px(2.0))
            .rounded(px(8.0))
            .bg(gpui::rgba(0x1111_1399))
            .child(cc_widgets::cc_bot_icon(status, 10.0));

        if let Some(ref model) = metrics.model {
            badge = badge.child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme::text_tertiary())
                    .child(cc_widgets::short_model_name(model)),
            );
        }

        badge = badge.child(cc_widgets::render_context_bar(metrics, 50.0, 4.0));

        if let Some(pct) = metrics.context_used_pct {
            let (_, pct_200k) =
                cc_widgets::context_usage_200k(pct, metrics.context_window_size.unwrap_or(200_000));
            badge = badge.child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme::text_tertiary())
                    .child(format!("{pct_200k:.0}%")),
            );
        }

        Some(badge.into_any_element())
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_output_reader(cx);
        self.start_cursor_blink(cx);

        if !self.focus_handle.is_focused(window) && !self.closed && !self.search_open {
            self.focus_handle.focus(window);
        }

        let terminal_el = self.render_terminal_element();
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
                let input_tx = self.handle.input_sender();
                let image_paste_tx = self.handle.image_paste_tx().cloned();
                let term = self.term.clone();
                let search_open = self.search_open;
                let entity = cx.entity().downgrade();
                let entity_id = cx.entity_id();
                let double_shift_kd = self.double_shift.clone();
                move |event: &KeyDownEvent, _window: &mut Window, cx: &mut App| {
                    let key = event.keystroke.key.as_str();
                    let mods = &event.keystroke.modifiers;

                    double_shift_kd.on_key_down_during_shift();

                    // Global shortcuts via centralized dispatch
                    if let Some(action) =
                        dispatch_global_key(key, mods.control, mods.shift, mods.alt)
                    {
                        let _ =
                            entity.update(
                                cx,
                                |this: &mut Self, cx: &mut Context<Self>| match action {
                                    KeyAction::OpenSearch => this.open_search(cx),
                                    KeyAction::OpenSessionSwitcher => {
                                        cx.emit(TerminalPanelEvent::OpenSessionSwitcher);
                                    }
                                    KeyAction::OpenCommandPalette(tab) => {
                                        if this.search_open {
                                            this.close_search(cx);
                                        }
                                        cx.emit(TerminalPanelEvent::OpenCommandPalette { tab });
                                    }
                                    KeyAction::OpenHelp => {
                                        cx.emit(TerminalPanelEvent::OpenHelp);
                                    }
                                    KeyAction::CloseOverlay => {}
                                },
                            );
                        cx.stop_propagation();
                        return;
                    }

                    if search_open {
                        return;
                    }

                    // Ctrl+C: copy selection if any, else send SIGINT
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
                            return;
                        }
                        if let Some(ref tx) = image_paste_tx
                            && let Some(b64) = read_clipboard_image_base64()
                        {
                            let _ = tx.send(b64);
                            return;
                        }
                    }

                    if let Some(bytes) = TerminalPanel::encode_keystroke(&event.keystroke) {
                        let _ = input_tx.send(bytes);
                    }
                }
            })
            .on_modifiers_changed({
                let hovered_url_idx = self.hovered_url_idx.clone();
                let entity_id = cx.entity_id();
                let entity_mc = cx.entity().downgrade();
                let double_shift_mc = self.double_shift.clone();
                move |event: &ModifiersChangedEvent, _window: &mut Window, cx: &mut App| {
                    let mods = &event.modifiers;

                    if !mods.control && hovered_url_idx.get().is_some() {
                        hovered_url_idx.set(None);
                        cx.notify(entity_id);
                    }

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
            .on_any_mouse_down({
                let focus = self.focus_handle.clone();
                move |_event: &MouseDownEvent, window: &mut Window, _cx: &mut App| {
                    focus.focus(window);
                }
            })
            .on_mouse_down(MouseButton::Left, {
                let term = self.term.clone();
                let layout_info = self.layout_info.clone();
                let entity_id = cx.entity_id();
                let url_detector = self.url_detector.clone();
                let hovered_url_idx = self.hovered_url_idx.clone();
                move |event: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
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
            .on_mouse_move({
                let term = self.term.clone();
                let layout_info = self.layout_info.clone();
                let entity_id = cx.entity_id();
                let url_detector = self.url_detector.clone();
                let hovered_url_idx = self.hovered_url_idx.clone();
                let content_generation = self.content_generation.clone();
                move |event: &MouseMoveEvent, _window: &mut Window, cx: &mut App| {
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
            .on_mouse_up(MouseButton::Left, {
                let entity_id = cx.entity_id();
                let term = self.term.clone();
                move |_event: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
                    if let Ok(mut t) = term.lock() {
                        if t.selection.as_ref().is_some_and(|s| s.is_empty()) {
                            t.selection = None;
                        } else if let Some(text) = t.selection_to_string() {
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

                    let is_precise = event.delta.precise();
                    let multiplier = 1.0_f32;

                    let line_delta = match event.touch_phase {
                        TouchPhase::Started => {
                            scroll_px.set(0.0);
                            None
                        }
                        TouchPhase::Moved => {
                            let pixel_delta = event.delta.pixel_delta(cell_h);
                            let old_offset = (px(scroll_px.get()) / cell_h) as i32;
                            let new_scroll_px =
                                scroll_px.get() + f32::from(pixel_delta.y) * multiplier;
                            let new_offset = (px(new_scroll_px) / cell_h) as i32;
                            let viewport_h =
                                f32::from(info.bounds.size.height).max(f32::from(cell_h));
                            scroll_px.set(new_scroll_px % viewport_h);
                            let delta = new_offset - old_offset;
                            if delta != 0 { Some(delta) } else { None }
                        }
                        TouchPhase::Ended => None,
                    };

                    if let Some(lines) = line_delta {
                        // precise = touchpad natural scroll: negate; wheel already correct direction
                        let scroll_lines = if is_precise { -lines } else { lines };
                        pending_scroll_delta.fetch_add(scroll_lines, Ordering::Relaxed);
                        cx.notify(entity_id);
                    }
                }
            })
            .focus(|style: StyleRefinement| style.border_color(theme::accent()).border_1())
            .child(terminal_el);

        if has_hovered_url {
            content = content.cursor(CursorStyle::PointingHand);
        }

        if let Some(status_overlay) = self.render_status_overlay() {
            content = content.child(status_overlay);
        }

        if let Some(cc_badge) = self.render_cc_metrics_badge() {
            content = content.child(cc_badge);
        }

        content = content.child(self.render_connection_badge());

        let mut wrapper = div().flex().flex_col().size_full();
        if let Some(overlay) = &self.search_overlay {
            wrapper = wrapper.child(overlay.clone());
        }
        wrapper.child(content)
    }
}

/// Minimal view for tooltip rendering (GPUI tooltips require `AnyView`).
struct ConnectionTooltip(String);

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
            .child(self.0.clone())
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

/// Strip Cursor Position Report responses (`ESC [ Ps ; Ps R`) from output bytes.
///
/// These are terminal-to-host responses that should never appear in the display stream,
/// but can leak through PTY echo during shell initialization (before the shell disables
/// echo mode). Only allocates when a CPR sequence is actually found.
///
/// **Limitation:** CPR sequences split across chunk boundaries are not stripped.
/// In practice this is acceptable because CPR leakage during shell init arrives
/// in a single PTY read, but callers should be aware.
fn strip_cpr_responses(data: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    use std::borrow::Cow;

    // Quick scan: if no ESC byte exists, return the original slice unchanged.
    if !data.contains(&0x1b) {
        return Cow::Borrowed(data);
    }

    let mut result = Vec::with_capacity(data.len());
    let mut i = 0;
    let mut found_cpr = false;

    while i < data.len() {
        if data[i] == 0x1b && i + 1 < data.len() && data[i + 1] == b'[' {
            // Try to match CPR pattern: ESC [ digits ; digits R
            let start = i;
            let mut j = i + 2;
            // First group of digits
            let d1_start = j;
            while j < data.len() && data[j].is_ascii_digit() {
                j += 1;
            }
            if j > d1_start && j < data.len() && data[j] == b';' {
                j += 1;
                let d2_start = j;
                while j < data.len() && data[j].is_ascii_digit() {
                    j += 1;
                }
                if j > d2_start && j < data.len() && data[j] == b'R' {
                    // Matched CPR response — skip it
                    if !found_cpr {
                        // First CPR found: copy everything before this point
                        result.extend_from_slice(&data[..start]);
                        found_cpr = true;
                    }
                    i = j + 1;
                    continue;
                }
            }
            // Not a CPR — copy bytes from start of this sequence
            if found_cpr {
                result.extend_from_slice(&data[start..j]);
            }
            i = j;
        } else {
            if found_cpr {
                result.push(data[i]);
            }
            i += 1;
        }
    }

    if found_cpr {
        Cow::Owned(result)
    } else {
        Cow::Borrowed(data)
    }
}

/// Convert an alacritty ANSI color to an optional hex string for preview rendering.
/// Returns `None` for the default foreground color.
fn ansi_color_to_hex(color: alacritty_terminal::vte::ansi::Color) -> Option<String> {
    use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor};

    match color {
        AnsiColor::Named(NamedColor::Foreground | NamedColor::Background) => None,
        AnsiColor::Named(name) => {
            let hex = match name {
                NamedColor::Black => "#1a1a1e",
                NamedColor::Red => "#ef4444",
                NamedColor::Green => "#4ade80",
                NamedColor::Yellow => "#facc15",
                NamedColor::Blue => "#60a5fa",
                NamedColor::Magenta => "#c084fc",
                NamedColor::Cyan => "#22d3ee",
                NamedColor::White => "#cccccc",
                NamedColor::BrightBlack => "#555555",
                NamedColor::BrightRed => "#f87171",
                NamedColor::BrightGreen => "#86efac",
                NamedColor::BrightYellow => "#fde68a",
                NamedColor::BrightBlue => "#93c5fd",
                NamedColor::BrightMagenta => "#d8b4fe",
                NamedColor::BrightCyan => "#67e8f9",
                NamedColor::BrightWhite => "#ffffff",
                _ => return None,
            };
            Some(hex.to_string())
        }
        AnsiColor::Spec(rgb) => Some(format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b)),
        AnsiColor::Indexed(idx) => {
            // Indices 0-15 map to named colors; higher indices use the 256-color palette
            if idx < 16 {
                // Re-use named color mapping via recursive call
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
                    15 => NamedColor::BrightWhite,
                    _ => return None,
                };
                ansi_color_to_hex(AnsiColor::Named(named))
            } else if idx < 232 {
                let i = u16::from(idx) - 16;
                let r = (i / 36) * 51;
                let g = ((i % 36) / 6) * 51;
                let b = (i % 6) * 51;
                Some(format!("#{r:02x}{g:02x}{b:02x}"))
            } else {
                let level = u16::from(idx - 232) * 10 + 8;
                Some(format!("#{level:02x}{level:02x}{level:02x}"))
            }
        }
    }
}

#[cfg(test)]
mod strip_cpr_tests {
    use super::strip_cpr_responses;

    #[test]
    fn no_esc_returns_borrowed() {
        let data = b"hello world";
        let result = strip_cpr_responses(data);
        assert!(matches!(result, std::borrow::Cow::Borrowed(_)));
        assert_eq!(&*result, b"hello world");
    }

    #[test]
    fn empty_input() {
        let result = strip_cpr_responses(b"");
        assert_eq!(&*result, b"");
    }

    #[test]
    fn single_cpr_stripped() {
        // ESC [ 4 ; 1 R
        let data = b"\x1b[4;1R";
        let result = strip_cpr_responses(data);
        assert_eq!(&*result, b"");
    }

    #[test]
    fn two_cprs_stripped() {
        let data = b"\x1b[4;1R\x1b[19;1R";
        let result = strip_cpr_responses(data);
        assert_eq!(&*result, b"");
    }

    #[test]
    fn cpr_embedded_in_text() {
        let data = b"before\x1b[4;1Rafter";
        let result = strip_cpr_responses(data);
        assert_eq!(&*result, b"beforeafter");
    }

    #[test]
    fn non_cpr_esc_sequence_preserved() {
        // SGR: ESC [ 1 m (bold)
        let data = b"\x1b[1m";
        let result = strip_cpr_responses(data);
        assert_eq!(&*result, b"\x1b[1m");
    }

    #[test]
    fn mixed_cpr_and_sgr() {
        // CPR then SGR then text
        let data = b"\x1b[4;1R\x1b[1mhello";
        let result = strip_cpr_responses(data);
        assert_eq!(&*result, b"\x1b[1mhello");
    }

    #[test]
    fn cpr_between_text_and_sgr() {
        let data = b"prompt\x1b[12;5R\x1b[32mgreen";
        let result = strip_cpr_responses(data);
        assert_eq!(&*result, b"prompt\x1b[32mgreen");
    }

    #[test]
    fn split_boundary_not_stripped() {
        // CPR split across chunks — first chunk has ESC [ digits
        let chunk1 = b"\x1b[12";
        let chunk2 = b";34R";
        let r1 = strip_cpr_responses(chunk1);
        let r2 = strip_cpr_responses(chunk2);
        // Split CPR is NOT stripped (documented limitation)
        assert_eq!(&*r1, b"\x1b[12");
        assert_eq!(&*r2, b";34R");
    }

    #[test]
    fn esc_at_end_of_buffer() {
        let data = b"text\x1b";
        let result = strip_cpr_responses(data);
        assert_eq!(&*result, b"text\x1b");
    }

    #[test]
    fn esc_bracket_at_end_of_buffer() {
        let data = b"text\x1b[";
        let result = strip_cpr_responses(data);
        assert_eq!(&*result, b"text\x1b[");
    }

    #[test]
    fn multi_param_sgr_after_cpr_preserved() {
        // ESC[1;32m is a two-param SGR (bold green) — must not be confused with CPR
        let data = b"\x1b[4;1R\x1b[1;32mtext";
        let result = strip_cpr_responses(data);
        assert_eq!(&*result, b"\x1b[1;32mtext");
    }
}
