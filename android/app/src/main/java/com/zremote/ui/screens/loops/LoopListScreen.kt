package com.zremote.ui.screens.loops

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Loop
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.zremote.sdk.FfiAgenticLoop
import com.zremote.sdk.FfiAgenticStatus
import com.zremote.ui.components.EmptyState
import com.zremote.ui.components.ErrorState
import com.zremote.ui.components.LoadingState
import com.zremote.ui.components.RefreshableList
import com.zremote.ui.theme.StatusCompleted
import com.zremote.ui.theme.StatusError
import com.zremote.ui.theme.StatusOffline
import com.zremote.ui.theme.StatusWaitingForInput
import com.zremote.ui.theme.StatusWorking

@Composable
fun LoopListScreen(
    onLoopClick: (String) -> Unit,
    viewModel: LoopListViewModel = hiltViewModel(),
) {
    val loops by viewModel.loops.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()
    val error by viewModel.error.collectAsStateWithLifecycle()

    val currentError = error
    when {
        isLoading && loops.isEmpty() -> LoadingState()
        currentError != null && loops.isEmpty() -> ErrorState(
            message = currentError,
            onRetry = { viewModel.refresh() },
        )
        loops.isEmpty() && !isLoading -> EmptyState(
            icon = Icons.Default.Loop,
            message = "No agentic loops",
            hint = "Loops will appear when agents are running",
        )
        else -> RefreshableList(
            isRefreshing = isLoading,
            onRefresh = { viewModel.refresh() },
        ) {
            items(loops, key = { it.id }) { loop ->
                LoopCard(loop = loop, onClick = { onLoopClick(loop.id) })
            }
        }
    }
}

@Composable
private fun LoopCard(loop: FfiAgenticLoop, onClick: () -> Unit) {
    val statusColor = when (loop.status) {
        FfiAgenticStatus.WORKING -> StatusWorking
        FfiAgenticStatus.WAITING_FOR_INPUT -> StatusWaitingForInput
        FfiAgenticStatus.ERROR -> StatusError
        FfiAgenticStatus.COMPLETED -> StatusCompleted
        FfiAgenticStatus.UNKNOWN -> StatusOffline
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
                    text = loop.taskName ?: loop.toolName,
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    text = loop.status.name.lowercase().replace('_', ' '),
                    style = MaterialTheme.typography.bodySmall,
                    color = statusColor,
                )
            }

            loop.projectPath?.let { path ->
                Text(
                    text = path,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }

            Text(
                text = "Started: ${loop.startedAt}",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
    }
}
