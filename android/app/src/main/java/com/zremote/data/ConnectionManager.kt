package com.zremote.data

import com.zremote.sdk.FfiError
import com.zremote.sdk.ZRemoteClient
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import javax.inject.Inject
import javax.inject.Singleton

@Singleton
class ConnectionManager @Inject constructor(
    val eventRepository: ZRemoteEventRepository,
) {
    var client: ZRemoteClient? = null
        private set

    private val _connectionError = MutableStateFlow<String?>(null)
    val connectionError: StateFlow<String?> = _connectionError.asStateFlow()

    fun connect(serverUrl: String) {
        disconnect()
        try {
            val newClient = ZRemoteClient(serverUrl)
            client = newClient
            eventRepository.connect(newClient)
            _connectionError.value = null
        } catch (e: FfiError) {
            _connectionError.value = e.message
        }
    }

    fun disconnect() {
        eventRepository.disconnect()
        client = null
    }
}
