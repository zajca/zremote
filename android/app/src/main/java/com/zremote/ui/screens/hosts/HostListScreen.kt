package com.zremote.ui.screens.hosts

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.Folder
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
import com.zremote.sdk.FfiHost
import com.zremote.ui.components.EmptyState
import com.zremote.ui.components.ErrorState
import com.zremote.ui.components.LoadingState
import com.zremote.ui.components.RefreshableList
import com.zremote.ui.components.StatusDot
import com.zremote.ui.theme.StatusOffline
import com.zremote.ui.theme.StatusOnline

@Composable
fun HostListScreen(
    onHostClick: (String) -> Unit,
    onProjectsClick: (String) -> Unit = {},
    viewModel: HostListViewModel = hiltViewModel(),
) {
    val hosts by viewModel.hosts.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()
    val error by viewModel.error.collectAsStateWithLifecycle()
    val isConnected by viewModel.isConnected.collectAsStateWithLifecycle()

    LaunchedEffect(isConnected) {
        if (isConnected) {
            viewModel.refresh()
        }
    }

    if (!isConnected) {
        EmptyState(
            icon = Icons.Default.CloudOff,
            message = "Not connected",
            hint = "Configure server URL in Settings",
        )
        return
    }

    val currentError = error
    when {
        isLoading && hosts.isEmpty() -> LoadingState()
        currentError != null && hosts.isEmpty() -> ErrorState(
            message = currentError,
            onRetry = { viewModel.refresh() },
        )
        hosts.isEmpty() && !isLoading -> EmptyState(
            icon = Icons.Default.Dns,
            message = "No hosts found",
            hint = "Hosts will appear when agents connect",
        )
        else -> RefreshableList(
            isRefreshing = isLoading,
            onRefresh = { viewModel.refresh() },
        ) {
            items(hosts, key = { it.id }) { host ->
                HostCard(
                    host = host,
                    onClick = { onHostClick(host.id) },
                    onProjectsClick = { onProjectsClick(host.id) },
                )
            }
        }
    }
}

@Composable
private fun HostCard(host: FfiHost, onClick: () -> Unit, onProjectsClick: () -> Unit) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp)
            .clickable(onClick = onClick),
    ) {
        Row(
            modifier = Modifier.padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            StatusDot(color = if (host.status == "online") StatusOnline else StatusOffline)

            Spacer(Modifier.width(12.dp))

            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = host.hostname,
                    style = MaterialTheme.typography.titleMedium,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    host.os?.let { os ->
                        Text(text = os, style = MaterialTheme.typography.bodySmall)
                    }
                    host.agentVersion?.let { version ->
                        Text(text = "v$version", style = MaterialTheme.typography.bodySmall)
                    }
                }
            }

            IconButton(onClick = onProjectsClick, modifier = Modifier.size(32.dp)) {
                Icon(
                    Icons.Default.Folder,
                    contentDescription = "Projects",
                    modifier = Modifier.size(20.dp),
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
