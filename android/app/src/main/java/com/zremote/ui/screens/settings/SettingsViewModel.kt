package com.zremote.ui.screens.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.zremote.data.ConnectionManager
import com.zremote.data.SettingsRepository
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class SettingsViewModel @Inject constructor(
    private val settingsRepository: SettingsRepository,
    private val connectionManager: ConnectionManager,
) : ViewModel() {

    private val _serverUrl = MutableStateFlow("")
    val serverUrl: StateFlow<String> = _serverUrl.asStateFlow()

    val isConnected = connectionManager.eventRepository.isConnected
    val connectionError = connectionManager.connectionError

    init {
        viewModelScope.launch {
            settingsRepository.serverUrl.collect { url ->
                _serverUrl.value = url
            }
        }
    }

    fun updateServerUrl(url: String) {
        _serverUrl.value = url
        viewModelScope.launch {
            settingsRepository.setServerUrl(url)
        }
    }

    fun connect() {
        val url = _serverUrl.value
        if (url.isNotBlank()) {
            connectionManager.connect(url)
        }
    }

    fun disconnect() {
        connectionManager.disconnect()
    }
}
