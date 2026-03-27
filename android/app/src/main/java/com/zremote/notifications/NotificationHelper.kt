package com.zremote.notifications

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import androidx.core.app.NotificationCompat

object NotificationHelper {

    const val CHANNEL_LOOP_STATUS = "loop_status"
    const val CHANNEL_LOOP_ERRORS = "loop_errors"
    const val CHANNEL_PERMISSIONS = "permissions"
    const val CHANNEL_TASK_STATUS = "task_status"
    const val CHANNEL_TASK_ERRORS = "task_errors"
    const val CHANNEL_HOST_STATUS = "host_status"
    const val CHANNEL_SERVICE = "service"

    fun createChannels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)

        val channels = listOf(
            NotificationChannel(
                CHANNEL_LOOP_STATUS, "Loop Status",
                NotificationManager.IMPORTANCE_DEFAULT,
            ).apply { description = "Agentic loop completion notifications" },

            NotificationChannel(
                CHANNEL_LOOP_ERRORS, "Loop Errors",
                NotificationManager.IMPORTANCE_HIGH,
            ).apply { description = "Agentic loop error notifications" },

            NotificationChannel(
                CHANNEL_PERMISSIONS, "Permission Requests",
                NotificationManager.IMPORTANCE_HIGH,
            ).apply { description = "Tool call permission requests requiring approval" },

            NotificationChannel(
                CHANNEL_TASK_STATUS, "Task Status",
                NotificationManager.IMPORTANCE_DEFAULT,
            ).apply { description = "Claude task completion notifications" },

            NotificationChannel(
                CHANNEL_TASK_ERRORS, "Task Errors",
                NotificationManager.IMPORTANCE_HIGH,
            ).apply { description = "Claude task error notifications" },

            NotificationChannel(
                CHANNEL_HOST_STATUS, "Host Status",
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = "Host connection/disconnection notifications" },

            NotificationChannel(
                CHANNEL_SERVICE, "Background Service",
                NotificationManager.IMPORTANCE_MIN,
            ).apply { description = "Keeps connection alive in background" },
        )

        manager.createNotificationChannels(channels)
    }

    fun buildNotification(
        context: Context,
        channel: String,
        title: String,
        body: String,
    ): NotificationCompat.Builder {
        return NotificationCompat.Builder(context, channel)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(title)
            .setContentText(body)
            .setAutoCancel(true)
    }

    fun buildServiceNotification(context: Context): NotificationCompat.Builder {
        return NotificationCompat.Builder(context, CHANNEL_SERVICE)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle("ZRemote")
            .setContentText("Connected to server")
            .setOngoing(true)
    }
}
