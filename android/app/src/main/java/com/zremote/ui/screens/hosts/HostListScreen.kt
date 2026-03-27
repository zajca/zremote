package com.zremote.ui.screens.hosts

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CloudOff
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.zremote.sdk.FfiHost
import com.zremote.ui.theme.StatusOffline
import com.zremote.ui.theme.StatusOnline

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HostListScreen(
    onHostClick: (String) -> Unit,
    viewModel: HostListViewModel = hiltViewModel(),
) {
    val hosts by viewModel.hosts.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()
    val error by viewModel.error.collectAsStateWithLifecycle()
    val isConnected by viewModel.isConnected.collectAsStateWithLifecycle()

    if (!isConnected) {
        NotConnectedState()
        return
    }

    PullToRefreshBox(
        isRefreshing = isLoading,
        onRefresh = { viewModel.refresh() },
        modifier = Modifier.fillMaxSize(),
    ) {
        if (hosts.isEmpty() && !isLoading) {
            EmptyState(error = error)
        } else {
            LazyColumn(modifier = Modifier.fillMaxSize()) {
                items(hosts, key = { it.id }) { host ->
                    HostCard(host = host, onClick = { onHostClick(host.id) })
                }
            }
        }
    }
}

@Composable
private fun HostCard(host: FfiHost, onClick: () -> Unit) {
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
            Surface(
                shape = CircleShape,
                color = if (host.status == "online") StatusOnline else StatusOffline,
                modifier = Modifier.size(10.dp),
            ) {}

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
        }
    }
}

@Composable
private fun NotConnectedState() {
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Icon(
                Icons.Default.CloudOff,
                contentDescription = null,
                modifier = Modifier.size(48.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                text = "Not connected",
                style = MaterialTheme.typography.bodyLarge,
                modifier = Modifier.padding(top = 8.dp),
            )
            Text(
                text = "Configure server URL in Settings",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun EmptyState(error: String?) {
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        if (error != null) {
            Text(text = error, color = MaterialTheme.colorScheme.error)
        } else {
            Text(text = "No hosts found", style = MaterialTheme.typography.bodyLarge)
        }
    }
}
