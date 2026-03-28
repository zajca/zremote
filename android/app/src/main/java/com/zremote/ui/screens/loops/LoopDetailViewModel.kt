package com.zremote.ui.screens.loops

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.zremote.data.ConnectionManager
import com.zremote.sdk.FfiAgenticLoop
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class LoopDetailViewModel @Inject constructor(
    private val connectionManager: ConnectionManager,
) : ViewModel() {

    private val _loop = MutableStateFlow<FfiAgenticLoop?>(null)
    val loop: StateFlow<FfiAgenticLoop?> = _loop.asStateFlow()

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    private var currentLoopId: String? = null

    fun loadLoop(loopId: String) {
        currentLoopId = loopId
        refresh()
    }

    fun refresh() {
        val loopId = currentLoopId ?: return
        val client = connectionManager.client ?: return
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null
            try {
                _loop.value = client.getLoop(loopId)
            } catch (e: Exception) {
                _error.value = e.message
            } finally {
                _isLoading.value = false
            }
        }
    }
}
