package com.zremote.ui.screens.terminal

import androidx.lifecycle.ViewModel
import com.zremote.data.ConnectionManager
import com.zremote.sdk.TerminalListener
import com.zremote.sdk.ZRemoteTerminal
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import javax.inject.Inject

@HiltViewModel
class TerminalViewModel @Inject constructor(
    private val connectionManager: ConnectionManager,
) : ViewModel() {

    private val parser = AnsiParser()
    private var terminal: ZRemoteTerminal? = null

    private val _lines = MutableStateFlow<List<TerminalLine>>(emptyList())
    val lines: StateFlow<List<TerminalLine>> = _lines.asStateFlow()

    private val _isConnected = MutableStateFlow(false)
    val isConnected: StateFlow<Boolean> = _isConnected.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    fun connectToSession(sessionId: String) {
        val client = connectionManager.client ?: return
        terminal?.disconnect()
        terminal = client.connectTerminal(sessionId, Listener())
    }

    fun sendInput(data: String) {
        terminal?.sendInput(data.toByteArray())
    }

    fun sendControlChar(c: Char) {
        val code = c.code - 'a'.code + 1
        terminal?.sendInput(byteArrayOf(code.toByte()))
    }

    fun resize(cols: UShort, rows: UShort) {
        terminal?.resize(cols, rows)
    }

    override fun onCleared() {
        terminal?.disconnect()
        terminal = null
    }

    private inner class Listener : TerminalListener {
        override fun onOutput(data: ByteArray) {
            val newLines = parser.parse(data)
            _lines.update { it + newLines }
        }

        override fun onPaneOutput(paneId: String, data: ByteArray) {
            onOutput(data)
        }

        override fun onPaneAdded(paneId: String, index: UShort) {}
        override fun onPaneRemoved(paneId: String) {}

        override fun onSessionClosed(exitCode: Int?) {
            _isConnected.value = false
        }

        override fun onScrollbackStart(cols: UShort, rows: UShort) {
            _lines.value = emptyList()
        }

        override fun onScrollbackEnd(truncated: Boolean) {}
        override fun onSessionSuspended() { _isConnected.value = false }
        override fun onSessionResumed() { _isConnected.value = true }

        override fun onError(message: String) {
            _error.value = message
        }

        override fun onDisconnected() {
            _isConnected.value = false
        }
    }
}
