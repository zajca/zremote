package com.zremote.ui.screens.terminal

import androidx.compose.ui.graphics.Color

data class StyledChar(
    val char: Char,
    val fg: Color = Color(0xFFE0E0E0),
    val bg: Color = Color.Transparent,
    val bold: Boolean = false,
)

data class TerminalLine(val chars: List<StyledChar>)

class AnsiParser {
    private var currentFg = DEFAULT_FG
    private var currentBg = Color.Transparent
    private var bold = false

    fun parse(bytes: ByteArray): List<TerminalLine> {
        val text = String(bytes, Charsets.UTF_8)
        val lines = mutableListOf<TerminalLine>()
        var currentLine = mutableListOf<StyledChar>()

        var i = 0
        while (i < text.length) {
            val c = text[i]
            when {
                c == '\u001B' && i + 1 < text.length && text[i + 1] == '[' -> {
                    i = parseEscapeSequence(text, i + 2)
                }
                c == '\n' -> {
                    lines.add(TerminalLine(currentLine.toList()))
                    currentLine = mutableListOf()
                    i++
                }
                c == '\r' -> {
                    i++
                }
                else -> {
                    currentLine.add(StyledChar(c, currentFg, currentBg, bold))
                    i++
                }
            }
        }
        if (currentLine.isNotEmpty()) {
            lines.add(TerminalLine(currentLine.toList()))
        }
        return lines
    }

    private fun parseEscapeSequence(text: String, start: Int): Int {
        var i = start
        val params = mutableListOf<Int>()
        var current = 0
        var hasParam = false

        while (i < text.length) {
            val c = text[i]
            when {
                c in '0'..'9' -> {
                    current = current * 10 + (c - '0')
                    hasParam = true
                    i++
                }
                c == ';' -> {
                    params.add(if (hasParam) current else 0)
                    current = 0
                    hasParam = false
                    i++
                }
                c == 'm' -> {
                    params.add(if (hasParam) current else 0)
                    applySgr(params)
                    return i + 1
                }
                c.isLetter() -> {
                    return i + 1
                }
                else -> {
                    return i + 1
                }
            }
        }
        return i
    }

    private fun applySgr(params: List<Int>) {
        if (params.isEmpty() || (params.size == 1 && params[0] == 0)) {
            currentFg = DEFAULT_FG
            currentBg = Color.Transparent
            bold = false
            return
        }

        var i = 0
        while (i < params.size) {
            when (val p = params[i]) {
                0 -> { currentFg = DEFAULT_FG; currentBg = Color.Transparent; bold = false }
                1 -> bold = true
                22 -> bold = false
                in 30..37 -> currentFg = ansi8Color(p - 30, bold)
                39 -> currentFg = DEFAULT_FG
                in 40..47 -> currentBg = ansi8Color(p - 40, false)
                49 -> currentBg = Color.Transparent
                in 90..97 -> currentFg = ansi8Color(p - 90 + 8, false)
                in 100..107 -> currentBg = ansi8Color(p - 100 + 8, false)
                38 -> {
                    if (i + 1 < params.size && params[i + 1] == 5 && i + 2 < params.size) {
                        currentFg = ansi256Color(params[i + 2])
                        i += 2
                    }
                }
                48 -> {
                    if (i + 1 < params.size && params[i + 1] == 5 && i + 2 < params.size) {
                        currentBg = ansi256Color(params[i + 2])
                        i += 2
                    }
                }
            }
            i++
        }
    }

    companion object {
        val DEFAULT_FG = Color(0xFFE0E0E0)

        private val ANSI_COLORS = arrayOf(
            Color(0xFF000000), // black
            Color(0xFFCD3131), // red
            Color(0xFF0DBC79), // green
            Color(0xFFE5E510), // yellow
            Color(0xFF2472C8), // blue
            Color(0xFFBC3FBC), // magenta
            Color(0xFF11A8CD), // cyan
            Color(0xFFE5E5E5), // white
            Color(0xFF666666), // bright black
            Color(0xFFF14C4C), // bright red
            Color(0xFF23D18B), // bright green
            Color(0xFFF5F543), // bright yellow
            Color(0xFF3B8EEA), // bright blue
            Color(0xFFD670D6), // bright magenta
            Color(0xFF29B8DB), // bright cyan
            Color(0xFFFFFFFF), // bright white
        )

        fun ansi8Color(index: Int, bold: Boolean): Color {
            val effectiveIndex = if (bold && index < 8) index + 8 else index
            return ANSI_COLORS.getOrElse(effectiveIndex) { DEFAULT_FG }
        }

        fun ansi256Color(index: Int): Color {
            return when {
                index < 16 -> ANSI_COLORS.getOrElse(index) { DEFAULT_FG }
                index < 232 -> {
                    val i = index - 16
                    val r = (i / 36) * 51
                    val g = ((i % 36) / 6) * 51
                    val b = (i % 6) * 51
                    Color(r, g, b)
                }
                else -> {
                    val gray = 8 + (index - 232) * 10
                    Color(gray, gray, gray)
                }
            }
        }
    }
}
