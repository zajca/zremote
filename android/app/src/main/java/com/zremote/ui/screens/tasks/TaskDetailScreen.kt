package com.zremote.ui.screens.tasks

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.zremote.sdk.FfiClaudeTaskStatus
import com.zremote.ui.components.DetailRow
import com.zremote.ui.components.ErrorState
import com.zremote.ui.components.LoadingState
import com.zremote.ui.theme.StatusCompleted
import com.zremote.ui.theme.StatusError
import com.zremote.ui.theme.StatusWorking

@Composable
fun TaskDetailScreen(
    taskId: String,
    onLoopClick: ((String) -> Unit)? = null,
    viewModel: TaskDetailViewModel = hiltViewModel(),
) {
    val task by viewModel.task.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()
    val error by viewModel.error.collectAsStateWithLifecycle()

    LaunchedEffect(taskId) {
        viewModel.loadTask(taskId)
    }

    val currentError = error
    when {
        isLoading -> LoadingState()
        currentError != null && task == null -> ErrorState(
            message = currentError,
            onRetry = { viewModel.refresh() },
        )
        else -> {
            val taskData = task
            if (taskData == null) {
                LoadingState()
                return
            }

            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(16.dp)
                    .verticalScroll(rememberScrollState()),
            ) {
                Text(
                    text = taskData.taskName ?: taskData.initialPrompt?.take(80) ?: "Task ${taskData.id.take(8)}",
                    style = MaterialTheme.typography.headlineMedium,
                )

                val statusColor = when (taskData.status) {
                    FfiClaudeTaskStatus.STARTING -> StatusWorking
                    FfiClaudeTaskStatus.ACTIVE -> StatusWorking
                    FfiClaudeTaskStatus.COMPLETED -> StatusCompleted
                    FfiClaudeTaskStatus.ERROR -> StatusError
                }
                Text(
                    text = taskData.status.name.lowercase(),
                    style = MaterialTheme.typography.bodyMedium,
                    color = statusColor,
                    modifier = Modifier.padding(top = 4.dp),
                )

                Spacer(Modifier.height(16.dp))

                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text("Details", style = MaterialTheme.typography.titleMedium)
                        Spacer(Modifier.height(8.dp))
                        taskData.model?.let { DetailRow("Model", it) }
                        DetailRow("Project", taskData.projectPath)
                        DetailRow("Started", taskData.startedAt)
                        taskData.endedAt?.let { DetailRow("Ended", it) }
                        DetailRow("Session", taskData.sessionId.take(8))
                    }
                }

                taskData.totalCostUsd?.let { cost ->
                    Spacer(Modifier.height(12.dp))
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(modifier = Modifier.padding(16.dp)) {
                            Text("Usage", style = MaterialTheme.typography.titleMedium)
                            Spacer(Modifier.height(8.dp))
                            DetailRow("Cost", "$${String.format("%.4f", cost)}")
                            taskData.totalTokensIn?.let { DetailRow("Tokens in", it.toString()) }
                            taskData.totalTokensOut?.let { DetailRow("Tokens out", it.toString()) }
                        }
                    }
                }

                taskData.initialPrompt?.let { prompt ->
                    Spacer(Modifier.height(12.dp))
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(modifier = Modifier.padding(16.dp)) {
                            Text("Initial prompt", style = MaterialTheme.typography.titleMedium)
                            Spacer(Modifier.height(8.dp))
                            Text(
                                text = prompt,
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                    }
                }

                taskData.summary?.let { summary ->
                    Spacer(Modifier.height(12.dp))
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(modifier = Modifier.padding(16.dp)) {
                            Text("Summary", style = MaterialTheme.typography.titleMedium)
                            Spacer(Modifier.height(8.dp))
                            Text(
                                text = summary,
                                style = MaterialTheme.typography.bodyMedium,
                            )
                        }
                    }
                }

                taskData.loopId?.let { loopId ->
                    Spacer(Modifier.height(12.dp))
                    Card(
                        onClick = { onLoopClick?.invoke(loopId) },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Column(modifier = Modifier.padding(16.dp)) {
                            Text("Associated loop", style = MaterialTheme.typography.titleMedium)
                            Spacer(Modifier.height(8.dp))
                            DetailRow("Loop ID", loopId.take(8))
                            if (onLoopClick != null) {
                                Text(
                                    text = "Tap to view loop details",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.primary,
                                    modifier = Modifier.padding(top = 4.dp),
                                )
                            }
                        }
                    }
                }

                Spacer(Modifier.height(12.dp))
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(modifier = Modifier.padding(16.dp)) {
                        Text(
                            text = "Permission approval",
                            style = MaterialTheme.typography.titleMedium,
                        )
                        Spacer(Modifier.height(8.dp))
                        Text(
                            text = "Approve/deny tool calls requires server v0.8+",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }

                Spacer(Modifier.height(16.dp))
            }
        }
    }
}
