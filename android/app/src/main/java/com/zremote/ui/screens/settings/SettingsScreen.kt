package com.zremote.ui.screens.settings

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
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

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(16.dp),
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
    }
}
