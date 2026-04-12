#!/usr/bin/env python3
"""Generate simulated full-screen TUI redraws for benchmarking.

Produces frames resembling htop/top/lazygit with:
- Box drawing borders with colors
- Colored status bars
- Table-like layouts with alternating row colors
- Progress bars
- Full-screen clear and redraw sequences

Each frame is a complete 80x24 screen redraw using cursor positioning.
Output: stdout (redirect to tui-frames.txt)
"""
import random
import sys

RESET = "\033[0m"
CLEAR = "\033[2J"
HOME = "\033[H"

def sgr(*codes):
    return f"\033[{';'.join(str(c) for c in codes)}m"

def cursor_pos(row, col):
    return f"\033[{row};{col}H"

def fg256(idx):
    return f"\033[38;5;{idx}m"

def bg256(idx):
    return f"\033[48;5;{idx}m"

COLS = 120
ROWS = 40

def draw_header(lines):
    """Draw a colored header bar."""
    title = " SYSTEM MONITOR v2.1.0 "
    pad = COLS - len(title)
    left = pad // 2
    right = pad - left
    lines.append(f"{sgr(1)}{bg256(27)}{sgr(37)}{' ' * left}{title}{' ' * right}{RESET}")

def draw_separator(lines, char_set="single"):
    """Draw a horizontal separator."""
    if char_set == "single":
        lines.append(f"{fg256(240)}\u2500" * COLS + RESET)
    elif char_set == "double":
        lines.append(f"{fg256(245)}\u2550" * COLS + RESET)
    else:
        lines.append(f"{fg256(238)}\u2504" * COLS + RESET)

def draw_cpu_bars(lines, num_cpus=8):
    """Draw CPU usage bars like htop."""
    for i in range(num_cpus):
        usage = random.randint(5, 95)
        bar_width = COLS - 12
        filled = int(bar_width * usage / 100)
        empty = bar_width - filled

        # Color gradient: green -> yellow -> red
        if usage < 50:
            bar_color = fg256(46)  # green
        elif usage < 75:
            bar_color = fg256(226)  # yellow
        else:
            bar_color = fg256(196)  # red

        label = f"{fg256(245)}CPU{i:<2}{RESET} "
        bar = f"{bar_color}{'|' * filled}{fg256(238)}{'|' * empty}{RESET}"
        pct = f" {sgr(1)}{usage:>3}%{RESET}"
        lines.append(f"{label}[{bar}]{pct}")

def draw_memory_bar(lines):
    """Draw memory usage bar."""
    used = random.randint(2048, 14336)
    total = 16384
    pct = used * 100 // total
    bar_width = COLS - 20
    filled = int(bar_width * pct / 100)
    empty = bar_width - filled

    label = f"{fg256(245)}Mem  {RESET} "
    bar = f"{fg256(82)}{'|' * filled}{fg256(238)}{'|' * empty}{RESET}"
    info = f" {used}M/{total}M"
    lines.append(f"{label}[{bar}]{info}")

def draw_process_table(lines, num_rows=20):
    """Draw a process table with alternating row colors."""
    header = f"{sgr(1, 7)}{' PID':>7} {'USER':<10} {'CPU%':>6} {'MEM%':>6} {'TIME':>10} {'COMMAND':<60}{' ' * (COLS - 99)}{RESET}"
    lines.append(header)

    processes = [
        ("root", "systemd"), ("root", "kworker/0:1"), ("user", "firefox"),
        ("user", "code"), ("user", "cargo"), ("root", "sshd"),
        ("user", "zremote"), ("user", "alacritty"), ("root", "dockerd"),
        ("user", "node"), ("user", "python3"), ("root", "postgres"),
        ("user", "rust-analyzer"), ("user", "nvim"), ("root", "NetworkManager"),
        ("user", "htop"), ("user", "bash"), ("root", "cupsd"),
        ("user", "slack"), ("user", "spotify"),
    ]

    for i in range(num_rows):
        pid = random.randint(1, 32768)
        user, cmd = random.choice(processes)
        cpu = random.uniform(0, 45.0)
        mem = random.uniform(0.1, 15.0)
        hours = random.randint(0, 99)
        mins = random.randint(0, 59)
        secs = random.randint(0, 59)
        time_str = f"{hours}:{mins:02d}:{secs:02d}"

        bg = bg256(235) if i % 2 == 0 else bg256(233)
        cpu_color = fg256(196) if cpu > 20 else fg256(226) if cpu > 5 else fg256(255)
        mem_color = fg256(196) if mem > 10 else fg256(226) if mem > 5 else fg256(255)

        row = f"{bg}{pid:>7} {user:<10} {cpu_color}{cpu:>6.1f}{RESET}{bg} {mem_color}{mem:>6.1f}{RESET}{bg} {time_str:>10} {cmd:<60}"
        # Pad to COLS
        visible_len = 7 + 1 + 10 + 1 + 6 + 1 + 6 + 1 + 10 + 1 + min(len(cmd), 60)
        padding = max(0, COLS - visible_len)
        lines.append(f"{row}{' ' * padding}{RESET}")

def draw_status_bar(lines):
    """Draw a bottom status bar."""
    left = " F1:Help  F2:Setup  F3:Search  F5:Tree  F6:Sort  F9:Kill  F10:Quit"
    pad = COLS - len(left)
    lines.append(f"{sgr(7)}{left}{' ' * pad}{RESET}")

def draw_box(lines, title, content_lines, width=None):
    """Draw a box with title and content."""
    w = width or COLS
    inner_w = w - 2
    # Top border
    lines.append(f"{fg256(245)}\u250c\u2500 {sgr(1)}{title}{RESET}{fg256(245)} {'\u2500' * (inner_w - len(title) - 3)}\u2510{RESET}")
    for line in content_lines:
        visible = len(line.replace(RESET, ""))  # rough approximation
        pad = max(0, inner_w - len(line) + (len(line) - visible))
        lines.append(f"{fg256(245)}\u2502{RESET}{line}{' ' * max(0, inner_w - visible)}{fg256(245)}\u2502{RESET}")
    # Bottom border
    lines.append(f"{fg256(245)}\u2514{'\u2500' * inner_w}\u2518{RESET}")

def generate_frame():
    """Generate a single full-screen frame."""
    lines = []
    lines.append(CLEAR + HOME)
    draw_header(lines)
    draw_separator(lines, "single")
    draw_cpu_bars(lines, num_cpus=random.choice([4, 8, 12]))
    draw_memory_bar(lines)
    draw_separator(lines, "double")
    draw_process_table(lines, num_rows=random.randint(15, 25))
    draw_separator(lines, "dotted")
    draw_status_bar(lines)
    return "\n".join(lines)

def main():
    # Generate 100 frames (simulating ~3 seconds of TUI updates at 30fps)
    for frame_num in range(100):
        sys.stdout.write(generate_frame())
        sys.stdout.write("\n")

if __name__ == "__main__":
    random.seed(42)
    main()
