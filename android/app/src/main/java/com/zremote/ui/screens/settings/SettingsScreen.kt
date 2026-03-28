package com.zremote.ui.screens.settings

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.zremote.ui.theme.StatusError
import com.zremote.ui.theme.StatusOnline

@Composable
fun SettingsScreen(viewModel: SettingsViewModel = hiltViewModel()) {
    val serverUrl by viewModel.serverUrl.collectAsStateWithLifecycle()
    val isConnected by viewModel.isConnected.collectAsStateWithLifecycle()
    val connectionError by viewModel.connectionError.collectAsStateWithLifecycle()

    val notifyLoopCompletions by viewModel.notifyLoopCompletions.collectAsStateWithLifecycle()
    val notifyLoopErrors by viewModel.notifyLoopErrors.collectAsStateWithLifecycle()
    val notifyPermissionRequests by viewModel.notifyPermissionRequests.collectAsStateWithLifecycle()
    val notifyTaskCompletions by viewModel.notifyTaskCompletions.collectAsStateWithLifecycle()
    val notifyTaskErrors by viewModel.notifyTaskErrors.collectAsStateWithLifecycle()
    val notifyHostDisconnections by viewModel.notifyHostDisconnections.collectAsStateWithLifecycle()

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(16.dp)
            .verticalScroll(rememberScrollState()),
    ) {
        Text(
            text = "Server Connection",
            style = MaterialTheme.typography.headlineMedium,
        )

        Spacer(Modifier.height(16.dp))

        OutlinedTextField(
            value = serverUrl,
            onValueChange = { viewModel.updateServerUrl(it) },
            label = { Text("Server URL") },
            placeholder = { Text("http://localhost:3000") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )

        Spacer(Modifier.height(8.dp))

        if (isConnected) {
            Text(
                text = "Connected",
                color = StatusOnline,
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        connectionError?.let { error ->
            Text(
                text = error,
                color = StatusError,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(top = 4.dp),
            )
        }

        Spacer(Modifier.height(16.dp))

        if (isConnected) {
            OutlinedButton(
                onClick = { viewModel.disconnect() },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Disconnect")
            }
        } else {
            Button(
                onClick = { viewModel.connect() },
                enabled = serverUrl.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Connect")
            }
        }

        Spacer(Modifier.height(32.dp))
        HorizontalDivider()
        Spacer(Modifier.height(16.dp))

        Text(
            text = "Notifications",
            style = MaterialTheme.typography.headlineMedium,
        )

        Spacer(Modifier.height(8.dp))

        Text(
            text = "Choose which events trigger notifications when the app is in the background.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(16.dp))

        NotificationToggle(
            label = "Loop completions",
            checked = notifyLoopCompletions,
            onCheckedChange = { viewModel.setNotifyLoopCompletions(it) },
        )
        NotificationToggle(
            label = "Loop errors",
            checked = notifyLoopErrors,
            onCheckedChange = { viewModel.setNotifyLoopErrors(it) },
        )
        NotificationToggle(
            label = "Permission requests",
            checked = notifyPermissionRequests,
            onCheckedChange = { viewModel.setNotifyPermissionRequests(it) },
        )
        NotificationToggle(
            label = "Task completions",
            checked = notifyTaskCompletions,
            onCheckedChange = { viewModel.setNotifyTaskCompletions(it) },
        )
        NotificationToggle(
            label = "Task errors",
            checked = notifyTaskErrors,
            onCheckedChange = { viewModel.setNotifyTaskErrors(it) },
        )
        NotificationToggle(
            label = "Host disconnections",
            checked = notifyHostDisconnections,
            onCheckedChange = { viewModel.setNotifyHostDisconnections(it) },
        )

        Spacer(Modifier.height(16.dp))
    }
}

@Composable
private fun NotificationToggle(
    label: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f),
        )
        Switch(
            checked = checked,
            onCheckedChange = onCheckedChange,
        )
    }
}
