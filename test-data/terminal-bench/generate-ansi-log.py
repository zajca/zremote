#!/usr/bin/env python3
"""Generate synthetic ANSI terminal output for benchmarking.

Produces ~10K lines with a realistic mix of:
- Plain text with various ANSI 16-color, 256-color, and true-color sequences
- Long lines (up to 200+ columns)
- Unicode characters (CJK, emoji, box drawing)
- Bold, italic, underline, dim, strikethrough, reverse video
- Interspersed blank lines and whitespace-heavy lines

Output: stdout (redirect to ansi-log.txt)
"""
import random
import sys

RESET = "\033[0m"

def sgr(*codes):
    return f"\033[{';'.join(str(c) for c in codes)}m"

def fg256(idx):
    return f"\033[38;5;{idx}m"

def bg256(idx):
    return f"\033[48;5;{idx}m"

def fg_rgb(r, g, b):
    return f"\033[38;2;{r};{g};{b}m"

def bg_rgb(r, g, b):
    return f"\033[48;2;{r};{g};{b}m"

# Sample text fragments for realistic log-like content
LOG_LEVELS = [
    (sgr(1, 32), "INFO"),
    (sgr(1, 33), "WARN"),
    (sgr(1, 31), "ERROR"),
    (sgr(2), "DEBUG"),
    (sgr(36), "TRACE"),
]

PATHS = [
    "src/main.rs", "crates/core/lib.rs", "server/routes/api.rs",
    "src/views/terminal.rs", "tests/integration.rs", "config/settings.toml",
]

MESSAGES = [
    "Request processed successfully",
    "Connection established to remote host",
    "Cache miss for key: user_session_abc123",
    "Database query completed in 12ms",
    "WebSocket handshake completed",
    "Spawning background task for cleanup",
    "Configuration reloaded from disk",
    "TLS certificate verification passed",
    "Rate limit threshold approaching: 450/500",
    "Metrics batch flushed: 1024 points",
]

UNICODE_SAMPLES = [
    "Hello World \u2500\u2500\u2500 \u250c\u2500\u2500\u2500\u2510",
    "\u2588\u2588\u2588 \u2591\u2591\u2591 \u2592\u2592\u2592 \u2593\u2593\u2593",
    "Status: \u2714 OK  \u2718 Failed  \u26a0 Warning",
    "\u256d\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u256e",
    "\u2502 Progress   \u2502",
    "\u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u256f",
    "\u4f60\u597d\u4e16\u754c CJK text mixed with ASCII",
    "\u2500\u2500\u2500\u253c\u2500\u2500\u2500\u253c\u2500\u2500\u2500",
]

def emit_log_line():
    """Emit a structured log line with ANSI colors."""
    color, level = random.choice(LOG_LEVELS)
    ts = f"2025-01-{random.randint(1,28):02d}T{random.randint(0,23):02d}:{random.randint(0,59):02d}:{random.randint(0,59):02d}.{random.randint(0,999):03d}Z"
    path = random.choice(PATHS)
    msg = random.choice(MESSAGES)
    return f"{sgr(2)}{ts}{RESET} {color}{level:>5}{RESET} {sgr(34)}{path}{RESET}: {msg}"

def emit_256_color_line():
    """Emit a line using 256-color palette."""
    parts = []
    for i in range(0, 80, 4):
        idx = random.randint(16, 231)
        parts.append(f"{fg256(idx)}\u2588\u2588\u2588\u2588")
    return "".join(parts) + RESET

def emit_truecolor_gradient():
    """Emit a smooth gradient using 24-bit RGB."""
    parts = []
    for i in range(80):
        r = int(255 * i / 80)
        g = int(255 * (80 - i) / 80)
        b = 128
        parts.append(f"{fg_rgb(r, g, b)}\u2588")
    return "".join(parts) + RESET

def emit_styled_text():
    """Emit text with various style attributes."""
    styles = [
        (sgr(1), "bold"),
        (sgr(3), "italic"),
        (sgr(4), "underline"),
        (sgr(2), "dim"),
        (sgr(9), "strikethrough"),
        (sgr(7), "reverse"),
        (sgr(1, 3), "bold+italic"),
        (sgr(1, 4), "bold+underline"),
        (sgr(2, 3), "dim+italic"),
    ]
    parts = []
    for style_code, label in styles:
        parts.append(f"{style_code}{label}{RESET}")
    return "  ".join(parts)

def emit_long_line():
    """Emit a line exceeding 200 columns."""
    base = f"{sgr(33)}{'=' * 200}{RESET}"
    return f"LONG: {base} END"

def emit_unicode_line():
    return random.choice(UNICODE_SAMPLES)

def emit_whitespace_line():
    """Emit a line that is mostly whitespace with a few colored tokens."""
    indent = " " * random.randint(4, 40)
    token = f"{fg256(random.randint(16, 231))}token_{random.randint(1,999)}{RESET}"
    return f"{indent}{token}"

def emit_blank_line():
    return ""

def main():
    generators = [
        (emit_log_line, 50),
        (emit_256_color_line, 8),
        (emit_truecolor_gradient, 5),
        (emit_styled_text, 8),
        (emit_long_line, 3),
        (emit_unicode_line, 10),
        (emit_whitespace_line, 10),
        (emit_blank_line, 6),
    ]

    total_weight = sum(w for _, w in generators)
    choices = []
    for gen, weight in generators:
        choices.extend([gen] * weight)

    for _ in range(10000):
        gen = random.choice(choices)
        print(gen())

if __name__ == "__main__":
    random.seed(42)  # Reproducible output
    main()
