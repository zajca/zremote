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

pub fn error() -> Rgba {
    rgb(0xef4444)
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
