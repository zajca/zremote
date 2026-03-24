use crate::terminal_direct::DirectTmuxHandle;
use crate::terminal_ws::TerminalWsHandle;
use crate::types::TerminalEvent;

/// Unified terminal I/O handle supporting both WebSocket and direct tmux connections.
pub enum TerminalHandle {
    WebSocket(TerminalWsHandle),
    Direct(DirectTmuxHandle),
}

impl TerminalHandle {
    pub fn input_tx(&self) -> &flume::Sender<Vec<u8>> {
        match self {
            Self::WebSocket(h) => &h.input_tx,
            Self::Direct(h) => &h.input_tx,
        }
    }

    pub fn output_rx(&self) -> &flume::Receiver<TerminalEvent> {
        match self {
            Self::WebSocket(h) => &h.output_rx,
            Self::Direct(h) => &h.output_rx,
        }
    }

    pub fn resize_tx(&self) -> &flume::Sender<(u16, u16)> {
        match self {
            Self::WebSocket(h) => &h.resize_tx,
            Self::Direct(h) => &h.resize_tx,
        }
    }

    /// Image paste channel (WebSocket only). Direct mode returns `None` because
    /// Claude Code can read the system clipboard itself on the same machine.
    pub fn image_paste_tx(&self) -> Option<&flume::Sender<String>> {
        match self {
            Self::WebSocket(h) => Some(&h.image_paste_tx),
            Self::Direct(_) => None,
        }
    }

    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct(_))
    }
}
