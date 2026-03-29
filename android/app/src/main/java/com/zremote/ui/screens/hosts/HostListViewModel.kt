package com.zremote.ui.screens.hosts

import android.util.Log
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.zremote.data.ConnectionManager
import com.zremote.sdk.FfiHost
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
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
        val client = connectionManager.client
        Log.i("ZRemote", "HostListVM.refresh() client=${client != null} isConnected=${isConnected.value}")
        if (client == null) return
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null
            try {
                val result = withContext(Dispatchers.IO) { client.listHosts() }
                Log.i("ZRemote", "HostListVM.refresh() got ${result.size} hosts")
                _hosts.value = result
            } catch (e: Exception) {
                Log.e("ZRemote", "HostListVM.refresh() error", e)
                _error.value = e.message ?: e.toString()
            } catch (e: Error) {
                Log.e("ZRemote", "HostListVM.refresh() Error", e)
                _error.value = e.message ?: e.toString()
            } finally {
                _isLoading.value = false
            }
        }
    }
}
