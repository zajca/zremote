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

    private val _notifyLoopCompletions = MutableStateFlow(true)
    val notifyLoopCompletions: StateFlow<Boolean> = _notifyLoopCompletions.asStateFlow()

    private val _notifyLoopErrors = MutableStateFlow(true)
    val notifyLoopErrors: StateFlow<Boolean> = _notifyLoopErrors.asStateFlow()

    private val _notifyPermissionRequests = MutableStateFlow(true)
    val notifyPermissionRequests: StateFlow<Boolean> = _notifyPermissionRequests.asStateFlow()

    private val _notifyTaskCompletions = MutableStateFlow(true)
    val notifyTaskCompletions: StateFlow<Boolean> = _notifyTaskCompletions.asStateFlow()

    private val _notifyTaskErrors = MutableStateFlow(true)
    val notifyTaskErrors: StateFlow<Boolean> = _notifyTaskErrors.asStateFlow()

    private val _notifyHostDisconnections = MutableStateFlow(true)
    val notifyHostDisconnections: StateFlow<Boolean> = _notifyHostDisconnections.asStateFlow()

    init {
        viewModelScope.launch {
            settingsRepository.serverUrl.collect { url ->
                _serverUrl.value = url
            }
        }
        viewModelScope.launch {
            settingsRepository.notifyLoopCompletions.collect { _notifyLoopCompletions.value = it }
        }
        viewModelScope.launch {
            settingsRepository.notifyLoopErrors.collect { _notifyLoopErrors.value = it }
        }
        viewModelScope.launch {
            settingsRepository.notifyPermissionRequests.collect { _notifyPermissionRequests.value = it }
        }
        viewModelScope.launch {
            settingsRepository.notifyTaskCompletions.collect { _notifyTaskCompletions.value = it }
        }
        viewModelScope.launch {
            settingsRepository.notifyTaskErrors.collect { _notifyTaskErrors.value = it }
        }
        viewModelScope.launch {
            settingsRepository.notifyHostDisconnections.collect { _notifyHostDisconnections.value = it }
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

    fun setNotifyLoopCompletions(enabled: Boolean) {
        _notifyLoopCompletions.value = enabled
        viewModelScope.launch { settingsRepository.setNotifyLoopCompletions(enabled) }
    }

    fun setNotifyLoopErrors(enabled: Boolean) {
        _notifyLoopErrors.value = enabled
        viewModelScope.launch { settingsRepository.setNotifyLoopErrors(enabled) }
    }

    fun setNotifyPermissionRequests(enabled: Boolean) {
        _notifyPermissionRequests.value = enabled
        viewModelScope.launch { settingsRepository.setNotifyPermissionRequests(enabled) }
    }

    fun setNotifyTaskCompletions(enabled: Boolean) {
        _notifyTaskCompletions.value = enabled
        viewModelScope.launch { settingsRepository.setNotifyTaskCompletions(enabled) }
    }

    fun setNotifyTaskErrors(enabled: Boolean) {
        _notifyTaskErrors.value = enabled
        viewModelScope.launch { settingsRepository.setNotifyTaskErrors(enabled) }
    }

    fun setNotifyHostDisconnections(enabled: Boolean) {
        _notifyHostDisconnections.value = enabled
        viewModelScope.launch { settingsRepository.setNotifyHostDisconnections(enabled) }
    }
}
