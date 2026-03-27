package com.zremote.ui.screens.sessions

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.zremote.data.ConnectionManager
import com.zremote.sdk.FfiSession
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class SessionListViewModel @Inject constructor(
    private val connectionManager: ConnectionManager,
) : ViewModel() {

    private val _sessions = MutableStateFlow<List<FfiSession>>(emptyList())
    val sessions: StateFlow<List<FfiSession>> = _sessions.asStateFlow()

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading.asStateFlow()

    fun loadSessions(hostId: String) {
        val client = connectionManager.client ?: return
        viewModelScope.launch {
            _isLoading.value = true
            try {
                _sessions.value = client.listSessions(hostId)
            } catch (_: Exception) {
                _sessions.value = emptyList()
            } finally {
                _isLoading.value = false
            }
        }
    }
}
