package com.zremote.data

import android.content.Context
import com.zremote.sdk.FfiError
import com.zremote.sdk.ZRemoteClient
import com.zremote.services.ZRemoteEventService
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import javax.inject.Inject
import javax.inject.Singleton

@Singleton
class ConnectionManager @Inject constructor(
    val eventRepository: ZRemoteEventRepository,
    @ApplicationContext private val appContext: Context,
) {
    @Volatile
    var client: ZRemoteClient? = null
        private set

    private val _connectionError = MutableStateFlow<String?>(null)
    val connectionError: StateFlow<String?> = _connectionError.asStateFlow()

    @Synchronized
    fun connect(serverUrl: String) {
        disconnect()
        try {
            val newClient = ZRemoteClient(serverUrl)
            client = newClient
            eventRepository.connect(newClient)
            _connectionError.value = null
            ZRemoteEventService.start(appContext, serverUrl)
        } catch (e: FfiError) {
            _connectionError.value = e.message
        }
    }

    @Synchronized
    fun disconnect() {
        ZRemoteEventService.stop(appContext)
        eventRepository.disconnect()
        client = null
    }
}
