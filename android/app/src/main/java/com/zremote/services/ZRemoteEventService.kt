package com.zremote.services

import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.IBinder
import androidx.core.app.ServiceCompat
import com.zremote.notifications.NotificationEventListener
import com.zremote.notifications.NotificationHelper
import com.zremote.sdk.ZRemoteClient
import com.zremote.sdk.ZRemoteEventStream

class ZRemoteEventService : Service() {

    private var eventStream: ZRemoteEventStream? = null

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

        connectToServer(serverUrl)
        return START_STICKY
    }

    private fun connectToServer(serverUrl: String) {
        eventStream?.disconnect()
        try {
            val client = ZRemoteClient(serverUrl)
            val listener = NotificationEventListener(
                context = this,
                isAppBackgrounded = { true },
            )
            eventStream = client.connectEvents(listener)
        } catch (_: Exception) {
            stopSelf()
        }
    }

    override fun onDestroy() {
        eventStream?.disconnect()
        eventStream = null
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
