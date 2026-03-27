package com.zremote.ui.screens.loops

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
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

@Composable
fun LoopDetailScreen(
    loopId: String,
    viewModel: LoopDetailViewModel = hiltViewModel(),
) {
    val loop by viewModel.loop.collectAsStateWithLifecycle()
    val isLoading by viewModel.isLoading.collectAsStateWithLifecycle()

    LaunchedEffect(loopId) {
        viewModel.loadLoop(loopId)
    }

    if (isLoading) {
        Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        return
    }

    val loopData = loop
    if (loopData == null) {
        Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Text("Loop not found")
        }
        return
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(16.dp),
    ) {
        Text(
            text = loopData.taskName ?: loopData.toolName,
            style = MaterialTheme.typography.headlineMedium,
        )

        Card(
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 16.dp),
        ) {
            Column(modifier = Modifier.padding(16.dp)) {
                DetailRow("Status", loopData.status.name.lowercase().replace('_', ' '))
                DetailRow("Tool", loopData.toolName)
                DetailRow("Session", loopData.sessionId.take(8))
                DetailRow("Started", loopData.startedAt)
                loopData.endedAt?.let { DetailRow("Ended", it) }
                loopData.endReason?.let { DetailRow("End reason", it) }
                loopData.projectPath?.let { DetailRow("Project", it) }
            }
        }
    }
}

@Composable
private fun DetailRow(label: String, value: String) {
    Column(modifier = Modifier.padding(vertical = 4.dp)) {
        Text(
            text = label,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = value,
            style = MaterialTheme.typography.bodyLarge,
        )
    }
}
