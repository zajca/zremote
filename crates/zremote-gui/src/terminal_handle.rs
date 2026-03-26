use zremote_client::{TerminalEvent, TerminalInput, TerminalSession};

use crate::terminal_direct::DirectTmuxHandle;

/// Unified terminal I/O handle supporting both WebSocket and direct tmux connections.
///
/// Both variants expose the same channel-based API for input via [`InputSender`].
pub enum TerminalHandle {
    WebSocket(TerminalSession),
    Direct(DirectTmuxHandle),
}

/// Clonable sender for raw terminal input bytes.
///
/// Wraps either a `Sender<TerminalInput>` (WebSocket, converts to `Data`) or
/// a `Sender<Vec<u8>>` (direct tmux). Cheap to clone — just an Arc bump.
#[derive(Clone)]
pub enum InputSender {
    WebSocket(flume::Sender<TerminalInput>),
    Direct(flume::Sender<Vec<u8>>),
}

impl InputSender {
    pub fn send(&self, data: Vec<u8>) -> Result<(), flume::SendError<Vec<u8>>> {
        match self {
            Self::WebSocket(tx) => {
                tx.send(TerminalInput::Data(data))
                    .map_err(|e| match e.into_inner() {
                        TerminalInput::Data(d) => flume::SendError(d),
                        _ => flume::SendError(Vec::new()),
                    })
            }
            Self::Direct(tx) => tx.send(data),
        }
    }

    /// Non-blocking send — used by the terminal event listener to avoid
    /// blocking while the term mutex is held (e.g. during DSR responses).
    pub fn try_send(&self, data: Vec<u8>) {
        let result = match self {
            Self::WebSocket(tx) => tx
                .try_send(TerminalInput::Data(data))
                .err()
                .map(|e| match e {
                    flume::TrySendError::Full(_) => "channel full",
                    flume::TrySendError::Disconnected(_) => "channel disconnected",
                }),
            Self::Direct(tx) => tx.try_send(data).err().map(|e| match e {
                flume::TrySendError::Full(_) => "channel full",
                flume::TrySendError::Disconnected(_) => "channel disconnected",
            }),
        };
        if let Some(reason) = result {
            tracing::warn!("PtyWrite response dropped: {reason}");
        }
    }
}

impl TerminalHandle {
    /// Create a WebSocket terminal handle.
    pub fn from_session(session: TerminalSession) -> Self {
        Self::WebSocket(session)
    }

    /// Get a clonable input sender (for use in closures).
    pub fn input_sender(&self) -> InputSender {
        match self {
            Self::WebSocket(session) => InputSender::WebSocket(session.input_tx.clone()),
            Self::Direct(h) => InputSender::Direct(h.input_tx.clone()),
        }
    }

    pub fn output_rx(&self) -> &flume::Receiver<TerminalEvent> {
        match self {
            Self::WebSocket(session) => &session.output_rx,
            Self::Direct(h) => &h.output_rx,
        }
    }

    pub fn resize_tx(&self) -> &flume::Sender<(u16, u16)> {
        match self {
            Self::WebSocket(session) => &session.resize_tx,
            Self::Direct(h) => &h.resize_tx,
        }
    }

    /// Image paste channel (WebSocket only). Direct mode returns `None` because
    /// Claude Code can read the system clipboard itself on the same machine.
    pub fn image_paste_tx(&self) -> Option<&flume::Sender<String>> {
        match self {
            Self::WebSocket(session) => Some(&session.image_paste_tx),
            Self::Direct(_) => None,
        }
    }

    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct(_))
    }
}
