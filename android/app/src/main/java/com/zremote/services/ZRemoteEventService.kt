package com.zremote.services

import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import androidx.core.app.ServiceCompat
import com.zremote.data.SettingsRepository
import com.zremote.data.dataStore
import com.zremote.notifications.NotificationEventListener
import com.zremote.notifications.NotificationHelper
import com.zremote.notifications.NotificationPreferences
import com.zremote.sdk.ZRemoteClient
import com.zremote.sdk.ZRemoteEventStream
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

class ZRemoteEventService : Service() {

    private var eventStream: ZRemoteEventStream? = null
    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    override fun onCreate() {
        super.onCreate()
        NotificationHelper.createChannels(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val serverUrl = intent?.getStringExtra(EXTRA_SERVER_URL)
        if (serverUrl.isNullOrBlank()) {
            stopSelf()
            return START_NOT_STICKY
        }

        val notification = NotificationHelper
            .buildServiceNotification(this)
            .build()
        ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, 0)

        serviceScope.launch {
            connectToServer(serverUrl)
        }
        return START_STICKY
    }

    private suspend fun connectToServer(serverUrl: String) {
        eventStream?.disconnect()
        try {
            val client = ZRemoteClient(serverUrl)
            val preferences = loadPreferences()
            val listener = NotificationEventListener(
                context = this,
                isAppBackgrounded = { true },
                preferences = preferences,
            )
            eventStream = client.connectEvents(listener)
        } catch (_: Exception) {
            stopSelf()
        }
    }

    private suspend fun loadPreferences(): NotificationPreferences {
        return try {
            val prefs = applicationContext.dataStore.data.first()
            NotificationPreferences(
                loopCompletions = prefs[SettingsRepository.KEY_NOTIFY_LOOP_COMPLETIONS] ?: true,
                loopErrors = prefs[SettingsRepository.KEY_NOTIFY_LOOP_ERRORS] ?: true,
                permissionRequests = prefs[SettingsRepository.KEY_NOTIFY_PERMISSION_REQUESTS] ?: true,
                taskCompletions = prefs[SettingsRepository.KEY_NOTIFY_TASK_COMPLETIONS] ?: true,
                taskErrors = prefs[SettingsRepository.KEY_NOTIFY_TASK_ERRORS] ?: true,
                hostDisconnections = prefs[SettingsRepository.KEY_NOTIFY_HOST_DISCONNECTIONS] ?: true,
            )
        } catch (_: Exception) {
            NotificationPreferences()
        }
    }

    override fun onDestroy() {
        eventStream?.disconnect()
        eventStream = null
        serviceScope.cancel()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        private const val NOTIFICATION_ID = 1
        private const val EXTRA_SERVER_URL = "server_url"

        fun start(context: Context, serverUrl: String) {
            val intent = Intent(context, ZRemoteEventService::class.java).apply {
                putExtra(EXTRA_SERVER_URL, serverUrl)
            }
            context.startForegroundService(intent)
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, ZRemoteEventService::class.java))
        }
    }
}
