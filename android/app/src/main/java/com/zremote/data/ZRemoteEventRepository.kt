package com.zremote.data

import com.zremote.sdk.EventListener
import com.zremote.sdk.FfiClaudeSessionMetrics
import com.zremote.sdk.FfiHostInfo
import com.zremote.sdk.FfiLoopInfo
import com.zremote.sdk.FfiSessionInfo
import com.zremote.sdk.ZRemoteClient
import com.zremote.sdk.ZRemoteEventStream
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import javax.inject.Inject
import javax.inject.Singleton

@Singleton
class ZRemoteEventRepository @Inject constructor() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val _isConnected = MutableStateFlow(false)
    val isConnected: StateFlow<Boolean> = _isConnected.asStateFlow()

    private val _loops = MutableStateFlow<List<FfiLoopInfo>>(emptyList())
    val loops: StateFlow<List<FfiLoopInfo>> = _loops.asStateFlow()

    private val _sessionMetrics = MutableStateFlow<Map<String, FfiClaudeSessionMetrics>>(emptyMap())
    val sessionMetrics: StateFlow<Map<String, FfiClaudeSessionMetrics>> = _sessionMetrics.asStateFlow()

    private var eventStream: ZRemoteEventStream? = null
    private var client: ZRemoteClient? = null

    fun connect(newClient: ZRemoteClient) {
        disconnect()
        client = newClient
        eventStream = newClient.connectEvents(Listener())
    }

    fun disconnect() {
        eventStream?.disconnect()
        eventStream = null
        client = null
        _isConnected.value = false
        _loops.value = emptyList()
        _sessionMetrics.value = emptyMap()
        scope.coroutineContext[Job]?.cancelChildren()
    }

    private inner class Listener : EventListener {
        override fun onConnected() {
            _isConnected.value = true
        }

        override fun onDisconnected() {
            _isConnected.value = false
        }

        override fun onHostConnected(host: FfiHostInfo) {
            refreshAfterEvent()
        }

        override fun onHostDisconnected(hostId: String) {
            refreshAfterEvent()
        }

        override fun onHostStatusChanged(hostId: String, status: String) {}

        override fun onSessionCreated(session: FfiSessionInfo) {}
        override fun onSessionClosed(sessionId: String, exitCode: Int?) {}
        override fun onSessionUpdated(sessionId: String) {}
        override fun onSessionSuspended(sessionId: String) {}
        override fun onSessionResumed(sessionId: String) {}

        override fun onProjectsUpdated(hostId: String) {}

        override fun onLoopDetected(loopInfo: FfiLoopInfo, hostId: String, hostname: String) {
            _loops.update { current -> current + loopInfo }
        }

        override fun onLoopStatusChanged(loopInfo: FfiLoopInfo, hostId: String, hostname: String) {
            _loops.update { current ->
                current.map { if (it.id == loopInfo.id) loopInfo else it }
            }
        }

        override fun onLoopEnded(loopInfo: FfiLoopInfo, hostId: String, hostname: String) {
            _loops.update { current ->
                current.map { if (it.id == loopInfo.id) loopInfo else it }
            }
        }

        override fun onKnowledgeStatusChanged(hostId: String, status: String, error: String?) {}
        override fun onIndexingProgress(
            projectId: String, projectPath: String, status: String,
            filesProcessed: ULong, filesTotal: ULong,
        ) {}
        override fun onMemoryExtracted(projectId: String, loopId: String, memoryCount: UInt) {}
        override fun onWorktreeError(hostId: String, projectPath: String, message: String) {}

        override fun onClaudeTaskStarted(
            taskId: String, sessionId: String, hostId: String, projectPath: String,
        ) {}
        override fun onClaudeTaskUpdated(taskId: String, status: String, loopId: String?) {}
        override fun onClaudeTaskEnded(taskId: String, status: String, summary: String?) {}

        override fun onClaudeSessionMetrics(metrics: FfiClaudeSessionMetrics) {
            _sessionMetrics.update { current ->
                current + (metrics.sessionId to metrics)
            }
        }
    }
}
