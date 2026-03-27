use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::debug;
use zremote_client::{TerminalSession, types::TerminalEvent, types::TerminalInput};

use crate::error::FfiError;

/// Callback interface for receiving terminal output and lifecycle events.
///
/// Implement this trait in Kotlin/Swift to receive terminal data.
/// `on_output` receives raw bytes (maps to `ByteArray` in Kotlin).
/// All methods are called from a background thread.
#[uniffi::export(callback_interface)]
pub trait TerminalListener: Send + Sync {
    fn on_output(&self, data: Vec<u8>);
    fn on_pane_output(&self, pane_id: String, data: Vec<u8>);
    fn on_pane_added(&self, pane_id: String, index: u16);
    fn on_pane_removed(&self, pane_id: String);
    fn on_session_closed(&self, exit_code: Option<i32>);
    fn on_scrollback_start(&self, cols: u16, rows: u16);
    fn on_scrollback_end(&self, truncated: bool);
    fn on_session_suspended(&self);
    fn on_session_resumed(&self);
    fn on_error(&self, message: String);
    fn on_disconnected(&self);
}

/// Handle to a running terminal WebSocket session.
/// Call `disconnect()` or drop to close the terminal connection.
#[derive(uniffi::Object)]
pub struct ZRemoteTerminal {
    input_tx: flume::Sender<TerminalInput>,
    resize_tx: flume::Sender<(u16, u16)>,
    image_paste_tx: flume::Sender<String>,
    cancel: CancellationToken,
}

#[uniffi::export]
impl ZRemoteTerminal {
    /// Send terminal input data (main pane).
    pub fn send_input(&self, data: Vec<u8>) -> Result<(), FfiError> {
        self.input_tx
            .try_send(TerminalInput::Data(data))
            .map_err(|_| FfiError::ChannelClosed)
    }

    /// Send terminal input data to a specific pane.
    pub fn send_pane_input(&self, pane_id: String, data: Vec<u8>) -> Result<(), FfiError> {
        self.input_tx
            .try_send(TerminalInput::PaneData { pane_id, data })
            .map_err(|_| FfiError::ChannelClosed)
    }

    /// Resize the terminal.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), FfiError> {
        self.resize_tx
            .try_send((cols, rows))
            .map_err(|_| FfiError::ChannelClosed)
    }

    /// Paste a base64-encoded image.
    pub fn paste_image(&self, base64_data: String) -> Result<(), FfiError> {
        self.image_paste_tx
            .try_send(base64_data)
            .map_err(|_| FfiError::ChannelClosed)
    }

    /// Disconnect the terminal session.
    pub fn disconnect(&self) {
        self.cancel.cancel();
    }
}

impl ZRemoteTerminal {
    /// Wrap a `TerminalSession` with a callback-based output dispatcher.
    pub(crate) fn start(
        session: TerminalSession,
        listener: Box<dyn TerminalListener>,
        runtime: &Arc<tokio::runtime::Runtime>,
    ) -> Arc<Self> {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let listener = Arc::from(listener);

        let input_tx = session.input_tx.clone();
        let resize_tx = session.resize_tx.clone();
        let image_paste_tx = session.image_paste_tx.clone();
        let output_rx = session.output_rx.clone();

        runtime.spawn(async move {
            dispatch_terminal_events(output_rx, listener, cancel_clone).await;
            // Keep session alive until dispatcher exits
            drop(session);
        });

        Arc::new(Self {
            input_tx,
            resize_tx,
            image_paste_tx,
            cancel,
        })
    }
}

async fn dispatch_terminal_events(
    output_rx: flume::Receiver<TerminalEvent>,
    listener: Arc<dyn TerminalListener>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                debug!("terminal dispatcher cancelled");
                return;
            }
            result = output_rx.recv_async() => {
                if let Ok(event) = result {
                    dispatch_terminal_event(&listener, event);
                } else {
                    debug!("terminal output channel closed");
                    listener.on_disconnected();
                    return;
                }
            }
        }
    }
}

fn dispatch_terminal_event(listener: &Arc<dyn TerminalListener>, event: TerminalEvent) {
    match event {
        TerminalEvent::Output(data) => listener.on_output(data),
        TerminalEvent::PaneOutput { pane_id, data } => listener.on_pane_output(pane_id, data),
        TerminalEvent::PaneAdded { pane_id, index } => listener.on_pane_added(pane_id, index),
        TerminalEvent::PaneRemoved { pane_id } => listener.on_pane_removed(pane_id),
        TerminalEvent::SessionClosed { exit_code } => listener.on_session_closed(exit_code),
        TerminalEvent::ScrollbackStart { cols, rows } => listener.on_scrollback_start(cols, rows),
        TerminalEvent::ScrollbackEnd { truncated } => listener.on_scrollback_end(truncated),
        TerminalEvent::SessionSuspended => listener.on_session_suspended(),
        TerminalEvent::SessionResumed => listener.on_session_resumed(),
        TerminalEvent::Error { message } => listener.on_error(message),
        TerminalEvent::Disconnected => listener.on_disconnected(),
    }
}
