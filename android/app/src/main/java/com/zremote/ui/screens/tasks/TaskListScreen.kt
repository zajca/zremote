package com.zremote.ui.screens.tasks

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Task
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.zremote.sdk.FfiClaudeTask
import com.zremote.sdk.FfiClaudeTaskStatus
import com.zremote.ui.components.EmptyState
import com.zremote.ui.components.ErrorState
import com.zremote.ui.components.LoadingState
import com.zremote.ui.components.RefreshableList
import com.zremote.ui.theme.StatusCompleted
import com.zremote.ui.theme.StatusError
import com.zremote.ui.theme.StatusWorking

@Composable
fun TaskListScreen(
    onTaskClick: (String) -> Unit = {},
    viewModel: TaskListViewModel = hiltViewModel(),
) {
    val tasks by viewModel.tasks.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()
    val error by viewModel.error.collectAsStateWithLifecycle()

    val currentError = error
    when {
        isLoading && tasks.isEmpty() -> LoadingState()
        currentError != null && tasks.isEmpty() -> ErrorState(
            message = currentError,
            onRetry = { viewModel.refresh() },
        )
        tasks.isEmpty() && !isLoading -> EmptyState(
            icon = Icons.Default.Task,
            message = "No tasks",
            hint = "Tasks will appear when Claude sessions are active",
        )
        else -> RefreshableList(
            isRefreshing = isLoading,
            onRefresh = { viewModel.refresh() },
        ) {
            items(tasks, key = { it.id }) { task ->
                TaskCard(task = task, onClick = { onTaskClick(task.id) })
            }
        }
    }
}

@Composable
private fun TaskCard(task: FfiClaudeTask, onClick: () -> Unit) {
    val statusColor = when (task.status) {
        FfiClaudeTaskStatus.STARTING -> StatusWorking
        FfiClaudeTaskStatus.ACTIVE -> StatusWorking
        FfiClaudeTaskStatus.COMPLETED -> StatusCompleted
        FfiClaudeTaskStatus.ERROR -> StatusError
    }

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp)
            .clickable(onClick = onClick),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    text = task.taskName ?: task.initialPrompt?.take(50) ?: "Task ${task.id.take(8)}",
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.weight(1f),
                )
                Text(
                    text = task.status.name.lowercase(),
                    style = MaterialTheme.typography.bodySmall,
                    color = statusColor,
                )
            }

            Row(
                modifier = Modifier.padding(top = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                task.model?.let {
                    Text(text = it, style = MaterialTheme.typography.bodySmall)
                }
                task.totalCostUsd?.let { cost ->
                    Text(
                        text = "$${String.format("%.4f", cost)}",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }

            Text(
                text = task.projectPath,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
    }
}
