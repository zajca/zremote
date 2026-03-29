package com.zremote.ui.screens.terminal

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.zremote.ui.theme.Background

@Composable
fun TerminalScreen(
    sessionId: String,
    viewModel: TerminalViewModel = hiltViewModel(),
) {
    val lines by viewModel.lines.collectAsStateWithLifecycle()
    val listState = rememberLazyListState()

    LaunchedEffect(sessionId) {
        viewModel.connectToSession(sessionId)
    }

    LaunchedEffect(lines.size) {
        if (lines.isNotEmpty()) {
            listState.animateScrollToItem(lines.size - 1)
        }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        // Terminal output area
        LazyColumn(
            state = listState,
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .background(Background)
                .padding(horizontal = 4.dp),
        ) {
            items(lines) { line ->
                TerminalLineView(line)
            }
        }

        // Quick command bar
        QuickCommandBar(
            onSendInput = { viewModel.sendInput(it) },
            onSendControl = { viewModel.sendControlChar(it) },
        )
    }
}

@Composable
private fun TerminalLineView(line: TerminalLine) {
    val annotated = remember(line) { lineToAnnotatedString(line) }
    Text(
        text = annotated,
        fontFamily = FontFamily.Monospace,
        fontSize = 13.sp,
        lineHeight = 17.sp,
        modifier = Modifier.horizontalScroll(rememberScrollState()),
    )
}

private fun lineToAnnotatedString(line: TerminalLine): AnnotatedString {
    return buildAnnotatedString {
        for (sc in line.chars) {
            pushStyle(
                SpanStyle(
                    color = sc.fg,
                    background = sc.bg,
                    fontWeight = if (sc.bold) FontWeight.Bold else FontWeight.Normal,
                )
            )
            append(sc.char)
            pop()
        }
    }
}

@Composable
private fun QuickCommandBar(
    onSendInput: (String) -> Unit,
    onSendControl: (Char) -> Unit,
) {
    var inputText by remember { mutableStateOf("") }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surface)
            .padding(8.dp),
    ) {
        // Quick action buttons
        Row(modifier = Modifier.fillMaxWidth()) {
            QuickButton("Ctrl+C") { onSendControl('c') }
            QuickButton("Tab") { onSendInput("\t") }
            QuickButton("Esc") { onSendInput("\u001B") }
            QuickButton("Up") { onSendInput("\u001B[A") }
            QuickButton("Down") { onSendInput("\u001B[B") }
        }

        // Text input field
        BasicTextField(
            value = inputText,
            onValueChange = { inputText = it },
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
            keyboardActions = KeyboardActions(
                onSend = {
                    onSendInput(inputText + "\n")
                    inputText = ""
                },
            ),
            textStyle = MaterialTheme.typography.bodyMedium.copy(
                color = MaterialTheme.colorScheme.onSurface,
                fontFamily = FontFamily.Monospace,
            ),
            cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 8.dp)
                .background(
                    MaterialTheme.colorScheme.surfaceVariant,
                    MaterialTheme.shapes.small,
                )
                .padding(12.dp),
        )
    }
}

@Composable
private fun QuickButton(label: String, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        colors = ButtonDefaults.buttonColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant,
            contentColor = MaterialTheme.colorScheme.onSurface,
        ),
        modifier = Modifier.padding(end = 4.dp),
    ) {
        Text(label, fontSize = 11.sp)
    }
}
