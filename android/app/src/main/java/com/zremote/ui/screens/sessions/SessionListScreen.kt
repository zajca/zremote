package com.zremote.ui.screens.sessions

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
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.zremote.sdk.FfiSession
import com.zremote.ui.theme.StatusCompleted
import com.zremote.ui.theme.StatusOffline
import com.zremote.ui.theme.StatusOnline

@Composable
fun SessionListScreen(
    hostId: String,
    viewModel: SessionListViewModel = hiltViewModel(),
) {
    val sessions by viewModel.sessions.collectAsStateWithLifecycle()

    LaunchedEffect(hostId) {
        viewModel.loadSessions(hostId)
    }

    LazyColumn(modifier = Modifier.fillMaxSize()) {
        items(sessions, key = { it.id }) { session ->
            SessionCard(session)
        }
    }
}

@Composable
private fun SessionCard(session: FfiSession) {
    val statusColor = when (session.status) {
        "active" -> StatusOnline
        "closed" -> StatusOffline
        "suspended" -> StatusCompleted
        else -> StatusOffline
    }

    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp),
    ) {
        Row(
            modifier = Modifier.padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Surface(
                shape = CircleShape,
                color = statusColor,
                modifier = Modifier.size(10.dp),
            ) {}

            Spacer(Modifier.width(12.dp))

            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = session.name ?: "Session ${session.id.take(8)}",
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    text = "${session.status} | ${session.shell ?: "unknown shell"}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
