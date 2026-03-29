//! Interactive terminal attach — SSH-like raw terminal session.

use std::io::Write;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use zremote_client::{ApiClient, TerminalEvent, TerminalInput};

/// Attach to a terminal session interactively.
///
/// Bridges the local TTY to a remote terminal session via WebSocket.
/// Supports raw mode, terminal resize, and `~.` escape to detach.
pub async fn run_attach(client: &ApiClient, session_id: &str) -> i32 {
    let url = client.terminal_ws_url(session_id);
    let handle = tokio::runtime::Handle::current();

    let session = match zremote_client::TerminalSession::connect(url, &handle).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error connecting to session {session_id}: {e}");
            return 1;
        }
    };

    // Send initial terminal size
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        let _ = session.resize_tx.send((cols, rows));
    }

    // Enable raw mode with RAII guard
    if let Err(e) = crossterm::terminal::enable_raw_mode() {
        eprintln!("Error enabling raw mode: {e}");
        return 1;
    }
    let _raw_guard = RawModeGuard;

    let input_tx = session.input_tx.clone();
    let resize_tx = session.resize_tx.clone();

    // Input task: read crossterm events and forward to WebSocket
    let input_handle = tokio::spawn(async move {
        let mut reader = EventStream::new();
        let mut escape_state = EscapeState::AfterNewline; // start as if after newline

        while let Some(Ok(event)) = reader.next().await {
            match event {
                Event::Key(key_event) => {
                    if key_event.kind != KeyEventKind::Press {
                        continue;
                    }

                    // Check for ~. escape
                    if let Some(action) = escape_state.feed(&key_event) {
                        match action {
                            EscapeAction::Detach => return DetachReason::UserDetach,
                            EscapeAction::Send(bytes) => {
                                let _ = input_tx.send_async(TerminalInput::Data(bytes)).await;
                            }
                            EscapeAction::Consumed => {}
                        }
                        continue;
                    }

                    if let Some(bytes) = key_event_to_bytes(&key_event) {
                        let _ = input_tx.send_async(TerminalInput::Data(bytes)).await;
                    }
                }
                Event::Resize(cols, rows) => {
                    let _ = resize_tx.send_async((cols, rows)).await;
                }
                _ => {}
            }
        }
        DetachReason::InputClosed
    });

    // Output loop: read from WebSocket and write to stdout
    let mut stdout = std::io::stdout();
    let exit_code = loop {
        match session.output_rx.recv_async().await {
            Ok(TerminalEvent::Output(data) | TerminalEvent::PaneOutput { data, .. }) => {
                let _ = stdout.write_all(&data);
                let _ = stdout.flush();
            }
            Ok(TerminalEvent::SessionClosed { exit_code }) => {
                break exit_code.unwrap_or(0);
            }
            Ok(TerminalEvent::Disconnected) | Err(_) => {
                break 1;
            }
            Ok(TerminalEvent::SessionSuspended) => {
                let msg = b"\r\n[session suspended]\r\n";
                let _ = stdout.write_all(msg);
                let _ = stdout.flush();
            }
            Ok(TerminalEvent::SessionResumed) => {
                let msg = b"\r\n[session resumed]\r\n";
                let _ = stdout.write_all(msg);
                let _ = stdout.flush();
            }
            Ok(TerminalEvent::Error { message }) => {
                let msg = format!("\r\n[error: {message}]\r\n");
                let _ = stdout.write_all(msg.as_bytes());
                let _ = stdout.flush();
            }
            Ok(_) => {} // Scrollback, pane events — ignore in CLI
        }
    };

    // Cancel input task
    input_handle.abort();

    // Check if input task requested detach
    if let Ok(DetachReason::UserDetach) = input_handle.await {
        let _ = stdout.write_all(b"\r\n[detached]\r\n");
        let _ = stdout.flush();
        return 0;
    }

    exit_code
}

/// RAII guard to restore terminal mode on drop.
struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Why the input task exited.
enum DetachReason {
    UserDetach,
    InputClosed,
}

// --- Escape sequence (~.) detection ---

/// State machine for detecting the `~.` escape sequence.
///
/// After a newline (Enter), `~.` detaches and `~~` sends a literal `~`.
enum EscapeState {
    Normal,
    AfterNewline,
    AfterTilde,
}

/// Action to take after escape state processing.
#[derive(Debug)]
enum EscapeAction {
    /// Detach from the session (user typed `~.`).
    Detach,
    /// Send buffered bytes (tilde resolved as non-escape).
    Send(Vec<u8>),
    /// Key was consumed (buffered), do not send anything yet.
    Consumed,
}

impl EscapeState {
    /// Feed a key event into the state machine.
    /// Returns `Some(action)` if the key was consumed by escape processing.
    /// Returns `None` if the key should be sent normally.
    fn feed(&mut self, key: &KeyEvent) -> Option<EscapeAction> {
        match (&self, key.code) {
            // Track newlines
            (_, KeyCode::Enter) => {
                *self = EscapeState::AfterNewline;
                None // Let Enter pass through
            }
            // After newline, ~ starts escape — buffer it, don't send yet
            (EscapeState::AfterNewline, KeyCode::Char('~')) => {
                *self = EscapeState::AfterTilde;
                Some(EscapeAction::Consumed)
            }
            // After ~, . means detach
            (EscapeState::AfterTilde, KeyCode::Char('.')) => {
                *self = EscapeState::Normal;
                Some(EscapeAction::Detach)
            }
            // After ~, another ~ sends one literal ~ and stays in AfterTilde
            (EscapeState::AfterTilde, KeyCode::Char('~')) => {
                // Stay in AfterTilde so ~~~ sends ~~ etc.
                Some(EscapeAction::Send(b"~".to_vec()))
            }
            // After ~, any other key: flush buffered ~ + the key
            (EscapeState::AfterTilde, _) => {
                *self = EscapeState::Normal;
                key_event_to_bytes(key).map(|key_bytes| {
                    let mut buf = b"~".to_vec();
                    buf.extend_from_slice(&key_bytes);
                    EscapeAction::Send(buf)
                })
            }
            // Normal state: just reset if not newline/tilde
            _ => {
                *self = EscapeState::Normal;
                None
            }
        }
    }
}

// Note: In AfterTilde state, we already sent the tilde in the SendTilde action
// so we suppress it here by returning Some. The actual ~ was sent as part of
// the escape action to avoid delay/buffering.

/// Convert a crossterm key event to PTY-compatible bytes.
fn key_event_to_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Ctrl+A..Z → 0x01..0x1A
                let byte = c.to_ascii_lowercase() as u8;
                if byte.is_ascii_lowercase() {
                    let ctrl_byte = byte - b'a' + 1;
                    return Some(if alt {
                        vec![0x1b, ctrl_byte]
                    } else {
                        vec![ctrl_byte]
                    });
                }
            }
            let mut buf = [0u8; 4];
            let bytes = c.encode_utf8(&mut buf).as_bytes().to_vec();
            Some(if alt {
                let mut v = vec![0x1b];
                v.extend_from_slice(&bytes);
                v
            } else {
                bytes
            })
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(csi_arrow(b'A', key.modifiers)),
        KeyCode::Down => Some(csi_arrow(b'B', key.modifiers)),
        KeyCode::Right => Some(csi_arrow(b'C', key.modifiers)),
        KeyCode::Left => Some(csi_arrow(b'D', key.modifiers)),
        KeyCode::Home => Some(csi_tilde(1, key.modifiers)),
        KeyCode::End => Some(csi_tilde(4, key.modifiers)),
        KeyCode::Insert => Some(csi_tilde(2, key.modifiers)),
        KeyCode::Delete => Some(csi_tilde(3, key.modifiers)),
        KeyCode::PageUp => Some(csi_tilde(5, key.modifiers)),
        KeyCode::PageDown => Some(csi_tilde(6, key.modifiers)),
        KeyCode::F(n) => Some(f_key_sequence(n, key.modifiers)),
        _ => None,
    }
}

/// CSI sequence for arrow keys, with modifier support.
fn csi_arrow(letter: u8, mods: KeyModifiers) -> Vec<u8> {
    let modifier = csi_modifier(mods);
    if modifier == 1 {
        vec![0x1b, b'[', letter]
    } else {
        format!("\x1b[1;{modifier}{}", letter as char).into_bytes()
    }
}

/// CSI sequence for tilde-terminated keys (Home, End, Insert, Delete, `PageUp`, `PageDown`).
fn csi_tilde(num: u8, mods: KeyModifiers) -> Vec<u8> {
    let modifier = csi_modifier(mods);
    if modifier == 1 {
        format!("\x1b[{num}~").into_bytes()
    } else {
        format!("\x1b[{num};{modifier}~").into_bytes()
    }
}

/// CSI modifier parameter (xterm-style).
fn csi_modifier(mods: KeyModifiers) -> u8 {
    let mut m: u8 = 1;
    if mods.contains(KeyModifiers::SHIFT) {
        m += 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        m += 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        m += 4;
    }
    m
}

/// F-key escape sequences.
fn f_key_sequence(n: u8, mods: KeyModifiers) -> Vec<u8> {
    let modifier = csi_modifier(mods);
    let num = match n {
        1 => {
            return if modifier == 1 {
                b"\x1bOP".to_vec()
            } else {
                format!("\x1b[1;{modifier}P").into_bytes()
            };
        }
        2 => {
            return if modifier == 1 {
                b"\x1bOQ".to_vec()
            } else {
                format!("\x1b[1;{modifier}Q").into_bytes()
            };
        }
        3 => {
            return if modifier == 1 {
                b"\x1bOR".to_vec()
            } else {
                format!("\x1b[1;{modifier}R").into_bytes()
            };
        }
        4 => {
            return if modifier == 1 {
                b"\x1bOS".to_vec()
            } else {
                format!("\x1b[1;{modifier}S").into_bytes()
            };
        }
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return vec![],
    };
    if modifier == 1 {
        format!("\x1b[{num}~").into_bytes()
    } else {
        format!("\x1b[{num};{modifier}~").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn press_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn escape_detach_after_newline() {
        let mut state = EscapeState::Normal;
        assert!(state.feed(&press(KeyCode::Enter)).is_none());
        assert!(matches!(
            state.feed(&press(KeyCode::Char('~'))),
            Some(EscapeAction::Consumed)
        ));
        assert!(matches!(
            state.feed(&press(KeyCode::Char('.'))),
            Some(EscapeAction::Detach)
        ));
    }

    #[test]
    fn escape_detach_at_start() {
        // Initial state is AfterNewline
        let mut state = EscapeState::AfterNewline;
        assert!(matches!(
            state.feed(&press(KeyCode::Char('~'))),
            Some(EscapeAction::Consumed)
        ));
        assert!(matches!(
            state.feed(&press(KeyCode::Char('.'))),
            Some(EscapeAction::Detach)
        ));
    }

    #[test]
    fn escape_double_tilde_sends_literal() {
        let mut state = EscapeState::AfterNewline;
        assert!(matches!(
            state.feed(&press(KeyCode::Char('~'))),
            Some(EscapeAction::Consumed)
        ));
        assert!(matches!(
            state.feed(&press(KeyCode::Char('~'))),
            Some(EscapeAction::Send(_))
        ));
    }

    #[test]
    fn escape_tilde_then_char_sends_both() {
        let mut state = EscapeState::AfterNewline;
        assert!(matches!(
            state.feed(&press(KeyCode::Char('~'))),
            Some(EscapeAction::Consumed)
        ));
        match state.feed(&press(KeyCode::Char('a'))) {
            Some(EscapeAction::Send(bytes)) => assert_eq!(bytes, b"~a"),
            other => panic!("expected Send(~a), got {other:?}"),
        }
    }

    #[test]
    fn escape_no_false_positive() {
        let mut state = EscapeState::Normal;
        // ~ without preceding newline should pass through
        assert!(state.feed(&press(KeyCode::Char('a'))).is_none());
        assert!(state.feed(&press(KeyCode::Char('~'))).is_none());
    }

    #[test]
    fn key_plain_char() {
        assert_eq!(
            key_event_to_bytes(&press(KeyCode::Char('a'))),
            Some(vec![b'a'])
        );
    }

    #[test]
    fn key_ctrl_c() {
        assert_eq!(
            key_event_to_bytes(&press_mod(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
    }

    #[test]
    fn key_alt_x() {
        assert_eq!(
            key_event_to_bytes(&press_mod(KeyCode::Char('x'), KeyModifiers::ALT)),
            Some(vec![0x1b, b'x'])
        );
    }

    #[test]
    fn key_enter() {
        assert_eq!(
            key_event_to_bytes(&press(KeyCode::Enter)),
            Some(vec![b'\r'])
        );
    }

    #[test]
    fn key_arrow_up() {
        assert_eq!(
            key_event_to_bytes(&press(KeyCode::Up)),
            Some(vec![0x1b, b'[', b'A'])
        );
    }

    #[test]
    fn key_ctrl_arrow() {
        assert_eq!(
            key_event_to_bytes(&press_mod(KeyCode::Right, KeyModifiers::CONTROL)),
            Some(b"\x1b[1;5C".to_vec())
        );
    }

    #[test]
    fn key_f1() {
        assert_eq!(
            key_event_to_bytes(&press(KeyCode::F(1))),
            Some(b"\x1bOP".to_vec())
        );
    }

    #[test]
    fn key_f5() {
        assert_eq!(
            key_event_to_bytes(&press(KeyCode::F(5))),
            Some(b"\x1b[15~".to_vec())
        );
    }

    #[test]
    fn key_backspace() {
        assert_eq!(
            key_event_to_bytes(&press(KeyCode::Backspace)),
            Some(vec![0x7f])
        );
    }

    #[test]
    fn key_delete() {
        assert_eq!(
            key_event_to_bytes(&press(KeyCode::Delete)),
            Some(b"\x1b[3~".to_vec())
        );
    }

    #[test]
    fn csi_modifier_computation() {
        assert_eq!(csi_modifier(KeyModifiers::NONE), 1);
        assert_eq!(csi_modifier(KeyModifiers::SHIFT), 2);
        assert_eq!(csi_modifier(KeyModifiers::ALT), 3);
        assert_eq!(csi_modifier(KeyModifiers::CONTROL), 5);
        assert_eq!(csi_modifier(KeyModifiers::SHIFT | KeyModifiers::ALT), 4);
    }
}
