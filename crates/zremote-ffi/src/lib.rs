//! `UniFFI` bindings for the `ZRemote` client SDK.
//!
//! This crate wraps `zremote-client` with FFI-safe types and callback interfaces,
//! generating Kotlin and Swift bindings via Mozilla `UniFFI`.

mod client;
mod error;
mod events;
mod terminal;
mod types;

pub use client::ZRemoteClient;
pub use error::FfiError;
pub use events::{EventListener, ZRemoteEventStream};
pub use terminal::{TerminalListener, ZRemoteTerminal};
pub use types::*;

uniffi::setup_scaffolding!();
