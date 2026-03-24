use zremote_client::{TerminalEvent, TerminalInput, TerminalSession};

use crate::terminal_direct::DirectTmuxHandle;

/// Unified terminal I/O handle supporting both WebSocket and direct tmux connections.
///
/// Both variants expose the same channel-based API (`flume::Sender<Vec<u8>>` for input).
/// For WebSocket, a bridge task converts `Vec<u8>` → `TerminalInput::Data`.
pub enum TerminalHandle {
    WebSocket {
        session: TerminalSession,
        /// Bridge sender: accepts raw bytes, forwarded as `TerminalInput::Data`.
        input_tx: flume::Sender<Vec<u8>>,
    },
    Direct(DirectTmuxHandle),
}

impl TerminalHandle {
    /// Create a WebSocket terminal handle with a bridge channel for raw byte input.
    pub fn from_session(session: TerminalSession, tokio_handle: &tokio::runtime::Handle) -> Self {
        let (bridge_tx, bridge_rx) = flume::bounded::<Vec<u8>>(256);
        let sdk_input_tx = session.input_tx.clone();
        tokio_handle.spawn(async move {
            while let Ok(data) = bridge_rx.recv_async().await {
                if sdk_input_tx.send(TerminalInput::Data(data)).is_err() {
                    break;
                }
            }
        });
        Self::WebSocket {
            session,
            input_tx: bridge_tx,
        }
    }

    pub fn input_tx(&self) -> &flume::Sender<Vec<u8>> {
        match self {
            Self::WebSocket { input_tx, .. } => input_tx,
            Self::Direct(h) => &h.input_tx,
        }
    }

    pub fn output_rx(&self) -> &flume::Receiver<TerminalEvent> {
        match self {
            Self::WebSocket { session, .. } => &session.output_rx,
            Self::Direct(h) => &h.output_rx,
        }
    }

    pub fn resize_tx(&self) -> &flume::Sender<(u16, u16)> {
        match self {
            Self::WebSocket { session, .. } => &session.resize_tx,
            Self::Direct(h) => &h.resize_tx,
        }
    }

    /// Image paste channel (WebSocket only). Direct mode returns `None` because
    /// Claude Code can read the system clipboard itself on the same machine.
    pub fn image_paste_tx(&self) -> Option<&flume::Sender<String>> {
        match self {
            Self::WebSocket { session, .. } => Some(&session.image_paste_tx),
            Self::Direct(_) => None,
        }
    }

    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct(_))
    }
}
