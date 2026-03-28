package com.zremote.ui.screens.loops

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.zremote.data.ConnectionManager
import com.zremote.data.ZRemoteEventRepository
import com.zremote.sdk.FfiAgenticLoop
import com.zremote.sdk.FfiClaudeSessionMetrics
import com.zremote.sdk.FfiListLoopsFilter
import com.zremote.sdk.FfiLoopInfo
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class LoopListViewModel @Inject constructor(
    private val connectionManager: ConnectionManager,
    private val eventRepository: ZRemoteEventRepository,
) : ViewModel() {

    private val _loops = MutableStateFlow<List<FfiAgenticLoop>>(emptyList())
    val loops: StateFlow<List<FfiAgenticLoop>> = _loops.asStateFlow()

    val realtimeLoops: StateFlow<List<FfiLoopInfo>> = eventRepository.loops
    val sessionMetrics: StateFlow<Map<String, FfiClaudeSessionMetrics>> = eventRepository.sessionMetrics

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        val client = connectionManager.client ?: return
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null
            try {
                _loops.value = client.listLoops(FfiListLoopsFilter(
                    status = null, hostId = null, sessionId = null, projectId = null,
                ))
            } catch (e: Exception) {
                _error.value = e.message
            } finally {
                _isLoading.value = false
            }
        }
    }
}
