//! Shared UI widgets for Claude Code session metrics display.

use std::fmt::Write as _;

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::icons::{Icon, icon};
use crate::theme;
use crate::views::sidebar::CcMetrics;
use zremote_client::AgenticStatus;

/// Render a context usage progress bar.
///
/// The bar shows usage relative to a 200k token baseline:
/// - 200,000 tokens = 100% fill
/// - Larger context windows can exceed 100% fill visually (capped at bar width)
/// - Color: green (<70%), yellow (70-90%), red (>90%)
/// - Overflow indicator when context_window_size > 200k and usage is high
#[allow(clippy::cast_possible_truncation)]
pub fn render_context_bar(metrics: &CcMetrics, width: f32, height: f32) -> impl IntoElement {
    let pct = metrics.context_used_pct.unwrap_or(0.0);
    let window_size = metrics.context_window_size.unwrap_or(200_000);

    // Calculate tokens used, then normalize to 200k baseline
    let (tokens_used, _) = context_usage_200k(pct, window_size);
    let fill_ratio_200k = (tokens_used / 200_000.0).min(1.0) as f32;

    let fill_color = if pct > 90.0 {
        theme::error()
    } else if pct > 70.0 {
        theme::warning()
    } else {
        theme::success()
    };

    let exceeds_200k = window_size > 200_000 && tokens_used > 160_000.0;

    div().flex().items_center().gap(px(4.0)).child(
        div()
            .w(px(width))
            .h(px(height))
            .rounded(px(height / 2.0))
            .bg(theme::bg_primary())
            .overflow_hidden()
            .child(
                div()
                    .h_full()
                    .w(relative(fill_ratio_200k))
                    .rounded(px(height / 2.0))
                    .bg(fill_color),
            )
            .when(exceeds_200k, |d: Div| {
                d.border_1().border_color(theme::warning())
            }),
    )
}

/// Shorten a model display name for compact UI display.
///
/// Examples:
/// - "Opus 4.6 (1M context)" -> "Opus4.6"
/// - "Sonnet 4.6" -> "Son4.6"
/// - "Haiku 4.5" -> "Hai4.5"
/// - "claude-opus-4-6" -> "Opus4.6"
pub fn short_model_name(model: &str) -> String {
    // Try to extract family + version from display name patterns
    let lower = model.to_lowercase();

    if let Some(rest) = lower.strip_prefix("opus ").or_else(|| {
        lower
            .find("opus")
            .map(|i| &lower[i + 4..])
            .map(|s| s.trim_start_matches([' ', '-']))
    }) {
        let version = extract_version(rest);
        return format!("Opus{version}");
    }
    if let Some(rest) = lower.strip_prefix("sonnet ").or_else(|| {
        lower
            .find("sonnet")
            .map(|i| &lower[i + 6..])
            .map(|s| s.trim_start_matches([' ', '-']))
    }) {
        let version = extract_version(rest);
        return format!("Son{version}");
    }
    if let Some(rest) = lower.strip_prefix("haiku ").or_else(|| {
        lower
            .find("haiku")
            .map(|i| &lower[i + 5..])
            .map(|s| s.trim_start_matches([' ', '-']))
    }) {
        let version = extract_version(rest);
        return format!("Hai{version}");
    }

    // Fallback: first 8 chars
    model.chars().take(8).collect()
}

/// Extract version number (major.minor) from start of string.
///
/// Accepts dots or dashes as separators and normalizes to dots.
/// Only keeps the first two numeric segments (major.minor):
/// - `"4.6 (1M context)"` -> `"4.6"`
/// - `"-4-6-20250514"` -> `"4.6"` (leading dashes stripped, trailing ignored)
fn extract_version(s: &str) -> String {
    let s = s.trim_start_matches([' ', '-']);
    let raw: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    // Normalize dashes to dots and keep only major.minor
    let normalized = raw.replace('-', ".");
    let mut parts = normalized.split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) => format!("{major}.{minor}"),
        (Some(major), None) => major.to_string(),
        _ => String::new(),
    }
}

/// Calculate context usage normalized to a 200k token baseline.
/// Returns (tokens_used, percentage_of_200k).
pub fn context_usage_200k(pct: f64, window_size: u64) -> (f64, f64) {
    let tokens_used = (pct / 100.0) * window_size as f64;
    let pct_200k = tokens_used / 200_000.0 * 100.0;
    (tokens_used, pct_200k)
}

/// Format cost as "$X.XX".
fn format_cost(cost_usd: f64) -> String {
    format!("${cost_usd:.2}")
}

/// Bot icon colored by agentic status.
pub fn cc_bot_icon(status: AgenticStatus, size: f32) -> gpui::Svg {
    let color = match status {
        AgenticStatus::Working => theme::accent(),
        AgenticStatus::RequiresAction | AgenticStatus::WaitingForInput => theme::warning(),
        AgenticStatus::Error => theme::error(),
        AgenticStatus::Completed => theme::success(),
        AgenticStatus::Idle | AgenticStatus::Unknown => theme::text_tertiary(),
    };
    icon(Icon::Bot).size(px(size)).text_color(color)
}

/// Render detailed tooltip content for CC session metrics.
pub fn render_cc_tooltip(
    metrics: &CcMetrics,
    status: Option<AgenticStatus>,
    task_name: Option<&str>,
    permission_mode: Option<&str>,
) -> impl IntoElement {
    let mut content = div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .p(px(8.0))
        .rounded(px(6.0))
        .bg(theme::bg_tertiary())
        .border_1()
        .border_color(theme::border())
        .max_w(px(280.0))
        .text_size(px(12.0))
        .text_color(theme::text_primary());

    // Status + task
    if let Some(status) = status {
        let status_text = match status {
            AgenticStatus::Working => "Working",
            AgenticStatus::RequiresAction => "Requires action",
            AgenticStatus::WaitingForInput => "Waiting for input",
            AgenticStatus::Idle => "Idle",
            AgenticStatus::Error => "Error",
            AgenticStatus::Completed => "Completed",
            AgenticStatus::Unknown => "Unknown",
        };
        let mut line = format!("Status: {status_text}");
        if let Some(task) = task_name {
            let _ = write!(line, " - {task}");
        }
        content = content.child(div().child(line));
    }

    // Mode
    if let Some(mode) = permission_mode.filter(|m| *m != "default") {
        content = content.child(div().child(format!("Mode: {mode}")));
    }

    // Model
    if let Some(ref model) = metrics.model {
        content = content.child(div().child(format!("Model: {model}")));
    }

    // Context
    if let Some(pct) = metrics.context_used_pct {
        let window = metrics.context_window_size.unwrap_or(200_000);
        let (tokens_used, _) = context_usage_200k(pct, window);
        let window_label = if window >= 1_000_000 {
            format!("{}M", window / 1_000_000)
        } else {
            format!("{}k", window / 1000)
        };
        content = content.child(div().child(format!(
            "Context: {:.0}k tokens ({pct:.0}% of {window_label})",
            tokens_used / 1000.0,
        )));
    }

    // Cost
    if let Some(cost) = metrics.cost_usd {
        content = content.child(div().child(format!("Cost: {}", format_cost(cost))));
    }

    // Lines
    if metrics.lines_added.is_some() || metrics.lines_removed.is_some() {
        let added = metrics.lines_added.unwrap_or(0);
        let removed = metrics.lines_removed.unwrap_or(0);
        content = content.child(div().child(format!("Lines: +{added} / -{removed}")));
    }

    // Rate limits
    if metrics.rate_limit_5h_pct.is_some() || metrics.rate_limit_7d_pct.is_some() {
        let r5 = metrics
            .rate_limit_5h_pct
            .map_or("-".to_string(), |v| format!("{v}%"));
        let r7 = metrics
            .rate_limit_7d_pct
            .map_or("-".to_string(), |v| format!("{v}%"));
        content = content.child(div().child(format!("Rate limits: 5h: {r5} | 7d: {r7}")));
    }

    content
}

/// Returns (background, foreground, label) for a permission mode badge.
pub fn permission_mode_badge_style(mode: &str) -> (Rgba, Rgba, &'static str) {
    match mode {
        "plan" => {
            let c = theme::warning();
            (Rgba { a: 0.15, ..c }, c, "plan")
        }
        "auto" => {
            let c = theme::accent();
            (Rgba { a: 0.15, ..c }, c, "auto")
        }
        "acceptEdits" => {
            let c = theme::success();
            (Rgba { a: 0.15, ..c }, c, "accept")
        }
        "bypassPermissions" | "dontAsk" => {
            let c = theme::error();
            (Rgba { a: 0.15, ..c }, c, "bypass")
        }
        _ => {
            let c = theme::text_tertiary();
            (Rgba { a: 0.15, ..c }, c, "other")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{context_usage_200k, extract_version, format_cost, short_model_name};

    #[test]
    fn short_model_name_display_names() {
        assert_eq!(short_model_name("Opus 4.6 (1M context)"), "Opus4.6");
        assert_eq!(short_model_name("Sonnet 4.6"), "Son4.6");
        assert_eq!(short_model_name("Haiku 4.5"), "Hai4.5");
    }

    #[test]
    fn short_model_name_raw_ids() {
        assert_eq!(short_model_name("claude-opus-4-6"), "Opus4.6");
        assert_eq!(short_model_name("claude-sonnet-4-6"), "Son4.6");
        assert_eq!(short_model_name("claude-haiku-4-5"), "Hai4.5");
    }

    #[test]
    fn short_model_name_fallback() {
        assert_eq!(short_model_name("gpt-4o"), "gpt-4o");
        assert_eq!(short_model_name("some-long-model-name"), "some-lon");
    }

    #[test]
    fn extract_version_patterns() {
        assert_eq!(extract_version("4.6 (1M context)"), "4.6");
        assert_eq!(extract_version("4-6"), "4.6");
        assert_eq!(extract_version("-4-6-20250514"), "4.6");
        assert_eq!(extract_version(""), "");
    }

    #[test]
    fn context_usage_200k_basic() {
        let (tokens, pct) = context_usage_200k(50.0, 200_000);
        assert!((tokens - 100_000.0).abs() < 1.0);
        assert!((pct - 50.0).abs() < 0.1);
    }

    #[test]
    fn context_usage_200k_large_window() {
        // 10% of 1M = 100k tokens = 50% of 200k
        let (tokens, pct) = context_usage_200k(10.0, 1_000_000);
        assert!((tokens - 100_000.0).abs() < 1.0);
        assert!((pct - 50.0).abs() < 0.1);
    }

    #[test]
    fn format_cost_basic() {
        assert_eq!(format_cost(1.5), "$1.50");
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(99.999), "$100.00");
    }
}
