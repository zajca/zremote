package com.zremote.ui.screens.sessions

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Terminal
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.zremote.sdk.FfiClaudeSessionMetrics
import com.zremote.sdk.FfiClaudeTask
import com.zremote.sdk.FfiClaudeTaskStatus
import com.zremote.sdk.FfiSession
import com.zremote.ui.components.EmptyState
import com.zremote.ui.components.ErrorState
import com.zremote.ui.components.LoadingState
import com.zremote.ui.components.RefreshableList
import com.zremote.ui.components.StatusDot
import com.zremote.ui.theme.StatusCompleted
import com.zremote.ui.theme.StatusOffline
import com.zremote.ui.theme.StatusOnline
import com.zremote.ui.theme.StatusWaitingForInput
import com.zremote.ui.theme.StatusWorking

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionListScreen(
    hostId: String,
    onSessionClick: (String) -> Unit = {},
    viewModel: SessionListViewModel = hiltViewModel(),
) {
    val sessions by viewModel.sessions.collectAsStateWithLifecycle()
    val claudeTasks by viewModel.claudeTasks.collectAsStateWithLifecycle()
    val sessionMetrics by viewModel.sessionMetrics.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()
    val error by viewModel.error.collectAsStateWithLifecycle()
    val createdSessionId by viewModel.createdSessionId.collectAsStateWithLifecycle()

    var showCreateSheet by remember { mutableStateOf(false) }

    LaunchedEffect(hostId) {
        viewModel.loadSessions(hostId)
    }

    LaunchedEffect(createdSessionId) {
        createdSessionId?.let { sessionId ->
            viewModel.clearCreatedSession()
            onSessionClick(sessionId)
        }
    }

    Box(modifier = Modifier.fillMaxSize()) {
        val currentError = error
        when {
            isLoading && sessions.isEmpty() -> LoadingState()
            currentError != null && sessions.isEmpty() -> ErrorState(
                message = currentError,
                onRetry = { viewModel.refresh() },
            )
            sessions.isEmpty() && !isLoading -> EmptyState(
                icon = Icons.Default.Terminal,
                message = "No active sessions",
                hint = "Tap + to create a new terminal session",
            )
            else -> RefreshableList(
                isRefreshing = isLoading,
                onRefresh = { viewModel.refresh() },
            ) {
                items(sessions, key = { it.id }) { session ->
                    SessionCard(
                        session = session,
                        claudeTask = claudeTasks[session.id],
                        metrics = sessionMetrics[session.id],
                        onClick = { onSessionClick(session.id) },
                    )
                }
            }
        }

        FloatingActionButton(
            onClick = { showCreateSheet = true },
            modifier = Modifier
                .align(Alignment.BottomEnd)
                .padding(16.dp),
        ) {
            Icon(Icons.Default.Add, contentDescription = "New session")
        }
    }

    if (showCreateSheet) {
        CreateSessionSheet(
            onDismiss = { showCreateSheet = false },
            onCreate = { shell, workingDir ->
                showCreateSheet = false
                viewModel.createSession(hostId, shell, workingDir)
            },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CreateSessionSheet(
    onDismiss: () -> Unit,
    onCreate: (shell: String?, workingDir: String?) -> Unit,
) {
    val sheetState = rememberModalBottomSheetState()
    var shell by remember { mutableStateOf("") }
    var workingDir by remember { mutableStateOf("") }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheetState,
    ) {
        Column(modifier = Modifier.padding(24.dp)) {
            Text(
                text = "New terminal session",
                style = MaterialTheme.typography.headlineMedium,
            )

            Spacer(Modifier.height(16.dp))

            OutlinedTextField(
                value = shell,
                onValueChange = { shell = it },
                label = { Text("Shell") },
                placeholder = { Text("Default shell") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )

            Spacer(Modifier.height(8.dp))

            OutlinedTextField(
                value = workingDir,
                onValueChange = { workingDir = it },
                label = { Text("Working directory") },
                placeholder = { Text("Home directory") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )

            Spacer(Modifier.height(16.dp))

            Button(
                onClick = {
                    onCreate(
                        shell.ifBlank { null },
                        workingDir.ifBlank { null },
                    )
                },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Create session")
            }

            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun SessionCard(
    session: FfiSession,
    claudeTask: FfiClaudeTask?,
    metrics: FfiClaudeSessionMetrics?,
    onClick: () -> Unit,
) {
    val statusColor = when (session.status) {
        "active" -> StatusOnline
        "suspended" -> StatusWaitingForInput
        else -> StatusOffline
    }

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp)
            .clickable(onClick = onClick),
    ) {
        Row(
            modifier = Modifier.padding(12.dp),
            verticalAlignment = Alignment.Top,
        ) {
            StatusDot(color = statusColor)

            Spacer(Modifier.width(12.dp))

            Column(modifier = Modifier.weight(1f)) {
                // Line 1: shell + status
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        text = session.shell ?: session.name ?: "Session ${session.id.take(8)}",
                        style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        text = session.status,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }

                // Line 2: working directory
                session.workingDir?.let { dir ->
                    Text(
                        text = shortenPath(dir),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }

                // Line 3+: Claude Code info
                if (claudeTask != null) {
                    Spacer(Modifier.height(4.dp))
                    ClaudeTaskInfo(task = claudeTask, metrics = metrics)
                }
            }
        }
    }
}

@Composable
private fun ClaudeTaskInfo(
    task: FfiClaudeTask,
    metrics: FfiClaudeSessionMetrics?,
) {
    val statusColor = when (task.status) {
        FfiClaudeTaskStatus.STARTING, FfiClaudeTaskStatus.ACTIVE -> StatusWorking
        FfiClaudeTaskStatus.COMPLETED -> StatusCompleted
        FfiClaudeTaskStatus.ERROR -> StatusOffline
    }

    val parts = buildList {
        add(task.status.name.lowercase())
        task.model?.let { add(shortenModelName(it)) }
        val cost = task.totalCostUsd ?: metrics?.costUsd
        if (cost != null) add("$${String.format("%.2f", cost)}")
        metrics?.contextUsedPct?.let { add("ctx ${it.toInt()}%") }
    }

    Text(
        text = "CC: ${parts.joinToString(" | ")}",
        style = MaterialTheme.typography.labelSmall,
        color = statusColor,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
    )

    task.initialPrompt?.let { prompt ->
        if (prompt.isNotBlank()) {
            Text(
                text = "\"${prompt.take(50)}${if (prompt.length > 50) "..." else ""}\"",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

private fun shortenPath(path: String): String {
    val normalized = path.replace(Regex("^/home/[^/]+"), "~")
    val parts = normalized.split("/")
    return if (parts.size > 2) {
        parts.takeLast(2).joinToString("/")
    } else {
        normalized
    }
}

private fun shortenModelName(model: String): String {
    // "claude-sonnet-4-5-20250514" -> "sonnet-4.5"
    val withoutPrefix = model.removePrefix("claude-")
    val withoutDate = withoutPrefix.replace(Regex("-\\d{8}$"), "")
    // Convert "sonnet-4-5" -> "sonnet-4.5"
    return withoutDate.replace(Regex("(\\d+)-(\\d+)$"), "$1.$2")
}
