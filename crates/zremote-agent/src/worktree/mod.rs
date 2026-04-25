//! Shared worktree operations used by both the local HTTP handler and the
//! server-mode WebSocket dispatcher. Extracting these into one place guarantees
//! every caller goes through the same validation (CWE-88 leading-dash guard),
//! the same git invocation, and the same post-create hook flow.

pub mod service;
