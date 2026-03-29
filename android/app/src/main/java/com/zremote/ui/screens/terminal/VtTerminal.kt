package com.zremote.ui.screens.terminal

import androidx.compose.ui.graphics.Color
import com.zremote.ui.screens.terminal.AnsiParser.Companion.DEFAULT_FG
import com.zremote.ui.screens.terminal.AnsiParser.Companion.TERMINAL_BG
import com.zremote.ui.screens.terminal.AnsiParser.Companion.ansi256Color
import com.zremote.ui.screens.terminal.AnsiParser.Companion.ansi8Color

data class Cell(
    var char: Char = ' ',
    var fg: Color = DEFAULT_FG,
    var bg: Color = Color.Transparent,
    var bold: Boolean = false,
    var dim: Boolean = false,
    var italic: Boolean = false,
    var underline: Boolean = false,
    var reverse: Boolean = false,
) {
    fun reset() {
        char = ' '
        fg = DEFAULT_FG
        bg = Color.Transparent
        bold = false
        dim = false
        italic = false
        underline = false
        reverse = false
    }

    fun copyFrom(other: Cell) {
        char = other.char
        fg = other.fg
        bg = other.bg
        bold = other.bold
        dim = other.dim
        italic = other.italic
        underline = other.underline
        reverse = other.reverse
    }
}

private enum class ParseState {
    Normal,
    Escape,
    Csi,
    Osc,
    CharsetDesignation,
    DcsPassthrough,
}

class VtTerminal(
    private var cols: Int = 80,
    private var rows: Int = 24,
) {
    private var cells: Array<Array<Cell>> = makeGrid(cols, rows)
    private val scrollback: MutableList<Array<Cell>> = mutableListOf()

    var cursorRow: Int = 0
        private set
    var cursorCol: Int = 0
        private set

    private var savedCursorRow: Int = 0
    private var savedCursorCol: Int = 0
    private var savedSgrState: SgrState = SgrState()

    private var scrollTop: Int = 0
    private var scrollBottom: Int = rows - 1

    private var altScreen: Array<Array<Cell>>? = null
    private var altCursorRow: Int = 0
    private var altCursorCol: Int = 0

    private var sgrState: SgrState = SgrState()
    private var parseState: ParseState = ParseState.Normal
    private var autoWrap: Boolean = true
    private var wrapPending: Boolean = false

    // CSI parser state
    private val csiParams = mutableListOf<Int>()
    private var csiCurrentParam: Int = 0
    private var csiHasParam: Boolean = false
    private var csiPrivateMarker: Char = '\u0000'
    private var csiIntermediates = StringBuilder()

    // OSC parser state
    private val oscBuffer = StringBuilder()
    private var oscPrevWasEsc = false

    // UTF-8 decoder state
    private var utf8Remaining = 0
    private var utf8Codepoint = 0

    companion object {
        private const val MAX_SCROLLBACK = 1000
        private const val TAB_WIDTH = 8
    }

    data class SgrState(
        var fg: Color = DEFAULT_FG,
        var bg: Color = Color.Transparent,
        var bold: Boolean = false,
        var dim: Boolean = false,
        var italic: Boolean = false,
        var underline: Boolean = false,
        var reverse: Boolean = false,
    ) {
        fun reset() {
            fg = DEFAULT_FG
            bg = Color.Transparent
            bold = false
            dim = false
            italic = false
            underline = false
            reverse = false
        }

        fun copyFrom(other: SgrState) {
            fg = other.fg
            bg = other.bg
            bold = other.bold
            dim = other.dim
            italic = other.italic
            underline = other.underline
            reverse = other.reverse
        }
    }

    @Synchronized
    fun write(data: ByteArray) {
        for (b in data) {
            val byte = b.toInt() and 0xFF
            if (utf8Remaining > 0) {
                if (byte and 0xC0 == 0x80) {
                    utf8Codepoint = (utf8Codepoint shl 6) or (byte and 0x3F)
                    utf8Remaining--
                    if (utf8Remaining == 0) {
                        processChar(utf8Codepoint.toChar())
                    }
                } else {
                    // Invalid continuation byte, reset and reprocess
                    utf8Remaining = 0
                    processChar('\uFFFD')
                    processByte(byte)
                }
            } else {
                processByte(byte)
            }
        }
    }

    private fun processByte(byte: Int) {
        when {
            byte and 0x80 == 0 -> processChar(byte.toChar())
            byte and 0xE0 == 0xC0 -> { utf8Remaining = 1; utf8Codepoint = byte and 0x1F }
            byte and 0xF0 == 0xE0 -> { utf8Remaining = 2; utf8Codepoint = byte and 0x0F }
            byte and 0xF8 == 0xF0 -> { utf8Remaining = 3; utf8Codepoint = byte and 0x07 }
            else -> processChar('\uFFFD')
        }
    }

    private fun processChar(c: Char) {
        when (parseState) {
            ParseState.Normal -> processNormal(c)
            ParseState.Escape -> processEscape(c)
            ParseState.Csi -> processCsi(c)
            ParseState.Osc -> processOsc(c)
            ParseState.CharsetDesignation -> {
                // Consume the designation byte and return to normal
                parseState = ParseState.Normal
            }
            ParseState.DcsPassthrough -> processDcs(c)
        }
    }

    private fun processNormal(c: Char) {
        when {
            c == '\u001B' -> parseState = ParseState.Escape
            c == '\n' -> lineFeed()
            c == '\r' -> { cursorCol = 0; wrapPending = false }
            c == '\b' -> { if (cursorCol > 0) cursorCol--; wrapPending = false }
            c == '\t' -> {
                val nextTab = ((cursorCol / TAB_WIDTH) + 1) * TAB_WIDTH
                cursorCol = nextTab.coerceAtMost(cols - 1)
                wrapPending = false
            }
            c == '\u0007' -> { /* BEL - ignore */ }
            c == '\u000E' || c == '\u000F' -> { /* Shift In/Out - ignore */ }
            c < ' ' -> { /* Other control chars - ignore */ }
            else -> putChar(c)
        }
    }

    private fun processEscape(c: Char) {
        parseState = ParseState.Normal
        when (c) {
            '[' -> {
                parseState = ParseState.Csi
                csiParams.clear()
                csiCurrentParam = 0
                csiHasParam = false
                csiPrivateMarker = '\u0000'
                csiIntermediates.clear()
            }
            ']' -> {
                parseState = ParseState.Osc
                oscBuffer.clear()
                oscPrevWasEsc = false
            }
            '(', ')', '*', '+' -> {
                parseState = ParseState.CharsetDesignation
            }
            'c' -> reset() // RIS - Full reset
            'D' -> index() // Index (scroll up)
            'M' -> reverseIndex() // Reverse index (scroll down)
            'E' -> { // Next line
                cursorCol = 0
                index()
            }
            '7' -> saveCursor() // DECSC
            '8' -> restoreCursor() // DECRC
            '=' , '>' -> { /* Keypad mode - ignore */ }
            'P' -> {
                parseState = ParseState.DcsPassthrough
            }
            '\\' -> { /* ST (String Terminator) - ignore */ }
            else -> { /* Unknown escape - ignore */ }
        }
    }

    private fun processCsi(c: Char) {
        when {
            c in '0'..'9' -> {
                csiCurrentParam = csiCurrentParam * 10 + (c - '0')
                csiHasParam = true
            }
            c == ';' || c == ':' -> {
                csiParams.add(if (csiHasParam) csiCurrentParam else 0)
                csiCurrentParam = 0
                csiHasParam = false
            }
            c == '?' || c == '>' || c == '!' -> {
                if (csiParams.isEmpty() && !csiHasParam) {
                    csiPrivateMarker = c
                } else {
                    // Ignore invalid position for private marker
                }
            }
            c in ' '..'/' -> {
                csiIntermediates.append(c)
            }
            c in '@'..'~' -> {
                // Final byte
                csiParams.add(if (csiHasParam) csiCurrentParam else 0)
                executeCsi(c)
                parseState = ParseState.Normal
            }
            else -> {
                // Malformed, bail
                parseState = ParseState.Normal
            }
        }
    }

    private fun processOsc(c: Char) {
        when {
            c == '\u0007' -> {
                // BEL terminates OSC
                parseState = ParseState.Normal
            }
            c == '\\' && oscPrevWasEsc -> {
                // ESC \ (ST) terminates OSC
                parseState = ParseState.Normal
            }
            c == '\u001B' -> {
                oscPrevWasEsc = true
                return
            }
            else -> {
                if (oscPrevWasEsc) {
                    // ESC followed by something other than \, treat as new escape
                    oscPrevWasEsc = false
                    parseState = ParseState.Escape
                    processEscape(c)
                    return
                }
                oscBuffer.append(c)
            }
        }
        oscPrevWasEsc = false
    }

    private fun processDcs(c: Char) {
        // DCS passthrough: consume until ST (ESC \) or BEL
        when {
            c == '\u0007' -> parseState = ParseState.Normal
            c == '\\' && oscPrevWasEsc -> parseState = ParseState.Normal
            c == '\u001B' -> oscPrevWasEsc = true
            else -> oscPrevWasEsc = false
        }
    }

    private fun executeCsi(finalByte: Char) {
        if (csiPrivateMarker == '?') {
            executePrivateMode(finalByte)
            return
        }
        if (csiPrivateMarker != '\u0000') {
            // Other private markers (>, !) - ignore
            return
        }

        val p = csiParams
        when (finalByte) {
            'm' -> applySgr(p)
            'A' -> { // Cursor up
                val n = maxOf(p.getOrDefault(0, 1), 1)
                cursorRow = (cursorRow - n).coerceAtLeast(scrollTop)
                wrapPending = false
            }
            'B' -> { // Cursor down
                val n = maxOf(p.getOrDefault(0, 1), 1)
                cursorRow = (cursorRow + n).coerceAtMost(scrollBottom)
                wrapPending = false
            }
            'C' -> { // Cursor forward
                val n = maxOf(p.getOrDefault(0, 1), 1)
                cursorCol = (cursorCol + n).coerceAtMost(cols - 1)
                wrapPending = false
            }
            'D' -> { // Cursor backward
                val n = maxOf(p.getOrDefault(0, 1), 1)
                cursorCol = (cursorCol - n).coerceAtLeast(0)
                wrapPending = false
            }
            'H', 'f' -> { // Cursor position
                cursorRow = (maxOf(p.getOrDefault(0, 1), 1) - 1).coerceIn(0, rows - 1)
                cursorCol = (maxOf(p.getOrDefault(1, 1), 1) - 1).coerceIn(0, cols - 1)
                wrapPending = false
            }
            'G' -> { // Cursor horizontal absolute
                cursorCol = (maxOf(p.getOrDefault(0, 1), 1) - 1).coerceIn(0, cols - 1)
                wrapPending = false
            }
            'd' -> { // Cursor vertical absolute
                cursorRow = (maxOf(p.getOrDefault(0, 1), 1) - 1).coerceIn(0, rows - 1)
                wrapPending = false
            }
            'J' -> eraseInDisplay(p.getOrDefault(0, 0))
            'K' -> eraseInLine(p.getOrDefault(0, 0))
            'X' -> { // Erase characters
                val n = maxOf(p.getOrDefault(0, 1), 1)
                val end = (cursorCol + n).coerceAtMost(cols)
                for (col in cursorCol until end) {
                    cells[cursorRow][col].reset()
                }
            }
            'L' -> { // Insert lines
                val n = maxOf(p.getOrDefault(0, 1), 1)
                insertLines(n)
            }
            'M' -> { // Delete lines
                val n = maxOf(p.getOrDefault(0, 1), 1)
                deleteLines(n)
            }
            '@' -> { // Insert characters
                val n = maxOf(p.getOrDefault(0, 1), 1)
                insertChars(n)
            }
            'P' -> { // Delete characters
                val n = maxOf(p.getOrDefault(0, 1), 1)
                deleteChars(n)
            }
            'S' -> { // Scroll up
                val n = maxOf(p.getOrDefault(0, 1), 1)
                repeat(n) { scrollUp() }
            }
            'T' -> { // Scroll down
                val n = maxOf(p.getOrDefault(0, 1), 1)
                repeat(n) { scrollDown() }
            }
            'r' -> { // Set scroll region (DECSTBM)
                val top = (maxOf(p.getOrDefault(0, 1), 1) - 1).coerceIn(0, rows - 1)
                val bottom = (maxOf(p.getOrDefault(1, rows), 1) - 1).coerceIn(0, rows - 1)
                if (top < bottom) {
                    scrollTop = top
                    scrollBottom = bottom
                }
                cursorRow = 0
                cursorCol = 0
                wrapPending = false
            }
            's' -> saveCursor()
            'u' -> restoreCursor()
            'n' -> { /* Device status report - ignore */ }
            'h' -> { // Set mode (non-private)
                // Handle auto-wrap: SM mode 7
                if (p.getOrDefault(0, 0) == 7) autoWrap = true
            }
            'l' -> { // Reset mode (non-private)
                if (p.getOrDefault(0, 0) == 7) autoWrap = false
            }
            'E' -> { // Cursor next line
                val n = maxOf(p.getOrDefault(0, 1), 1)
                cursorRow = (cursorRow + n).coerceAtMost(scrollBottom)
                cursorCol = 0
                wrapPending = false
            }
            'F' -> { // Cursor previous line
                val n = maxOf(p.getOrDefault(0, 1), 1)
                cursorRow = (cursorRow - n).coerceAtLeast(scrollTop)
                cursorCol = 0
                wrapPending = false
            }
        }
    }

    private fun executePrivateMode(finalByte: Char) {
        val p = csiParams
        when (finalByte) {
            'h' -> { // Set private mode
                for (mode in p) {
                    when (mode) {
                        1049 -> enterAltScreen()
                        47, 1047 -> enterAltScreen()
                        7 -> autoWrap = true
                        // 25 (cursor visible), 2004 (bracketed paste), etc. - ignore
                    }
                }
            }
            'l' -> { // Reset private mode
                for (mode in p) {
                    when (mode) {
                        1049 -> leaveAltScreen()
                        47, 1047 -> leaveAltScreen()
                        7 -> autoWrap = false
                    }
                }
            }
        }
    }

    private fun putChar(c: Char) {
        if (wrapPending && autoWrap) {
            cursorCol = 0
            index()
            wrapPending = false
        }

        if (cursorRow in 0 until rows && cursorCol in 0 until cols) {
            val cell = cells[cursorRow][cursorCol]
            cell.char = c
            cell.fg = sgrState.fg
            cell.bg = sgrState.bg
            cell.bold = sgrState.bold
            cell.dim = sgrState.dim
            cell.italic = sgrState.italic
            cell.underline = sgrState.underline
            cell.reverse = sgrState.reverse
        }

        if (cursorCol >= cols - 1) {
            wrapPending = true
        } else {
            cursorCol++
        }
    }

    private fun lineFeed() {
        wrapPending = false
        if (cursorRow == scrollBottom) {
            scrollUp()
        } else if (cursorRow < rows - 1) {
            cursorRow++
        }
    }

    private fun index() {
        if (cursorRow == scrollBottom) {
            scrollUp()
        } else if (cursorRow < rows - 1) {
            cursorRow++
        }
    }

    private fun reverseIndex() {
        if (cursorRow == scrollTop) {
            scrollDown()
        } else if (cursorRow > 0) {
            cursorRow--
        }
    }

    private fun scrollUp() {
        // Save the top line to scrollback (only if in main screen)
        if (altScreen == null && scrollTop == 0) {
            scrollback.add(cells[0].map { Cell().apply { copyFrom(it) } }.toTypedArray())
            if (scrollback.size > MAX_SCROLLBACK) {
                scrollback.removeAt(0)
            }
        }

        // Shift lines up within scroll region
        for (row in scrollTop until scrollBottom) {
            val src = cells[row + 1]
            val dst = cells[row]
            for (col in 0 until cols) {
                dst[col].copyFrom(src[col])
            }
        }

        // Clear the bottom line of scroll region
        for (col in 0 until cols) {
            cells[scrollBottom][col].reset()
        }
    }

    private fun scrollDown() {
        // Shift lines down within scroll region
        for (row in scrollBottom downTo scrollTop + 1) {
            val src = cells[row - 1]
            val dst = cells[row]
            for (col in 0 until cols) {
                dst[col].copyFrom(src[col])
            }
        }

        // Clear the top line of scroll region
        for (col in 0 until cols) {
            cells[scrollTop][col].reset()
        }
    }

    private fun insertLines(n: Int) {
        if (cursorRow < scrollTop || cursorRow > scrollBottom) return
        val count = n.coerceAtMost(scrollBottom - cursorRow + 1)
        // Shift lines down
        for (row in scrollBottom downTo cursorRow + count) {
            for (col in 0 until cols) {
                cells[row][col].copyFrom(cells[row - count][col])
            }
        }
        // Clear inserted lines
        for (row in cursorRow until (cursorRow + count).coerceAtMost(scrollBottom + 1)) {
            for (col in 0 until cols) {
                cells[row][col].reset()
            }
        }
    }

    private fun deleteLines(n: Int) {
        if (cursorRow < scrollTop || cursorRow > scrollBottom) return
        val count = n.coerceAtMost(scrollBottom - cursorRow + 1)
        // Shift lines up
        for (row in cursorRow..scrollBottom - count) {
            for (col in 0 until cols) {
                cells[row][col].copyFrom(cells[row + count][col])
            }
        }
        // Clear bottom lines
        for (row in (scrollBottom - count + 1).coerceAtLeast(cursorRow)..scrollBottom) {
            for (col in 0 until cols) {
                cells[row][col].reset()
            }
        }
    }

    private fun insertChars(n: Int) {
        val count = n.coerceAtMost(cols - cursorCol)
        // Shift characters right
        for (col in cols - 1 downTo cursorCol + count) {
            cells[cursorRow][col].copyFrom(cells[cursorRow][col - count])
        }
        // Clear inserted positions
        for (col in cursorCol until (cursorCol + count).coerceAtMost(cols)) {
            cells[cursorRow][col].reset()
        }
    }

    private fun deleteChars(n: Int) {
        val count = n.coerceAtMost(cols - cursorCol)
        // Shift characters left
        for (col in cursorCol until cols - count) {
            cells[cursorRow][col].copyFrom(cells[cursorRow][col + count])
        }
        // Clear end positions
        for (col in (cols - count).coerceAtLeast(cursorCol) until cols) {
            cells[cursorRow][col].reset()
        }
    }

    private fun eraseInDisplay(mode: Int) {
        when (mode) {
            0 -> { // Erase below (including cursor position)
                eraseInLine(0)
                for (row in cursorRow + 1 until rows) {
                    for (col in 0 until cols) {
                        cells[row][col].reset()
                    }
                }
            }
            1 -> { // Erase above (including cursor position)
                for (row in 0 until cursorRow) {
                    for (col in 0 until cols) {
                        cells[row][col].reset()
                    }
                }
                eraseInLine(1)
            }
            2 -> { // Erase all
                for (row in 0 until rows) {
                    for (col in 0 until cols) {
                        cells[row][col].reset()
                    }
                }
            }
            3 -> { // Erase all + scrollback
                scrollback.clear()
                for (row in 0 until rows) {
                    for (col in 0 until cols) {
                        cells[row][col].reset()
                    }
                }
            }
        }
    }

    private fun eraseInLine(mode: Int) {
        when (mode) {
            0 -> { // Erase right (including cursor position)
                for (col in cursorCol until cols) {
                    cells[cursorRow][col].reset()
                }
            }
            1 -> { // Erase left (including cursor position)
                for (col in 0..cursorCol.coerceAtMost(cols - 1)) {
                    cells[cursorRow][col].reset()
                }
            }
            2 -> { // Erase whole line
                for (col in 0 until cols) {
                    cells[cursorRow][col].reset()
                }
            }
        }
    }

    private fun enterAltScreen() {
        if (altScreen != null) return
        // Save current screen and cursor
        altCursorRow = cursorRow
        altCursorCol = cursorCol
        altScreen = cells
        // Create fresh screen
        cells = makeGrid(cols, rows)
        cursorRow = 0
        cursorCol = 0
        wrapPending = false
    }

    private fun leaveAltScreen() {
        val saved = altScreen ?: return
        cells = saved
        altScreen = null
        cursorRow = altCursorRow
        cursorCol = altCursorCol
        wrapPending = false
    }

    private fun saveCursor() {
        savedCursorRow = cursorRow
        savedCursorCol = cursorCol
        savedSgrState = SgrState().apply { copyFrom(sgrState) }
    }

    private fun restoreCursor() {
        cursorRow = savedCursorRow.coerceIn(0, rows - 1)
        cursorCol = savedCursorCol.coerceIn(0, cols - 1)
        sgrState.copyFrom(savedSgrState)
        wrapPending = false
    }

    private fun applySgr(params: List<Int>) {
        if (params.isEmpty() || (params.size == 1 && params[0] == 0)) {
            sgrState.reset()
            return
        }

        var i = 0
        while (i < params.size) {
            when (val p = params[i]) {
                0 -> sgrState.reset()
                1 -> sgrState.bold = true
                2 -> sgrState.dim = true
                3 -> sgrState.italic = true
                4 -> sgrState.underline = true
                5, 6 -> { /* blink - ignore */ }
                7 -> sgrState.reverse = true
                8 -> { /* hidden - ignore */ }
                9 -> { /* strikethrough - ignore */ }
                22 -> { sgrState.bold = false; sgrState.dim = false }
                23 -> sgrState.italic = false
                24 -> sgrState.underline = false
                25 -> { /* not blink */ }
                27 -> sgrState.reverse = false
                28 -> { /* not hidden */ }
                29 -> { /* not strikethrough */ }
                in 30..37 -> sgrState.fg = ansi8Color(p - 30, sgrState.bold)
                38 -> { i = parseExtendedColor(params, i) { sgrState.fg = it } }
                39 -> sgrState.fg = DEFAULT_FG
                in 40..47 -> sgrState.bg = ansi8Color(p - 40, false)
                48 -> { i = parseExtendedColor(params, i) { sgrState.bg = it } }
                49 -> sgrState.bg = Color.Transparent
                in 90..97 -> sgrState.fg = ansi8Color(p - 90 + 8, false)
                in 100..107 -> sgrState.bg = ansi8Color(p - 100 + 8, false)
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
                if (i + 2 < params.size) {
                    apply(ansi256Color(params[i + 2]))
                    i + 2
                } else i + 1
            }
            2 -> {
                if (i + 4 < params.size) {
                    apply(Color(params[i + 2], params[i + 3], params[i + 4]))
                    i + 4
                } else i + 1
            }
            else -> i + 1
        }
    }

    @Synchronized
    fun getDisplayLines(): List<TerminalLine> {
        val result = mutableListOf<TerminalLine>()

        // Add scrollback lines (skip trailing empty ones)
        for (row in scrollback) {
            val line = cellRowToTerminalLine(row)
            if (line.chars.isNotEmpty()) {
                result.add(line)
            }
        }

        // Add visible grid lines (always include all rows to preserve layout)
        for (row in 0 until rows) {
            result.add(cellRowToTerminalLine(cells[row]))
        }

        return result
    }

    private fun cellRowToTerminalLine(row: Array<Cell>): TerminalLine {
        val chars = mutableListOf<StyledChar>()
        for (cell in row) {
            val fg: Color
            val bg: Color
            if (cell.reverse) {
                fg = if (cell.bg == Color.Transparent) TERMINAL_BG else cell.bg
                bg = cell.fg
            } else {
                fg = cell.fg
                bg = cell.bg
            }
            chars.add(
                StyledChar(
                    char = cell.char,
                    fg = fg,
                    bg = bg,
                    bold = cell.bold,
                )
            )
        }

        // Trim trailing spaces with transparent background
        var end = chars.size
        while (end > 0 && chars[end - 1].char == ' ' && chars[end - 1].bg == Color.Transparent) {
            end--
        }

        return TerminalLine(if (end == chars.size) chars else chars.subList(0, end))
    }

    @Synchronized
    fun resize(newCols: Int, newRows: Int) {
        if (newCols <= 0 || newRows <= 0) return
        if (newCols == cols && newRows == rows) return

        val newCells = makeGrid(newCols, newRows)

        // Copy existing content
        val copyRows = minOf(rows, newRows)
        val copyCols = minOf(cols, newCols)
        for (row in 0 until copyRows) {
            for (col in 0 until copyCols) {
                newCells[row][col].copyFrom(cells[row][col])
            }
        }

        cells = newCells
        cols = newCols
        rows = newRows

        cursorRow = cursorRow.coerceIn(0, rows - 1)
        cursorCol = cursorCol.coerceIn(0, cols - 1)

        scrollTop = 0
        scrollBottom = rows - 1

        // Resize alt screen if active
        altScreen?.let {
            val newAlt = makeGrid(newCols, newRows)
            val altCopyRows = minOf(it.size, newRows)
            val altCopyCols = minOf(if (it.isNotEmpty()) it[0].size else 0, newCols)
            for (row in 0 until altCopyRows) {
                for (col in 0 until altCopyCols) {
                    newAlt[row][col].copyFrom(it[row][col])
                }
            }
            altScreen = newAlt
        }

        wrapPending = false
    }

    @Synchronized
    fun reset() {
        cells = makeGrid(cols, rows)
        scrollback.clear()
        cursorRow = 0
        cursorCol = 0
        savedCursorRow = 0
        savedCursorCol = 0
        savedSgrState = SgrState()
        scrollTop = 0
        scrollBottom = rows - 1
        altScreen = null
        sgrState.reset()
        parseState = ParseState.Normal
        autoWrap = true
        wrapPending = false
        utf8Remaining = 0
        utf8Codepoint = 0
    }

    private fun List<Int>.getOrDefault(index: Int, default: Int): Int {
        return if (index < size) this[index] else default
    }

    private fun makeGrid(gridCols: Int, gridRows: Int): Array<Array<Cell>> {
        return Array(gridRows) { Array(gridCols) { Cell() } }
    }
}
