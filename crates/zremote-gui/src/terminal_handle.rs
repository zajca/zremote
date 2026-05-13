use zremote_client::{TerminalEvent, TerminalInput, TerminalSession};

/// Unified terminal I/O handle supporting both WebSocket and bridge connections.
///
/// Both variants expose the same channel-based API for input via [`InputSender`].
pub enum TerminalHandle {
    WebSocket(TerminalSession),
    /// Direct bridge to agent on the same machine (bypasses server relay).
    Bridge(TerminalSession),
    #[cfg(test)]
    Test {
        input_tx: flume::Sender<TerminalInput>,
        output_rx: flume::Receiver<TerminalEvent>,
        resize_tx: flume::Sender<(u16, u16)>,
        image_paste_tx: Option<flume::Sender<String>>,
    },
}

/// Clonable sender for raw terminal input bytes.
///
/// Wraps a `Sender<TerminalInput>` (WebSocket/bridge, converts to `Data`).
/// Cheap to clone — just an Arc bump.
#[derive(Clone)]
pub struct InputSender {
    tx: flume::Sender<TerminalInput>,
}

impl InputSender {
    pub fn send(&self, data: Vec<u8>) -> Result<(), flume::SendError<Vec<u8>>> {
        self.tx
            .send(TerminalInput::Data(data))
            .map_err(|e| match e.into_inner() {
                TerminalInput::Data(d) => flume::SendError(d),
                _ => flume::SendError(Vec::new()),
            })
    }

    /// Non-blocking send — used by the terminal event listener to avoid
    /// blocking while the term mutex is held (e.g. during DSR responses).
    pub fn try_send(&self, data: Vec<u8>) {
        let result = self
            .tx
            .try_send(TerminalInput::Data(data))
            .err()
            .map(|e| match e {
                flume::TrySendError::Full(_) => "channel full",
                flume::TrySendError::Disconnected(_) => "channel disconnected",
            });
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
            Self::WebSocket(session) | Self::Bridge(session) => InputSender {
                tx: session.input_tx.clone(),
            },
            #[cfg(test)]
            Self::Test { input_tx, .. } => InputSender {
                tx: input_tx.clone(),
            },
        }
    }

    pub fn output_rx(&self) -> &flume::Receiver<TerminalEvent> {
        match self {
            Self::WebSocket(session) | Self::Bridge(session) => &session.output_rx,
            #[cfg(test)]
            Self::Test { output_rx, .. } => output_rx,
        }
    }

    pub fn resize_tx(&self) -> &flume::Sender<(u16, u16)> {
        match self {
            Self::WebSocket(session) | Self::Bridge(session) => &session.resize_tx,
            #[cfg(test)]
            Self::Test { resize_tx, .. } => resize_tx,
        }
    }

    /// Image paste channel (WebSocket only). Bridge mode returns `None`
    /// because Claude Code can read the system clipboard itself on the same machine.
    pub fn image_paste_tx(&self) -> Option<&flume::Sender<String>> {
        match self {
            Self::WebSocket(session) => Some(&session.image_paste_tx),
            Self::Bridge(_) => None,
            #[cfg(test)]
            Self::Test { image_paste_tx, .. } => image_paste_tx.as_ref(),
        }
    }

    pub fn is_bridge(&self) -> bool {
        matches!(self, Self::Bridge(_))
    }
}
