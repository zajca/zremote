package com.zremote.ui.screens.projects

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Card
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.zremote.sdk.FfiProject
import com.zremote.ui.components.EmptyState
import com.zremote.ui.components.ErrorState
import com.zremote.ui.components.LoadingState
import com.zremote.ui.components.RefreshableList
import com.zremote.ui.theme.StatusError
import com.zremote.ui.theme.StatusOnline
import com.zremote.ui.theme.StatusWorking

@Composable
fun ProjectListScreen(
    hostId: String,
    viewModel: ProjectListViewModel = hiltViewModel(),
) {
    val projects by viewModel.projects.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()
    val error by viewModel.error.collectAsStateWithLifecycle()

    LaunchedEffect(hostId) {
        viewModel.loadProjects(hostId)
    }

    val currentError = error
    when {
        isLoading && projects.isEmpty() -> LoadingState()
        currentError != null && projects.isEmpty() -> ErrorState(
            message = currentError,
            onRetry = { viewModel.refresh() },
        )
        projects.isEmpty() && !isLoading -> EmptyState(
            icon = Icons.Default.Folder,
            message = "No projects",
            hint = "Tap refresh to scan for projects on this host",
        )
        else -> RefreshableList(
            isRefreshing = isLoading,
            onRefresh = { viewModel.refresh() },
        ) {
            items(projects, key = { it.id }) { project ->
                ProjectCard(
                    project = project,
                    onGitRefresh = { viewModel.triggerGitRefresh(project.id) },
                )
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ProjectCard(project: FfiProject, onGitRefresh: () -> Unit) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = project.name,
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = onGitRefresh, modifier = Modifier.size(32.dp)) {
                    Icon(
                        Icons.Default.Refresh,
                        contentDescription = "Refresh git status",
                        modifier = Modifier.size(18.dp),
                    )
                }
            }

            Text(
                text = project.path,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            project.gitBranch?.let { branch ->
                Row(
                    modifier = Modifier.padding(top = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = branch,
                        style = MaterialTheme.typography.bodyMedium,
                        color = StatusWorking,
                    )
                    if (project.gitDirty) {
                        Text(
                            text = "dirty",
                            style = MaterialTheme.typography.labelSmall,
                            color = StatusError,
                        )
                    }
                    project.gitAheadBehind?.let { ab ->
                        Text(
                            text = ab,
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }

            FlowRow(
                modifier = Modifier.padding(top = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (project.hasClaudeConfig) {
                    AssistChip(
                        onClick = {},
                        label = { Text("Claude", style = MaterialTheme.typography.labelSmall) },
                    )
                }
                if (project.hasZremoteConfig) {
                    AssistChip(
                        onClick = {},
                        label = { Text("ZRemote", style = MaterialTheme.typography.labelSmall) },
                    )
                }
            }
        }
    }
}
