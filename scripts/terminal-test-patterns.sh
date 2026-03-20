#!/usr/bin/env bash
# Terminal test patterns for visual quality testing
# Outputs ANSI colors, Unicode, text styles, and alignment grids

set -euo pipefail

echo "=== ANSI Standard Colors (FG on default BG) ==="
for i in {30..37}; do
    printf "\e[${i}m %-8s \e[0m" "Color$((i-30))"
done
echo ""
for i in {90..97}; do
    printf "\e[${i}m %-8s \e[0m" "Bright$((i-90))"
done
echo ""
echo ""

echo "=== ANSI Standard Colors (BG) ==="
for i in {40..47}; do
    printf "\e[${i}m  %3d  \e[0m" "$i"
done
echo ""
for i in {100..107}; do
    printf "\e[${i}m  %3d  \e[0m" "$i"
done
echo ""
echo ""

echo "=== 256 Color Palette ==="
# Standard colors (0-15)
for i in $(seq 0 15); do
    printf "\e[48;5;%dm  \e[0m" "$i"
done
echo ""
# Color cube (16-231) - show a subset
for row in $(seq 0 5); do
    for col in $(seq 0 35); do
        idx=$((16 + row * 36 + col))
        printf "\e[48;5;%dm \e[0m" "$idx"
    done
    echo ""
done
# Grayscale (232-255)
for i in $(seq 232 255); do
    printf "\e[48;5;%dm \e[0m" "$i"
done
echo ""
echo ""

echo "=== Text Styles ==="
printf "\e[1m%-20s\e[0m" "Bold"
printf "\e[2m%-20s\e[0m" "Dim"
printf "\e[3m%-20s\e[0m" "Italic"
printf "\e[4m%-20s\e[0m" "Underline"
echo ""
printf "\e[7m%-20s\e[0m" "Reverse"
printf "\e[9m%-20s\e[0m" "Strikethrough"
printf "\e[1;3m%-20s\e[0m" "Bold+Italic"
printf "\e[1;4m%-20s\e[0m" "Bold+Underline"
echo ""
echo ""

echo "=== Unicode Box Drawing ==="
echo "┌──────────┬──────────┬──────────┐"
echo "│  Cell 1  │  Cell 2  │  Cell 3  │"
echo "├──────────┼──────────┼──────────┤"
echo "│  Cell 4  │  Cell 5  │  Cell 6  │"
echo "└──────────┴──────────┴──────────┘"
echo ""
echo "╔══════════╦══════════╦══════════╗"
echo "║  Double  ║  Border  ║  Style   ║"
echo "╠══════════╬══════════╬══════════╣"
echo "║  Cell A  ║  Cell B  ║  Cell C  ║"
echo "╚══════════╩══════════╩══════════╝"
echo ""

echo "=== Unicode Symbols ==="
echo "Arrows:   ← → ↑ ↓ ↔ ↕ ⇐ ⇒ ⇑ ⇓"
echo "Math:     ± × ÷ ≠ ≤ ≥ ∞ √ ∑ ∏"
echo "Misc:     ● ○ ■ □ ▲ △ ♠ ♣ ♥ ♦"
echo "Braille:  ⠁ ⠃ ⠇ ⠏ ⠟ ⠿ ⡿ ⣿"
echo "Blocks:   ░ ▒ ▓ █ ▀ ▄ ▌ ▐"
echo ""

echo "=== Block Elements (Smooth Gradient) ==="
printf "  "
for i in $(seq 232 255); do
    printf "\e[48;5;%dm \e[0m" "$i"
done
echo ""
printf "  ░░░░▒▒▒▒▓▓▓▓████"
echo ""
echo ""

echo "=== Alignment Grid ==="
echo "0123456789012345678901234567890123456789012345678901234567890123456789012345678"
echo "         1111111111222222222233333333334444444444555555555566666666667777777777"
echo ""

echo "=== Monospace Verification ==="
echo "MMMMMMMMMM"
echo "iiiiiiiiii"
echo "WWWWWWWWWW"
echo "1234567890"
echo "||||||||||"
echo "All lines above should be exactly the same width."
echo ""

echo "=== Colored Text on Colored Background ==="
for bg in {40..47}; do
    for fg in {30..37}; do
        printf "\e[${fg};${bg}m X \e[0m"
    done
    echo ""
done
echo ""

echo "=== True Color (24-bit) Gradient ==="
# Red gradient
printf "R: "
for i in $(seq 0 4 255); do
    printf "\e[48;2;%d;0;0m \e[0m" "$i"
done
echo ""
# Green gradient
printf "G: "
for i in $(seq 0 4 255); do
    printf "\e[48;2;0;%d;0m \e[0m" "$i"
done
echo ""
# Blue gradient
printf "B: "
for i in $(seq 0 4 255); do
    printf "\e[48;2;0;0;%dm \e[0m" "$i"
done
echo ""
echo ""

echo "=== Test Complete ==="
