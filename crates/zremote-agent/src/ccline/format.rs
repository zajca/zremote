use super::input::StatusInput;
use std::fmt::Write;

// ANSI color codes
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Abbreviate the model display name to a short form.
fn short_model(display_name: &str) -> &str {
    // "Opus 4.6 (1M context)" -> "Opus 4.6"
    // Strip parenthetical suffixes
    display_name
        .find('(')
        .map_or(display_name, |i| display_name[..i].trim_end())
}

/// Color a percentage value: green <60%, yellow 60-80%, red >80%.
fn color_pct(pct: u64) -> &'static str {
    if pct >= 80 {
        RED
    } else if pct >= 60 {
        YELLOW
    } else {
        GREEN
    }
}

/// Format the status line output from parsed input.
/// Returns the ANSI-colored string to print to stdout.
pub fn format_status(input: &StatusInput, git_branch: Option<&str>) -> String {
    let mut out = String::with_capacity(128);

    // Model name
    if let Some(ref model) = input.model
        && let Some(ref name) = model.display_name
    {
        let _ = write!(out, "{DIM}[{RESET}{}{DIM}]{RESET}", short_model(name));
    }

    // Context usage
    if let Some(ref ctx) = input.context_window
        && let Some(pct) = ctx.used_percentage
    {
        let color = color_pct(pct);
        let _ = write!(out, " {color}ctx:{pct}%{RESET}");
    }

    // Cost
    if let Some(ref cost) = input.cost {
        if let Some(usd) = cost.total_cost_usd {
            let _ = write!(out, " ${usd:.2}");
        }

        // Lines added/removed
        match (cost.total_lines_added, cost.total_lines_removed) {
            (Some(added), Some(removed)) if added > 0 || removed > 0 => {
                let _ = write!(out, " {GREEN}+{added}{RESET}/{RED}-{removed}{RESET}");
            }
            _ => {}
        }
    }

    // Rate limits
    let has_rate_limits = input
        .rate_limits
        .as_ref()
        .is_some_and(|r| r.five_hour.is_some() || r.seven_day.is_some());

    if has_rate_limits {
        let _ = write!(out, " {DIM}|{RESET}");
        if let Some(ref limits) = input.rate_limits {
            if let Some(ref five) = limits.five_hour
                && let Some(pct) = five.used_percentage
            {
                let color = color_pct(pct);
                let _ = write!(out, " {color}5h:{pct}%{RESET}");
            }
            if let Some(ref seven) = limits.seven_day
                && let Some(pct) = seven.used_percentage
            {
                let color = color_pct(pct);
                let _ = write!(out, " {color}7d:{pct}%{RESET}");
            }
        }
    }

    // Git branch + project dir
    let has_git_or_dir = git_branch.is_some() || input.cwd.is_some();
    if has_git_or_dir {
        let _ = write!(out, " {DIM}|{RESET}");

        if let Some(branch) = git_branch {
            let _ = write!(out, " {CYAN}{branch}{RESET}");
        }

        if let Some(ref cwd) = input.cwd {
            // Show just the last path component
            let dir_name = std::path::Path::new(cwd)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(cwd);
            let _ = write!(out, " {DIM}{dir_name}{RESET}");
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::super::input::{ContextWindow, CostInfo, ModelInfo, RateLimit, RateLimits};
    use super::*;

    #[test]
    fn format_full_status() {
        let input = StatusInput {
            model: Some(ModelInfo {
                display_name: Some("Opus 4.6 (1M context)".to_string()),
                ..Default::default()
            }),
            context_window: Some(ContextWindow {
                used_percentage: Some(6),
                ..Default::default()
            }),
            cost: Some(CostInfo {
                total_cost_usd: Some(2.93),
                total_lines_added: Some(168),
                total_lines_removed: Some(2),
                ..Default::default()
            }),
            rate_limits: Some(RateLimits {
                five_hour: Some(RateLimit {
                    used_percentage: Some(11),
                    ..Default::default()
                }),
                seven_day: Some(RateLimit {
                    used_percentage: Some(85),
                    ..Default::default()
                }),
            }),
            cwd: Some("/home/user/myproject".to_string()),
            ..Default::default()
        };

        let result = format_status(&input, Some("main"));
        assert!(result.contains("Opus 4.6"));
        assert!(result.contains("ctx:6%"));
        assert!(result.contains("$2.93"));
        assert!(result.contains("+168"));
        assert!(result.contains("-2"));
        assert!(result.contains("5h:11%"));
        assert!(result.contains("7d:85%"));
        assert!(result.contains("main"));
        assert!(result.contains("myproject"));
    }

    #[test]
    fn format_empty_input() {
        let input = StatusInput::default();
        let result = format_status(&input, None);
        assert!(result.is_empty());
    }

    #[test]
    fn format_model_only() {
        let input = StatusInput {
            model: Some(ModelInfo {
                display_name: Some("Sonnet 4.5".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let result = format_status(&input, None);
        assert!(result.contains("Sonnet 4.5"));
    }

    #[test]
    fn short_model_strips_parenthetical() {
        assert_eq!(short_model("Opus 4.6 (1M context)"), "Opus 4.6");
        assert_eq!(short_model("Sonnet 4.5"), "Sonnet 4.5");
        assert_eq!(short_model("Haiku"), "Haiku");
    }

    #[test]
    fn context_color_thresholds() {
        assert_eq!(color_pct(0), GREEN);
        assert_eq!(color_pct(59), GREEN);
        assert_eq!(color_pct(60), YELLOW);
        assert_eq!(color_pct(79), YELLOW);
        assert_eq!(color_pct(80), RED);
        assert_eq!(color_pct(100), RED);
    }
}
