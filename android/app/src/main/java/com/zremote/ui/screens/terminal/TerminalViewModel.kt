package com.zremote.ui.screens.terminal

import android.util.Log
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.zremote.data.ConnectionManager
import com.zremote.sdk.TerminalListener
import com.zremote.sdk.ZRemoteTerminal
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class TerminalViewModel @Inject constructor(
    private val connectionManager: ConnectionManager,
) : ViewModel() {

    private val vt = VtTerminal(80, 24)
    private var terminal: ZRemoteTerminal? = null

    private val _lines = MutableStateFlow<List<TerminalLine>>(emptyList())
    val lines: StateFlow<List<TerminalLine>> = _lines.asStateFlow()

    private val _isConnected = MutableStateFlow(false)
    val isConnected: StateFlow<Boolean> = _isConnected.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    fun connectToSession(sessionId: String) {
        val client = connectionManager.client
        if (client == null) {
            Log.e("ZRemote", "TerminalVM.connectToSession($sessionId) client is null")
            _error.value = "Not connected to server"
            return
        }
        try {
            terminal?.disconnect()
            terminal = client.connectTerminal(sessionId, Listener())
            _isConnected.value = true
        } catch (e: Exception) {
            Log.e("ZRemote", "TerminalVM.connectToSession($sessionId) Exception", e)
            _error.value = e.message ?: e.toString()
        } catch (e: Error) {
            Log.e("ZRemote", "TerminalVM.connectToSession($sessionId) Error", e)
            _error.value = e.message ?: e.toString()
        }
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
        vt.resize(cols.toInt(), rows.toInt())
    }

    override fun onCleared() {
        terminal?.disconnect()
        terminal = null
    }

    private inner class Listener : TerminalListener {
        override fun onOutput(data: ByteArray) {
            viewModelScope.launch(Dispatchers.Default) {
                try {
                    vt.write(data)
                    _lines.value = vt.getDisplayLines()
                } catch (e: Exception) {
                    Log.e("ZRemote", "TerminalVM.onOutput error", e)
                }
            }
        }

        override fun onPaneOutput(paneId: String, data: ByteArray) {
            onOutput(data)
        }

        override fun onPaneAdded(paneId: String, index: UShort) {}
        override fun onPaneRemoved(paneId: String) {}

        override fun onSessionClosed(exitCode: Int?) {
            _isConnected.value = false
            if (exitCode != null) {
                _error.value = "Session closed (exit $exitCode)"
            }
        }

        override fun onScrollbackStart(cols: UShort, rows: UShort) {
            vt.reset()
            if (cols.toInt() > 0 && rows.toInt() > 0) {
                vt.resize(cols.toInt(), rows.toInt())
            }
        }

        override fun onScrollbackEnd(truncated: Boolean) {}
        override fun onSessionSuspended() {
            _isConnected.value = false
        }
        override fun onSessionResumed() {
            _isConnected.value = true
        }

        override fun onError(message: String) {
            Log.e("ZRemote", "TerminalVM.onError: $message")
            _error.value = message
        }

        override fun onDisconnected() {
            _isConnected.value = false
        }
    }
}
