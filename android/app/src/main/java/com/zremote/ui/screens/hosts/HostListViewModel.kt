package com.zremote.ui.screens.hosts

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.zremote.data.ConnectionManager
import com.zremote.sdk.FfiHost
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class HostListViewModel @Inject constructor(
    private val connectionManager: ConnectionManager,
) : ViewModel() {

    private val _hosts = MutableStateFlow<List<FfiHost>>(emptyList())
    val hosts: StateFlow<List<FfiHost>> = _hosts.asStateFlow()

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    val isConnected = connectionManager.eventRepository.isConnected

    init {
        refresh()
    }

    fun refresh() {
        val client = connectionManager.client ?: return
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null
            try {
                _hosts.value = client.listHosts()
            } catch (e: Exception) {
                _error.value = e.message
            } finally {
                _isLoading.value = false
            }
        }
    }
}
