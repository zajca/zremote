use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use vte::{Params, Perform};

const SNAPSHOT_ROWS: usize = 30;
const SNAPSHOT_COLS: usize = 120;

/// A compact snapshot of the terminal screen for preview rendering.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScreenSnapshot {
    pub lines: Vec<ScreenLine>,
    pub cols: u16,
    pub rows: u16,
}

/// A single line of terminal output with optional color spans.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScreenLine {
    pub text: String,
    pub spans: Vec<ColorSpan>,
}

/// A colored region within a screen line.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColorSpan {
    pub start: u16,
    pub end: u16,
    pub fg: String,
}

struct Cell {
    ch: char,
    fg: Option<String>,
}

impl Default for Cell {
    fn default() -> Self {
        Self { ch: ' ', fg: None }
    }
}

/// Internal grid state that implements `Perform`.
struct GridState {
    grid: VecDeque<Vec<Cell>>,
    cursor_row: usize,
    cursor_col: usize,
    current_fg: Option<String>,
}

impl GridState {
    fn new() -> Self {
        let grid = (0..SNAPSHOT_ROWS)
            .map(|_| (0..SNAPSHOT_COLS).map(|_| Cell::default()).collect())
            .collect();
        Self {
            grid,
            cursor_row: 0,
            cursor_col: 0,
            current_fg: None,
        }
    }
}

/// Lightweight VTE performer that maintains a fixed-size character grid
/// for generating terminal preview snapshots.
pub struct SnapshotParser {
    parser: vte::Parser,
    state: GridState,
}

impl Default for SnapshotParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotParser {
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: vte::Parser::new(),
            state: GridState::new(),
        }
    }

    /// Feed raw terminal bytes through the VTE parser.
    pub fn advance(&mut self, data: &[u8]) {
        self.parser.advance(&mut self.state, data);
    }

    /// Export the current grid as a compact `ScreenSnapshot`.
    ///
    /// Trailing empty lines and trailing whitespace per line are trimmed.
    #[must_use]
    pub fn snapshot(&self) -> ScreenSnapshot {
        let mut lines: Vec<ScreenLine> = Vec::new();

        for row in &self.state.grid {
            // Find last non-space character
            let last_non_space = row
                .iter()
                .rposition(|c| c.ch != ' ')
                .map_or(0, |pos| pos + 1);

            let text: String = row[..last_non_space].iter().map(|c| c.ch).collect();

            // Build color spans
            let mut spans: Vec<ColorSpan> = Vec::new();
            let mut span_start: Option<(usize, &str)> = None;

            for (col, cell) in row[..last_non_space].iter().enumerate() {
                if let Some(ref fg) = cell.fg {
                    match span_start {
                        Some((_start, current_fg)) if current_fg == fg.as_str() => {
                            // Continue current span
                        }
                        Some((start, current_fg)) => {
                            // End previous span, start new one
                            spans.push(ColorSpan {
                                start: u16::try_from(start).unwrap_or(u16::MAX),
                                end: u16::try_from(col).unwrap_or(u16::MAX),
                                fg: current_fg.to_string(),
                            });
                            span_start = Some((col, fg.as_str()));
                        }
                        None => {
                            span_start = Some((col, fg.as_str()));
                        }
                    }
                } else if let Some((start, current_fg)) = span_start.take() {
                    spans.push(ColorSpan {
                        start: u16::try_from(start).unwrap_or(u16::MAX),
                        end: u16::try_from(col).unwrap_or(u16::MAX),
                        fg: current_fg.to_string(),
                    });
                }
            }
            // Close any remaining span
            if let Some((start, current_fg)) = span_start {
                spans.push(ColorSpan {
                    start: u16::try_from(start).unwrap_or(u16::MAX),
                    end: u16::try_from(last_non_space).unwrap_or(u16::MAX),
                    fg: current_fg.to_string(),
                });
            }

            lines.push(ScreenLine { text, spans });
        }

        // Trim trailing empty lines
        while lines.last().is_some_and(|l| l.text.is_empty()) {
            lines.pop();
        }

        ScreenSnapshot {
            lines,
            cols: u16::try_from(SNAPSHOT_COLS).unwrap_or(u16::MAX),
            rows: u16::try_from(SNAPSHOT_ROWS).unwrap_or(u16::MAX),
        }
    }
}

impl GridState {
    fn scroll_up(&mut self) {
        self.grid.pop_front();
        self.grid
            .push_back((0..SNAPSHOT_COLS).map(|_| Cell::default()).collect());
    }

    fn clear_row(&mut self, row: usize) {
        if let Some(r) = self.grid.get_mut(row) {
            for cell in r.iter_mut() {
                *cell = Cell::default();
            }
        }
    }
}

impl Perform for GridState {
    fn print(&mut self, c: char) {
        if self.cursor_row < SNAPSHOT_ROWS && self.cursor_col < SNAPSHOT_COLS {
            self.grid[self.cursor_row][self.cursor_col] = Cell {
                ch: c,
                fg: self.current_fg.clone(),
            };
            self.cursor_col += 1;
            if self.cursor_col >= SNAPSHOT_COLS {
                // Wrap to next line
                self.cursor_col = 0;
                if self.cursor_row + 1 >= SNAPSHOT_ROWS {
                    self.scroll_up();
                } else {
                    self.cursor_row += 1;
                }
            }
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // CR - carriage return
            0x0D => self.cursor_col = 0,
            // LF - line feed
            0x0A => {
                if self.cursor_row + 1 >= SNAPSHOT_ROWS {
                    self.scroll_up();
                } else {
                    self.cursor_row += 1;
                }
            }
            // BS - backspace
            0x08 if self.cursor_col > 0 => {
                self.cursor_col -= 1;
            }
            // HT - horizontal tab
            0x09 => {
                self.cursor_col = ((self.cursor_col / 8) + 1) * 8;
                if self.cursor_col >= SNAPSHOT_COLS {
                    self.cursor_col = SNAPSHOT_COLS - 1;
                }
            }
            _ => {}
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}

    fn csi_dispatch(
        &mut self,
        params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let params_vec: Vec<u16> = params.iter().flat_map(|sub| sub.iter().copied()).collect();
        let p = |i: usize, default: u16| -> u16 {
            params_vec
                .get(i)
                .copied()
                .filter(|&v| v != 0)
                .unwrap_or(default)
        };

        match action {
            // CUU - Cursor Up
            'A' => {
                let n = usize::from(p(0, 1));
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            // CUD - Cursor Down
            'B' => {
                let n = usize::from(p(0, 1));
                self.cursor_row = (self.cursor_row + n).min(SNAPSHOT_ROWS - 1);
            }
            // CUF - Cursor Forward
            'C' => {
                let n = usize::from(p(0, 1));
                self.cursor_col = (self.cursor_col + n).min(SNAPSHOT_COLS - 1);
            }
            // CUB - Cursor Back
            'D' => {
                let n = usize::from(p(0, 1));
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            // CUP - Cursor Position (and 'f' variant)
            'H' | 'f' => {
                let row = usize::from(p(0, 1)).saturating_sub(1);
                let col = usize::from(p(1, 1)).saturating_sub(1);
                self.cursor_row = row.min(SNAPSHOT_ROWS - 1);
                self.cursor_col = col.min(SNAPSHOT_COLS - 1);
            }
            // ED - Erase in Display
            'J' => {
                let mode = p(0, 0);
                match mode {
                    0 => {
                        // Clear from cursor to end of screen
                        for col in self.cursor_col..SNAPSHOT_COLS {
                            self.grid[self.cursor_row][col] = Cell::default();
                        }
                        for row in (self.cursor_row + 1)..SNAPSHOT_ROWS {
                            self.clear_row(row);
                        }
                    }
                    1 => {
                        // Clear from start of screen to cursor
                        for row in 0..self.cursor_row {
                            self.clear_row(row);
                        }
                        for col in 0..=self.cursor_col.min(SNAPSHOT_COLS - 1) {
                            self.grid[self.cursor_row][col] = Cell::default();
                        }
                    }
                    2 | 3 => {
                        // Clear entire screen
                        for row in 0..SNAPSHOT_ROWS {
                            self.clear_row(row);
                        }
                    }
                    _ => {}
                }
            }
            // EL - Erase in Line
            'K' => {
                let mode = p(0, 0);
                match mode {
                    0 => {
                        // Clear from cursor to end of line
                        for col in self.cursor_col..SNAPSHOT_COLS {
                            self.grid[self.cursor_row][col] = Cell::default();
                        }
                    }
                    1 => {
                        // Clear from start of line to cursor
                        for col in 0..=self.cursor_col.min(SNAPSHOT_COLS - 1) {
                            self.grid[self.cursor_row][col] = Cell::default();
                        }
                    }
                    2 => {
                        // Clear entire line
                        self.clear_row(self.cursor_row);
                    }
                    _ => {}
                }
            }
            // SGR - Select Graphic Rendition
            'm' => {
                self.handle_sgr(&params_vec);
            }
            _ => {}
        }
    }
}

impl GridState {
    fn handle_sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.current_fg = None;
            return;
        }

        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    // Reset
                    self.current_fg = None;
                }
                // Basic foreground colors (30-37)
                n @ 30..=37 => {
                    self.current_fg = Some(basic_color(n - 30));
                }
                // Bright foreground colors (90-97)
                n @ 90..=97 => {
                    self.current_fg = Some(bright_color(n - 90));
                }
                // Extended foreground: 38;5;N or 38;2;R;G;B
                38 => {
                    if i + 1 < params.len() && params[i + 1] == 5 {
                        // 256-color mode
                        if i + 2 < params.len() {
                            self.current_fg = Some(color_256(params[i + 2]));
                            i += 2;
                        }
                    } else if i + 1 < params.len() && params[i + 1] == 2 {
                        // RGB mode
                        if i + 4 < params.len() {
                            let r = u8::try_from(params[i + 2]).unwrap_or(0);
                            let g = u8::try_from(params[i + 3]).unwrap_or(0);
                            let b = u8::try_from(params[i + 4]).unwrap_or(0);
                            self.current_fg = Some(format!("#{r:02x}{g:02x}{b:02x}"));
                            i += 4;
                        }
                    }
                }
                // Default foreground
                39 => {
                    self.current_fg = None;
                }
                _ => {
                    // Ignore background, bold, italic, etc.
                }
            }
            i += 1;
        }
    }
}

const BASIC_COLORS: [&str; 8] = [
    "#000000", "#aa0000", "#00aa00", "#aa5500", "#0000aa", "#aa00aa", "#00aaaa", "#aaaaaa",
];

const BRIGHT_COLORS: [&str; 8] = [
    "#555555", "#ff5555", "#55ff55", "#ffff55", "#5555ff", "#ff55ff", "#55ffff", "#ffffff",
];

fn basic_color(idx: u16) -> String {
    BASIC_COLORS
        .get(usize::from(idx))
        .unwrap_or(&"#aaaaaa")
        .to_string()
}

fn bright_color(idx: u16) -> String {
    BRIGHT_COLORS
        .get(usize::from(idx))
        .unwrap_or(&"#ffffff")
        .to_string()
}

fn color_256(idx: u16) -> String {
    if idx < 8 {
        return basic_color(idx);
    }
    if idx < 16 {
        return bright_color(idx - 8);
    }
    if idx < 232 {
        // 6x6x6 color cube
        let idx = idx - 16;
        let r = (idx / 36) * 51;
        let g = ((idx % 36) / 6) * 51;
        let b = (idx % 6) * 51;
        let r = u8::try_from(r).unwrap_or(0);
        let g = u8::try_from(g).unwrap_or(0);
        let b = u8::try_from(b).unwrap_or(0);
        return format!("#{r:02x}{g:02x}{b:02x}");
    }
    // Grayscale ramp (232-255)
    let level = u8::try_from((idx - 232) * 10 + 8).unwrap_or(255);
    format!("#{level:02x}{level:02x}{level:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_text_output() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"Hello, World!");
        let snap = parser.snapshot();
        assert_eq!(snap.lines.len(), 1);
        assert_eq!(snap.lines[0].text, "Hello, World!");
        assert!(snap.lines[0].spans.is_empty());
    }

    #[test]
    fn newline_moves_cursor_down() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"Line 1\r\nLine 2\r\nLine 3");
        let snap = parser.snapshot();
        assert_eq!(snap.lines.len(), 3);
        assert_eq!(snap.lines[0].text, "Line 1");
        assert_eq!(snap.lines[1].text, "Line 2");
        assert_eq!(snap.lines[2].text, "Line 3");
    }

    #[test]
    fn cursor_movement_up_down() {
        let mut parser = SnapshotParser::new();
        // Write on row 0, move down 2, write
        parser.advance(b"Row0\x1b[2BDown");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "Row0");
        assert_eq!(snap.lines[2].text, "    Down");
    }

    #[test]
    fn cursor_position_absolute() {
        let mut parser = SnapshotParser::new();
        // Move to row 3, col 5 (1-based)
        parser.advance(b"\x1b[3;5HHere");
        let snap = parser.snapshot();
        assert!(snap.lines.len() >= 3);
        assert_eq!(&snap.lines[2].text[4..], "Here");
    }

    #[test]
    fn color_basic_foreground() {
        let mut parser = SnapshotParser::new();
        // Set red (31), write text, reset
        parser.advance(b"\x1b[31mRed\x1b[0m");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "Red");
        assert_eq!(snap.lines[0].spans.len(), 1);
        assert_eq!(snap.lines[0].spans[0].fg, "#aa0000");
        assert_eq!(snap.lines[0].spans[0].start, 0);
        assert_eq!(snap.lines[0].spans[0].end, 3);
    }

    #[test]
    fn color_256_mode() {
        let mut parser = SnapshotParser::new();
        // 256-color: index 196 (bright red)
        parser.advance(b"\x1b[38;5;196mHi\x1b[0m");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "Hi");
        assert_eq!(snap.lines[0].spans.len(), 1);
    }

    #[test]
    fn color_rgb() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"\x1b[38;2;255;128;0mOrange\x1b[0m");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "Orange");
        assert_eq!(snap.lines[0].spans[0].fg, "#ff8000");
    }

    #[test]
    fn scrolling_at_bottom() {
        let mut parser = SnapshotParser::new();
        // Write 31 lines to trigger scrolling (grid is 30 rows)
        // Each "Line N\r\n" writes text then moves cursor down.
        // After writing "Line 30\r\n", the cursor has scrolled past the grid bottom twice,
        // so Lines 0 and 1 scroll off.
        for i in 0..31 {
            parser.advance(format!("Line {i}\r\n").as_bytes());
        }
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "Line 2");
    }

    #[test]
    fn erase_entire_screen() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"Hello");
        parser.advance(b"\x1b[2J");
        let snap = parser.snapshot();
        assert!(snap.lines.is_empty());
    }

    #[test]
    fn erase_line_from_cursor() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"Hello World");
        // Move cursor to col 5, erase to end of line
        parser.advance(b"\x1b[1;6H\x1b[K");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "Hello");
    }

    #[test]
    fn carriage_return_overwrites() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"AAAA\rBB");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "BBAA");
    }

    #[test]
    fn tab_stop() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"A\tB");
        let snap = parser.snapshot();
        // Tab advances to col 8
        assert_eq!(snap.lines[0].text, "A       B");
    }

    #[test]
    fn backspace() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"AB\x08C");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "AC");
    }

    #[test]
    fn empty_grid_snapshot() {
        let parser = SnapshotParser::new();
        let snap = parser.snapshot();
        assert!(snap.lines.is_empty());
        assert_eq!(snap.cols, 120);
        assert_eq!(snap.rows, 30);
    }

    #[test]
    fn snapshot_serialization_roundtrip() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"\x1b[32mGreen\x1b[0m Normal");
        let snap = parser.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: ScreenSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.lines[0].text, snap.lines[0].text);
        assert_eq!(parsed.lines[0].spans.len(), snap.lines[0].spans.len());
    }

    #[test]
    fn multiple_color_spans() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"\x1b[31mRed\x1b[32mGreen\x1b[0mNone");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "RedGreenNone");
        assert_eq!(snap.lines[0].spans.len(), 2);
        assert_eq!(snap.lines[0].spans[0].fg, "#aa0000");
        assert_eq!(snap.lines[0].spans[0].start, 0);
        assert_eq!(snap.lines[0].spans[0].end, 3);
        assert_eq!(snap.lines[0].spans[1].fg, "#00aa00");
        assert_eq!(snap.lines[0].spans[1].start, 3);
        assert_eq!(snap.lines[0].spans[1].end, 8);
    }

    #[test]
    fn bright_colors() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"\x1b[91mBright\x1b[0m");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].spans[0].fg, "#ff5555");
    }

    #[test]
    fn erase_from_start_of_line() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"Hello World");
        parser.advance(b"\x1b[1;6H\x1b[1K");
        let snap = parser.snapshot();
        // Cols 0-5 should be cleared, "World" remains at cols 6-10
        assert_eq!(snap.lines[0].text, "      World");
    }

    #[test]
    fn erase_entire_line() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"Hello World");
        parser.advance(b"\x1b[2K");
        let snap = parser.snapshot();
        assert!(snap.lines.is_empty());
    }

    #[test]
    fn cursor_forward_backward() {
        let mut parser = SnapshotParser::new();
        parser.advance(b"ABCDE\x1b[3DX");
        let snap = parser.snapshot();
        assert_eq!(snap.lines[0].text, "ABXDE");
    }
}
