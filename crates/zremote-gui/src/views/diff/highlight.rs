//! Syntect → GPUI bridge for the diff view.
//!
//! The protocol ships structured hunks (not full file contents) — see
//! `zremote-protocol::project::diff::DiffFile`. So we highlight hunk-line
//! content only, one pass per "side" (pre-image vs. post-image). Multi-line
//! constructs spanning unchanged regions are a known best-effort limitation
//! that follows from the wire shape; the RFC §4.3 ideal (pre-highlight the
//! full file) is blocked until the agent ships full blobs alongside hunks.
//!
//! `HighlightEngine::global()` is instantiated once per process and owns the
//! bundled syntect syntax set + an in-code `Theme` mapped to `theme::*()`.
//! Results are cached on `DiffView` keyed by `(blob_sha, syntax_name,
//! SideKey)` so scroll and view-mode toggles do not re-highlight.

use std::ops::Range;
use std::str::FromStr;
use std::sync::OnceLock;

use gpui::{FontStyle as GpuiFontStyle, FontWeight, HighlightStyle};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color, FontStyle, ScopeSelectors, Style, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::theme;

/// Per-line span: the inner Vec is the run of `(byte-range, style)` pairs
/// produced by syntect for that single line. Byte ranges are absolute
/// offsets within the per-line string (never cumulative across lines), and
/// the tuple order matches GPUI's `StyledText::with_highlights` contract so
/// consumers can feed them straight through without swapping.
pub type LineSpans = Vec<(Range<usize>, HighlightStyle)>;

/// Upper bound on single-file input in bytes. Larger files render without
/// highlight (diff-only colours). Matches RFC §4.3 cap.
pub const HIGHLIGHT_MAX_BYTES: usize = 1_048_576;

/// Upper bound on line count per file. Larger files render without
/// highlight. Matches RFC §4.3 cap.
pub const HIGHLIGHT_MAX_LINES: usize = 10_000;

/// Marks which side a highlight cache entry belongs to. Kept in the cache
/// key so old vs. new content of a renamed/modified file don't clobber
/// each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SideKey {
    Old,
    New,
}

/// Lazy process-wide highlight engine. `SyntaxSet::load_defaults_newlines`
/// allocates a few MB and ~50 ms at first call, so we amortise it behind a
/// `OnceLock`.
pub struct HighlightEngine {
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl HighlightEngine {
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<HighlightEngine> = OnceLock::new();
        INSTANCE.get_or_init(Self::new)
    }

    /// Warm up the process-wide engine off the render path. Callers that
    /// want to front-load the ~50 ms `SyntaxSet::load_defaults_newlines`
    /// cost call this from a background task on startup; subsequent
    /// `global()` calls from render paths are then synchronous no-ops.
    pub fn prime() {
        let _ = Self::global();
    }

    fn new() -> Self {
        // newline-preserving syntaxes — required by syntect's state machine
        // so multi-line constructs (block comments, template strings)
        // transition correctly.
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme = build_minimal_theme();
        Self { syntax_set, theme }
    }

    /// Resolve a path's extension to a syntect syntax. Falls back to
    /// `plain text` for unknown / missing extensions so highlighting never
    /// panics on arbitrary paths.
    pub fn detect_syntax(&self, path: &str) -> &SyntaxReference {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        self.syntax_set
            .find_syntax_by_extension(ext)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
    }

    /// Highlight a block of text line-by-line. Returns per-line spans; the
    /// outer Vec index matches the 0-based line order of `text`. Each
    /// inner Vec holds `(HighlightStyle, byte_range)` pairs addressed
    /// relative to the START of that line (not the whole text), so a
    /// consumer can feed them directly to `StyledText::with_highlights`.
    pub fn highlight_file(&self, text: &str, syntax: &SyntaxReference) -> Vec<LineSpans> {
        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(text) {
            // If syntect hits a pathological input, skip the line rather
            // than poison the whole file: empty spans = diff-only colours
            // for that one row.
            let ranges = match highlighter.highlight_line(line, &self.syntax_set) {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("syntect highlight failed on line: {e}");
                    out.push(Vec::new());
                    continue;
                }
            };
            let mut spans: LineSpans = Vec::with_capacity(ranges.len());
            let mut cursor = 0usize;
            for (style, piece) in ranges {
                let start = cursor;
                let end = start + piece.len();
                cursor = end;
                // Skip pure-whitespace pieces — they produce no visible
                // ink, and the default text colour already matches.
                if piece.chars().all(char::is_whitespace) {
                    continue;
                }
                spans.push((start..end, syntect_style_to_gpui(style)));
            }
            out.push(spans);
        }
        out
    }
}

/// Decide whether a file is small enough to highlight at all.
#[must_use]
pub fn should_highlight(text: &str) -> bool {
    if text.len() > HIGHLIGHT_MAX_BYTES {
        return false;
    }
    // `LinesWithEndings::from` iterates lazily so we can short-circuit.
    let mut count = 0usize;
    for _ in LinesWithEndings::from(text) {
        count += 1;
        if count > HIGHLIGHT_MAX_LINES {
            return false;
        }
    }
    true
}

fn syntect_style_to_gpui(style: Style) -> HighlightStyle {
    let color = gpui::Rgba {
        r: f32::from(style.foreground.r) / 255.0,
        g: f32::from(style.foreground.g) / 255.0,
        b: f32::from(style.foreground.b) / 255.0,
        a: f32::from(style.foreground.a) / 255.0,
    };
    let font_weight = if style.font_style.contains(FontStyle::BOLD) {
        Some(FontWeight::BOLD)
    } else {
        None
    };
    let font_style = if style.font_style.contains(FontStyle::ITALIC) {
        Some(GpuiFontStyle::Italic)
    } else {
        None
    };
    HighlightStyle {
        color: Some(color.into()),
        font_weight,
        font_style,
        ..Default::default()
    }
}

fn rgba_to_color(c: gpui::Rgba) -> Color {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        Color {
            r: (c.r * 255.0).round().clamp(0.0, 255.0) as u8,
            g: (c.g * 255.0).round().clamp(0.0, 255.0) as u8,
            b: (c.b * 255.0).round().clamp(0.0, 255.0) as u8,
            a: (c.a * 255.0).round().clamp(0.0, 255.0) as u8,
        }
    }
}

fn selectors(s: &str) -> ScopeSelectors {
    // `ScopeSelectors::from_str` is infallible for the simple comma/space
    // forms we feed it from this module; if it ever breaks, we swallow the
    // error (empty selectors = no match, equivalent to "rule skipped").
    ScopeSelectors::from_str(s).unwrap_or_default()
}

fn item(scope: &str, color: gpui::Rgba, bold: bool) -> ThemeItem {
    let mut style = FontStyle::empty();
    if bold {
        style |= FontStyle::BOLD;
    }
    ThemeItem {
        scope: selectors(scope),
        style: StyleModifier {
            foreground: Some(rgba_to_color(color)),
            background: None,
            font_style: Some(style),
        },
    }
}

/// Build a syntect `Theme` from scratch using the ZRemote `theme::syntax_*()`
/// palette. We do **not** bundle any Sublime-format .tmTheme — this keeps the
/// colour space under product control and avoids clashing with our UI palette
/// (RFC §4.3).
fn build_minimal_theme() -> Theme {
    let text = theme::text_primary();
    Theme {
        name: Some("ZRemote Diff".to_string()),
        author: Some("zremote".to_string()),
        settings: ThemeSettings {
            foreground: Some(rgba_to_color(text)),
            background: Some(rgba_to_color(theme::bg_primary())),
            ..ThemeSettings::default()
        },
        scopes: vec![
            item("comment", theme::syntax_comment(), false),
            item("string, string.quoted", theme::syntax_string(), false),
            item("constant.character.escape", theme::syntax_string(), false),
            item("constant.numeric", theme::syntax_number(), false),
            item(
                "constant.language, constant.other",
                theme::syntax_constant(),
                false,
            ),
            item(
                "keyword, keyword.control, keyword.operator",
                theme::syntax_keyword(),
                true,
            ),
            item(
                "storage, storage.type, storage.modifier",
                theme::syntax_keyword(),
                true,
            ),
            item(
                "entity.name.function, support.function, meta.function-call",
                theme::syntax_function(),
                false,
            ),
            item(
                "entity.name.type, entity.name.class, entity.name.struct, support.class, support.type",
                theme::syntax_type(),
                false,
            ),
            item(
                "variable.parameter, variable.other",
                theme::syntax_variable(),
                false,
            ),
            item("punctuation", theme::syntax_punctuation(), false),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_syntax_rust() {
        let e = HighlightEngine::global();
        let s = e.detect_syntax("foo.rs");
        assert_eq!(s.name.to_lowercase(), "rust");
    }

    #[test]
    fn detect_syntax_typescript_or_plain_text() {
        // syntect's bundled `default-fancy` assets DO NOT include
        // TypeScript. We still want `.ts` paths to resolve deterministically
        // rather than panic — the acceptable outcomes are either "TypeScript"
        // (if a future dep bump starts bundling it) or "Plain Text" (current
        // reality). Anything else means the fallback logic broke.
        let e = HighlightEngine::global();
        let s = e.detect_syntax("bar.ts");
        let name = s.name.to_lowercase();
        assert!(
            name.contains("typescript") || name == "plain text",
            "unexpected ts syntax: {name}"
        );
    }

    #[test]
    fn detect_syntax_python() {
        let e = HighlightEngine::global();
        let s = e.detect_syntax("baz.py");
        assert_eq!(s.name.to_lowercase(), "python");
    }

    #[test]
    fn detect_syntax_go() {
        let e = HighlightEngine::global();
        let s = e.detect_syntax("qux.go");
        assert_eq!(s.name.to_lowercase(), "go");
    }

    #[test]
    fn detect_syntax_markdown() {
        let e = HighlightEngine::global();
        let s = e.detect_syntax("README.md");
        let name = s.name.to_lowercase();
        assert!(
            name.contains("markdown") || name.contains("md"),
            "name: {name}"
        );
    }

    #[test]
    fn detect_syntax_no_extension_is_plain_text() {
        let e = HighlightEngine::global();
        let s = e.detect_syntax("no_extension");
        assert_eq!(s.name.to_lowercase(), "plain text");
    }

    #[test]
    fn detect_syntax_unknown_is_plain_text() {
        let e = HighlightEngine::global();
        let s = e.detect_syntax("weird.xyz");
        assert_eq!(s.name.to_lowercase(), "plain text");
    }

    #[test]
    fn highlight_stability() {
        let e = HighlightEngine::global();
        let rust = e.detect_syntax("x.rs");
        let text = "fn foo() -> i32 { 42 }\n";
        let a = e.highlight_file(text, rust);
        let b = e.highlight_file(text, rust);
        assert_eq!(a, b);
        // Sanity: we got at least one non-empty span.
        assert!(a.iter().any(|line| !line.is_empty()));
    }

    #[test]
    fn highlight_byte_ranges_are_per_line_relative() {
        let e = HighlightEngine::global();
        let rust = e.detect_syntax("x.rs");
        let text = "fn a() {}\nfn b() {}\n";
        let out = e.highlight_file(text, rust);
        assert_eq!(out.len(), 2);
        // Every range must fit inside the corresponding line's byte length,
        // confirming ranges are RELATIVE to the line start, not absolute
        // into the whole file.
        let line0_len = "fn a() {}\n".len();
        let line1_len = "fn b() {}\n".len();
        for (r, _) in &out[0] {
            assert!(r.end <= line0_len, "line0 range {r:?} > {line0_len}");
        }
        for (r, _) in &out[1] {
            assert!(r.end <= line1_len, "line1 range {r:?} > {line1_len}");
        }
    }

    #[test]
    fn should_highlight_small_text_is_true() {
        assert!(should_highlight("fn foo() {}\n"));
    }

    #[test]
    fn should_highlight_over_byte_limit_is_false() {
        let big = "a".repeat(HIGHLIGHT_MAX_BYTES + 1);
        assert!(!should_highlight(&big));
    }

    #[test]
    fn should_highlight_over_line_limit_is_false() {
        // Small total bytes but too many lines.
        let many = "x\n".repeat(HIGHLIGHT_MAX_LINES + 1);
        assert!(!should_highlight(&many));
    }

    #[test]
    fn highlight_large_file_not_highlighted_via_gate() {
        // Caller contract: gate with `should_highlight` first; if false,
        // skip `highlight_file` entirely. Assert the gate behaves so callers
        // don't accidentally burn CPU on giant inputs.
        let huge = "a".repeat(HIGHLIGHT_MAX_BYTES + 10);
        assert!(!should_highlight(&huge));
    }
}
