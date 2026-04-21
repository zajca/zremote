/// ZRemote color palette mapped from web/src/index.css @theme tokens.
use gpui::{Rgba, rgb};

pub fn bg_primary() -> Rgba {
    rgb(0x111113)
}

pub fn bg_secondary() -> Rgba {
    rgb(0x16161a)
}

pub fn bg_tertiary() -> Rgba {
    rgb(0x1a1a1e)
}

pub fn text_primary() -> Rgba {
    rgb(0xeeeeee)
}

pub fn text_secondary() -> Rgba {
    rgb(0x8b8b8b)
}

pub fn text_tertiary() -> Rgba {
    rgb(0x5a5a5a)
}

pub fn accent() -> Rgba {
    rgb(0x5e6ad2)
}

pub fn border() -> Rgba {
    rgb(0x2a2a2e)
}

pub fn success() -> Rgba {
    rgb(0x4ade80)
}

/// Subtle green tint for added-line backgrounds in diff views.
pub fn success_bg() -> Rgba {
    Rgba {
        r: 0.290,
        g: 0.871,
        b: 0.502,
        a: 0.10,
    }
}

pub fn error() -> Rgba {
    rgb(0xef4444)
}

/// Subtle red tint for removed-line backgrounds in diff views.
pub fn error_bg() -> Rgba {
    Rgba {
        r: 0.937,
        g: 0.267,
        b: 0.267,
        a: 0.10,
    }
}

pub fn warning() -> Rgba {
    rgb(0xfbbf24)
}

/// Warning background: warning color at ~20% opacity over dark background.
pub fn warning_bg() -> Rgba {
    Rgba {
        r: 0.984,
        g: 0.749,
        b: 0.141,
        a: 0.08,
    }
}

/// Warning border: warning color at ~27% opacity.
pub fn warning_border() -> Rgba {
    Rgba {
        r: 0.984,
        g: 0.749,
        b: 0.141,
        a: 0.27,
    }
}

pub fn terminal_bg() -> Rgba {
    rgb(0x0a0a0b)
}

pub fn terminal_cursor() -> Rgba {
    rgb(0xcccccc)
}

/// Semi-transparent scrim painted behind full-screen modal surfaces (settings,
/// help, command palette, session switcher). Dark base with ~40% alpha so the
/// backgrounded UI stays visually anchored.
pub fn modal_backdrop() -> Rgba {
    Rgba {
        r: 0.067,
        g: 0.067,
        b: 0.075,
        a: 0.40,
    }
}
