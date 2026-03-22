# RFC: Direct Tmux via Control Mode

## Problem

The GPUI terminal client currently uses WebSocket relay for all terminal I/O (GUI → server → agent → tmux). When the tmux session runs on the same machine as the GUI, this adds unnecessary latency. The previous approach (pipe-pane takeover) failed because it disrupted the agent's session monitoring.

## Solution

Use **tmux control mode** (`tmux -C attach-session`) to attach as a second client. Control mode coexists with the agent's pipe-pane -- both channels receive data simultaneously without interference.

## Protocol Summary

Control mode is line-based on stdin/stdout:

- **Output**: `%output %PANE_ID value` (octal-escaped non-printable bytes, e.g. `\033` for ESC)
- **Input**: `send-keys -t %PANE_ID -H hex_bytes` via stdin
- **Resize**: `refresh-client -C WxH` via stdin
- **Initial content**: `capture-pane -t %PANE_ID -p -e` (raw ANSI, no octal encoding)
- **Session close**: `%exit` notification
- **Command responses**: Wrapped in `%begin TIMESTAMP CMD_NUM FLAGS` / `%end TIMESTAMP CMD_NUM FLAGS`

Octal decoding: scan for `\` + 3 octal digits (`[0-7]{3}`), convert to byte. All other bytes pass through.

## Architecture

```
Current (WebSocket):
  GUI  →ws→  Server  →ws→  Agent  →fifo→  tmux pane

Direct (Control Mode):
  GUI  →stdin/stdout→  tmux -C attach-session
                       ↓ (coexists with)
  Agent →fifo (pipe-pane)→  tmux pane
```

Both paths receive the same output simultaneously. No coordination needed.

## Implementation

### File: `crates/zremote-gui/src/terminal_direct.rs`

Replace the entire pipe-pane implementation with control mode. The module becomes ~200 lines simpler.

#### Data structures

```rust
pub struct DirectTmuxHandle {
    pub input_tx: flume::Sender<Vec<u8>>,
    pub output_rx: flume::Receiver<TerminalEvent>,
    pub resize_tx: flume::Sender<(u16, u16)>,
    _shutdown_tx: flume::Sender<()>,
}
```

Same public interface as before -- `TerminalHandle` enum works unchanged.

#### `connect_standalone(session_id, pane_id, tokio_handle) -> Result<DirectTmuxHandle>`

1. Spawn child process: `tmux -L zremote -C attach-session -t zremote-{session_id}`
   - `stdin`: piped
   - `stdout`: piped
   - `stderr`: null
2. Send initial commands via stdin:
   - `capture-pane -t %{pane_id} -p -e` (get current screen content)
3. Spawn 3 tokio tasks:

**Reader task** (stdout → output_rx):
- Read stdout line by line with `BufReader`
- Parse `%output %PANE_ID value` lines → decode octal escapes → `TerminalEvent::Output(bytes)`
- Parse `%begin`/`%end` blocks → for `capture-pane` response, collect lines and send as `TerminalEvent::Output`
- Parse `%exit` → `TerminalEvent::SessionClosed`
- Ignore other notifications (`%layout-change`, `%session-changed`, etc.)

**Writer task** (input_tx → stdin):
- Receive `Vec<u8>` from input_tx
- Batch pending input (drain channel)
- Format as `send-keys -t %{pane_id} -H {hex_bytes}\n`
- Write to stdin

**Resize task** (resize_tx → stdin):
- Receive `(cols, rows)` from resize_tx
- Format as `refresh-client -C {cols}x{rows}\n`
- Write to stdin

**Cleanup** (shutdown_rx):
- On shutdown signal, write `detach-client\n` to stdin
- Kill child process
- No pipe-pane restore needed (we never touched it)

#### `probe_local_session(session_id) -> Option<String>`

Unchanged -- runs `tmux -L zremote list-panes -t zremote-{session_id} -F '#{pane_id}'`.

#### `tmux_available() -> bool`

Unchanged.

#### Octal decode function

```rust
fn decode_tmux_octal(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len()
            && bytes[i+1].is_ascii_digit() // not strictly [0-7] but fine
            && bytes[i+2].is_ascii_digit()
            && bytes[i+3].is_ascii_digit()
        {
            let val = (bytes[i+1] - b'0') * 64
                    + (bytes[i+2] - b'0') * 8
                    + (bytes[i+3] - b'0');
            result.push(val);
            i += 4;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    result
}
```

### File: `crates/zremote-gui/src/views/main_view.rs`

#### `connect_terminal()` → re-enable direct connections

```rust
fn connect_terminal(app_state, session_id) -> TerminalHandle {
    // Probe local tmux socket
    if terminal_direct::tmux_available() {
        if let Some(pane_id) = terminal_direct::probe_local_session(session_id) {
            match terminal_direct::connect_standalone(
                session_id.to_string(), pane_id, &app_state.tokio_handle
            ) {
                Ok(direct) => return TerminalHandle::Direct(direct),
                Err(e) => tracing::warn!(error = %e, "direct tmux failed, using WS"),
            }
        }
    }
    // Fallback to WebSocket
    let ws_url = app_state.api.terminal_ws_url(session_id);
    TerminalHandle::WebSocket(terminal_ws::connect(ws_url, &app_state.tokio_handle))
}
```

### Files NOT changed

- `terminal_handle.rs` -- `TerminalHandle` enum unchanged, `is_direct()` works
- `terminal_panel.rs` -- connection indicator unchanged
- `api.rs` -- no direct-attach/detach API needed
- Agent code -- zero changes, pipe-pane untouched

## Removed code

- `TmuxConnectionInfo` struct
- `connect()` (old pipe-pane version)
- `setup_pipe_pane()`, `setup_tee_pipe_pane()`
- `create_fifo()`, `agent_fifo_path()`, `fifo_dir()`, `current_uid()`
- All FIFO-related logic

## Edge cases

1. **tmux session dies while connected**: Reader gets `%exit` → sends `TerminalEvent::SessionClosed`
2. **GUI crashes**: Child process (tmux control client) gets SIGPIPE on stdin write → exits. Agent unaffected.
3. **Agent restarts**: Pipe-pane is re-established by agent. Control mode client continues to receive output independently.
4. **Binary output**: ~4x expansion for non-printable bytes (octal encoding). Acceptable for terminal use.
5. **Resize race**: `refresh-client -C` changes pane size. Agent's pipe-pane output will be formatted for new size. Same behavior as WebSocket relay.

## Testing

1. `cargo check -p zremote-gui` + `cargo clippy -p zremote-gui`
2. Run GUI with `RUST_LOG=debug`, click sessions -- verify "direct tmux connection established" in logs
3. Switch between sessions -- verify no session crashes, no agent disruption
4. Type in terminal -- verify keystrokes work
5. Resize window -- verify terminal reflows
6. Close session -- verify clean disconnect
7. Check agent logs -- verify no errors or pipe-pane disruption
