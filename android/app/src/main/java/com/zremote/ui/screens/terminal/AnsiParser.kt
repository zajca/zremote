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
                c == '\u001B' -> {
                    i = parseEscape(text, i + 1)
                }
                c == '\n' -> {
                    lines.add(TerminalLine(trimTrailingSpaces(currentLine)))
                    currentLine = mutableListOf()
                    i++
                }
                c == '\r' -> {
                    // Skip carriage return (we don't track cursor position)
                    i++
                }
                c < ' ' && c != '\t' -> {
                    // Skip other control characters (BEL, BS, etc.)
                    i++
                }
                else -> {
                    currentLine.add(StyledChar(c, currentFg, currentBg, bold))
                    i++
                }
            }
        }
        if (currentLine.isNotEmpty()) {
            lines.add(TerminalLine(trimTrailingSpaces(currentLine)))
        }
        return lines
    }

    private fun parseEscape(text: String, start: Int): Int {
        if (start >= text.length) return start
        return when (text[start]) {
            '[' -> parseCsi(text, start + 1)
            ']' -> parseOsc(text, start + 1)
            '(' , ')' , '*' , '+' -> {
                // Charset designation: ESC ( X -- skip next char
                if (start + 1 < text.length) start + 2 else start + 1
            }
            else -> start + 1 // Single-char escape (ESC =, ESC >, etc.)
        }
    }

    // Parse CSI sequence: ESC [ (params) (intermediates) (final byte)
    // Final byte is 0x40..0x7E (@..~)
    // Intermediate bytes are 0x20..0x2F (space../)
    // Parameter bytes are 0x30..0x3F (0..? including digits, semicolons, and ?)
    private fun parseCsi(text: String, start: Int): Int {
        var i = start
        val params = mutableListOf<Int>()
        var current = 0
        var hasParam = false
        var isPrivate = false

        // Check for private mode marker (?)
        if (i < text.length && text[i] == '?') {
            isPrivate = true
            i++
        }

        while (i < text.length) {
            val c = text[i]
            when {
                c in '0'..'9' -> {
                    current = current * 10 + (c - '0')
                    hasParam = true
                    i++
                }
                c == ';' || c == ':' -> {
                    params.add(if (hasParam) current else 0)
                    current = 0
                    hasParam = false
                    i++
                }
                c in ' '..'/' -> {
                    // Intermediate bytes - skip
                    i++
                }
                c in '@'..'~' -> {
                    // Final byte - process if SGR, otherwise ignore
                    if (c == 'm' && !isPrivate) {
                        params.add(if (hasParam) current else 0)
                        applySgr(params)
                    }
                    return i + 1
                }
                else -> {
                    // Malformed sequence, bail out
                    return i + 1
                }
            }
        }
        return i
    }

    // Parse OSC sequence: ESC ] ... (ST or BEL)
    // ST is ESC \ or 0x9C; BEL is 0x07
    private fun parseOsc(text: String, start: Int): Int {
        var i = start
        while (i < text.length) {
            val c = text[i]
            when {
                c == '\u0007' -> return i + 1 // BEL terminates
                c == '\u001B' && i + 1 < text.length && text[i + 1] == '\\' -> return i + 2 // ST
                c == '\u009C'.code.toChar() -> return i + 1 // 8-bit ST
                else -> i++
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
                2 -> {} // dim - ignore
                3 -> {} // italic - ignore
                4 -> {} // underline - ignore
                5, 6 -> {} // blink - ignore
                7 -> { // reverse video
                    val tmp = currentFg
                    currentFg = if (currentBg == Color.Transparent) TERMINAL_BG else currentBg
                    currentBg = tmp
                }
                8 -> {} // hidden - ignore
                9 -> {} // strikethrough - ignore
                22 -> bold = false
                23 -> {} // not italic
                24 -> {} // not underline
                25 -> {} // not blink
                27 -> {} // not reverse
                28 -> {} // not hidden
                29 -> {} // not strikethrough
                in 30..37 -> currentFg = ansi8Color(p - 30, bold)
                38 -> {
                    i = parseExtendedColor(params, i) { currentFg = it }
                }
                39 -> currentFg = DEFAULT_FG
                in 40..47 -> currentBg = ansi8Color(p - 40, false)
                48 -> {
                    i = parseExtendedColor(params, i) { currentBg = it }
                }
                49 -> currentBg = Color.Transparent
                in 90..97 -> currentFg = ansi8Color(p - 90 + 8, false)
                in 100..107 -> currentBg = ansi8Color(p - 100 + 8, false)
            }
            i++
        }
    }

    private inline fun parseExtendedColor(
        params: List<Int>,
        i: Int,
        apply: (Color) -> Unit,
    ): Int {
        if (i + 1 >= params.size) return i
        return when (params[i + 1]) {
            5 -> {
                // 256-color: 38;5;N
                if (i + 2 < params.size) {
                    apply(ansi256Color(params[i + 2]))
                    i + 2
                } else i + 1
            }
            2 -> {
                // RGB: 38;2;R;G;B
                if (i + 4 < params.size) {
                    apply(Color(params[i + 2], params[i + 3], params[i + 4]))
                    i + 4
                } else i + 1
            }
            else -> i + 1
        }
    }

    private fun trimTrailingSpaces(chars: MutableList<StyledChar>): List<StyledChar> {
        var end = chars.size
        while (end > 0 && chars[end - 1].char == ' ' && chars[end - 1].bg == Color.Transparent) {
            end--
        }
        return if (end == chars.size) chars.toList() else chars.subList(0, end).toList()
    }

    companion object {
        val DEFAULT_FG = Color(0xFFE0E0E0)
        val TERMINAL_BG = Color(0xFF1A1A2E)

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
