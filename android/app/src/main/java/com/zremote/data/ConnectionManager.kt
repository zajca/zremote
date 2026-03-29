package com.zremote.data

import android.content.Context
import android.util.Log
import com.zremote.sdk.FfiException
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
        Log.i("ZRemote", "connect() called with url=$serverUrl")
        disconnect()
        try {
            Log.i("ZRemote", "Creating ZRemoteClient...")
            val newClient = ZRemoteClient(serverUrl)
            Log.i("ZRemote", "ZRemoteClient created, connecting events...")
            client = newClient
            eventRepository.connect(newClient)
            // Mark as connected immediately so UI can start loading data.
            // The WS event stream will update this via onConnected/onDisconnected.
            eventRepository.setConnected(true)
            _connectionError.value = null
            ZRemoteEventService.start(appContext, serverUrl)
            Log.i("ZRemote", "Connected successfully")
        } catch (e: Exception) {
            Log.e("ZRemote", "connect() Exception", e)
            _connectionError.value = e.message ?: e.toString()
        } catch (e: Error) {
            Log.e("ZRemote", "connect() Error", e)
            _connectionError.value = e.message ?: e.toString()
        }
    }

    @Synchronized
    fun disconnect() {
        ZRemoteEventService.stop(appContext)
        eventRepository.disconnect()
        client = null
    }
}
