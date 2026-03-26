//! URL detection in terminal viewport using alacritty's regex search.
//!
//! Scans visible terminal lines for URL patterns (http/https/file) and caches
//! results by `(display_offset, content_generation)` -- same invalidation
//! strategy as [`CellRunCache`].

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

use super::terminal_panel::TerminalTerm;

const URL_PATTERN: &str = r#"(?:https?://|file://)[^\s<>\[\]{}()'"`,;]+"#;

/// Maximum number of URL matches to cache per viewport.
const MAX_URL_MATCHES: usize = 1000;

pub struct UrlDetector {
    regex: Option<RegexSearch>,
    cached_matches: Vec<Match>,
    cached_offset: usize,
    cached_generation: u64,
}

impl UrlDetector {
    pub fn new() -> Self {
        let regex = RegexSearch::new(URL_PATTERN).ok();
        if regex.is_none() {
            tracing::warn!("failed to compile URL regex pattern");
        }
        Self {
            regex,
            cached_matches: Vec::new(),
            cached_offset: usize::MAX,
            cached_generation: u64::MAX,
        }
    }

    /// Detect URLs in the visible viewport. Returns cached results if
    /// display_offset and content_generation haven't changed.
    pub fn detect(
        &mut self,
        term: &TerminalTerm,
        display_offset: usize,
        generation: u64,
    ) -> &[Match] {
        if self.cached_offset == display_offset && self.cached_generation == generation {
            return &self.cached_matches;
        }

        self.cached_matches.clear();
        self.cached_offset = display_offset;
        self.cached_generation = generation;

        let Some(regex) = &mut self.regex else {
            return &self.cached_matches;
        };

        let rows = term.screen_lines();
        let cols = term.columns();
        if rows == 0 || cols == 0 {
            return &self.cached_matches;
        }

        // Visible viewport in grid coordinates.
        let start = Point::new(Line(-(display_offset as i32)), Column(0));
        let end = Point::new(
            Line(rows as i32 - 1 - display_offset as i32),
            Column(cols - 1),
        );

        let iter = RegexIter::new(start, end, Direction::Right, term, regex);
        for m in iter {
            self.cached_matches.push(m);
            if self.cached_matches.len() >= MAX_URL_MATCHES {
                break;
            }
        }

        &self.cached_matches
    }

    /// Find a URL match at the given grid point.
    /// Returns `(index, &match)` if found.
    pub fn match_at_point(&self, point: Point) -> Option<(usize, &Match)> {
        self.cached_matches
            .iter()
            .enumerate()
            .find(|(_, m)| point >= *m.start() && point <= *m.end())
    }

    /// Return a clone of the cached match at the given index.
    pub fn cached_match(&self, idx: usize) -> Option<Match> {
        self.cached_matches.get(idx).cloned()
    }

    /// Extract the URL text for a match at the given index by walking grid cells.
    pub fn url_text(&self, term: &TerminalTerm, idx: usize) -> String {
        let Some(m) = self.cached_matches.get(idx) else {
            return String::new();
        };

        let cols = term.columns();
        let mut text = String::new();
        let start = *m.start();
        let end = *m.end();
        let mut point = start;

        loop {
            let cell = &term.grid()[point];
            if cell.c != '\0' {
                text.push(cell.c);
            }

            if point == end {
                break;
            }

            // Advance to next cell (wrapping at end of line).
            if point.column.0 + 1 < cols {
                point.column.0 += 1;
            } else {
                point.column = Column(0);
                point.line += 1;
            }
        }

        text
    }
}
