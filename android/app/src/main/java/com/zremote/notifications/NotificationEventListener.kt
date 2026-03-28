package com.zremote.notifications

import android.content.Context
import androidx.core.app.NotificationManagerCompat
import com.zremote.sdk.EventListener
import com.zremote.sdk.FfiAgenticStatus
import com.zremote.sdk.FfiClaudeSessionMetrics
import com.zremote.sdk.FfiHostInfo
import com.zremote.sdk.FfiLoopInfo
import com.zremote.sdk.FfiSessionInfo

class NotificationEventListener(
    private val context: Context,
    private val isAppBackgrounded: () -> Boolean,
    private val preferences: NotificationPreferences = NotificationPreferences(),
) : EventListener {

    private val notificationManager = NotificationManagerCompat.from(context)

    private fun notifyIfBackgrounded(id: Int, channel: String, title: String, body: String) {
        if (!isAppBackgrounded()) return
        try {
            val notification = NotificationHelper
                .buildNotification(context, channel, title, body)
                .build()
            notificationManager.notify(id, notification)
        } catch (_: SecurityException) {
            // POST_NOTIFICATIONS permission not granted
        }
    }

    // Connection lifecycle
    override fun onConnected() {}
    override fun onDisconnected() {}

    // Hosts
    override fun onHostConnected(host: FfiHostInfo) {}

    override fun onHostDisconnected(hostId: String) {
        if (!preferences.hostDisconnections) return
        notifyIfBackgrounded(
            hostId.hashCode(),
            NotificationHelper.CHANNEL_HOST_STATUS,
            "Host disconnected",
            hostId.take(8),
        )
    }

    override fun onHostStatusChanged(hostId: String, status: String) {}

    // Sessions
    override fun onSessionCreated(session: FfiSessionInfo) {}
    override fun onSessionClosed(sessionId: String, exitCode: Int?) {}
    override fun onSessionUpdated(sessionId: String) {}
    override fun onSessionSuspended(sessionId: String) {}
    override fun onSessionResumed(sessionId: String) {}

    // Projects
    override fun onProjectsUpdated(hostId: String) {}

    // Loops
    override fun onLoopDetected(loopInfo: FfiLoopInfo, hostId: String, hostname: String) {}

    override fun onLoopStatusChanged(loopInfo: FfiLoopInfo, hostId: String, hostname: String) {
        if (loopInfo.status == FfiAgenticStatus.WAITING_FOR_INPUT && preferences.permissionRequests) {
            notifyIfBackgrounded(
                loopInfo.id.hashCode(),
                NotificationHelper.CHANNEL_PERMISSIONS,
                "Permission request on $hostname",
                "${loopInfo.toolName}: waiting for input",
            )
        }
    }

    override fun onLoopEnded(loopInfo: FfiLoopInfo, hostId: String, hostname: String) {
        val (channel, title, enabled) = when (loopInfo.status) {
            FfiAgenticStatus.ERROR -> Triple(
                NotificationHelper.CHANNEL_LOOP_ERRORS,
                "Loop error on $hostname",
                preferences.loopErrors,
            )
            else -> Triple(
                NotificationHelper.CHANNEL_LOOP_STATUS,
                "Loop completed on $hostname",
                preferences.loopCompletions,
            )
        }

        if (!enabled) return

        notifyIfBackgrounded(
            loopInfo.id.hashCode(),
            channel,
            title,
            "${loopInfo.taskName ?: loopInfo.toolName}: ${loopInfo.status.name.lowercase()}",
        )
    }

    // Knowledge
    override fun onKnowledgeStatusChanged(hostId: String, status: String, error: String?) {}
    override fun onIndexingProgress(
        projectId: String, projectPath: String, status: String,
        filesProcessed: ULong, filesTotal: ULong,
    ) {}
    override fun onMemoryExtracted(projectId: String, loopId: String, memoryCount: UInt) {}
    override fun onWorktreeError(hostId: String, projectPath: String, message: String) {}

    // Claude tasks
    override fun onClaudeTaskStarted(
        taskId: String, sessionId: String, hostId: String, projectPath: String,
    ) {}

    override fun onClaudeTaskUpdated(taskId: String, status: String, loopId: String?) {}

    override fun onClaudeTaskEnded(taskId: String, status: String, summary: String?) {
        val (channel, title, enabled) = when (status) {
            "error" -> Triple(NotificationHelper.CHANNEL_TASK_ERRORS, "Task failed", preferences.taskErrors)
            else -> Triple(NotificationHelper.CHANNEL_TASK_STATUS, "Task completed", preferences.taskCompletions)
        }
        if (!enabled) return
        notifyIfBackgrounded(
            taskId.hashCode(),
            channel,
            title,
            summary ?: "Task $status",
        )
    }

    override fun onClaudeSessionMetrics(metrics: FfiClaudeSessionMetrics) {}
}

data class NotificationPreferences(
    val loopCompletions: Boolean = true,
    val loopErrors: Boolean = true,
    val permissionRequests: Boolean = true,
    val taskCompletions: Boolean = true,
    val taskErrors: Boolean = true,
    val hostDisconnections: Boolean = true,
)
