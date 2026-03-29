package com.zremote.ui.screens.sessions

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.zremote.data.ConnectionManager
import com.zremote.sdk.FfiClaudeSessionMetrics
import com.zremote.sdk.FfiClaudeTask
import com.zremote.sdk.FfiCreateSessionRequest
import com.zremote.sdk.FfiListClaudeTasksFilter
import com.zremote.sdk.FfiSession
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import javax.inject.Inject

@HiltViewModel
class SessionListViewModel @Inject constructor(
    private val connectionManager: ConnectionManager,
) : ViewModel() {

    private val _sessions = MutableStateFlow<List<FfiSession>>(emptyList())
    val sessions: StateFlow<List<FfiSession>> = _sessions.asStateFlow()

    private val _claudeTasks = MutableStateFlow<Map<String, FfiClaudeTask>>(emptyMap())
    val claudeTasks: StateFlow<Map<String, FfiClaudeTask>> = _claudeTasks.asStateFlow()

    val sessionMetrics: StateFlow<Map<String, FfiClaudeSessionMetrics>> =
        connectionManager.eventRepository.sessionMetrics

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    private val _createdSessionId = MutableStateFlow<String?>(null)
    val createdSessionId: StateFlow<String?> = _createdSessionId.asStateFlow()

    private var currentHostId: String? = null

    fun loadSessions(hostId: String) {
        currentHostId = hostId
        refresh()
    }

    fun refresh() {
        val hostId = currentHostId ?: return
        val client = connectionManager.client ?: return
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null
            try {
                val allSessions = withContext(Dispatchers.IO) {
                    client.listSessions(hostId)
                }
                _sessions.value = allSessions.filter { it.status != "closed" }

                val tasks = withContext(Dispatchers.IO) {
                    client.listClaudeTasks(
                        FfiListClaudeTasksFilter(
                            hostId = hostId,
                            status = null,
                            projectId = null,
                        ),
                    )
                }
                _claudeTasks.value = tasks.associateBy { it.sessionId }
            } catch (e: Exception) {
                _error.value = e.message
            } finally {
                _isLoading.value = false
            }
        }
    }

    fun createSession(hostId: String, shell: String?, workingDir: String?) {
        val client = connectionManager.client ?: return
        viewModelScope.launch {
            _error.value = null
            try {
                // Initial dimensions; TerminalScreen sends a resize once layout is measured
                val response = client.createSession(
                    hostId,
                    FfiCreateSessionRequest(
                        name = null,
                        shell = shell,
                        cols = DEFAULT_COLS,
                        rows = DEFAULT_ROWS,
                        workingDir = workingDir,
                    ),
                )
                _createdSessionId.value = response.id
                refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    fun clearCreatedSession() {
        _createdSessionId.value = null
    }

    companion object {
        private const val DEFAULT_COLS: UShort = 80u
        private const val DEFAULT_ROWS: UShort = 24u
    }
}
