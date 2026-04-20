//! Server-side authentication primitives (RFC auth-overhaul §Phase 1).
//!
//! This module is the collection point for admin bearer auth, ws-ticket
//! bindings, admin-token hashing, and the deprecated per-connection agent
//! token path. Phase 2 will add `auth_mw` + the REST routes, Phase 3 will
//! replace the legacy agent-token check with the HMAC challenge-response
//! flow and then delete `legacy.rs`.

pub mod admin_token;
pub mod bearer;
pub mod legacy;
pub mod session;
pub mod ws_ticket;

// Legacy re-exports used throughout the server crate. The legacy helpers
// remain the entry point for the pre-v2 `Register { token }` path until
// Phase 3 replaces it.
pub use legacy::{hash_token, verify_token};

// Surfaced at module level so Phase 2 (auth_mw, routes::auth) can import them
// via `crate::auth::AuthContext` etc. Held behind `allow` because nothing in
// Phase 1 consumes them yet — clippy -D warnings would otherwise trip on the
// unused public re-export.
#[allow(unused_imports)]
pub use bearer::{AuthContext, AuthErr};
#[allow(unused_imports)]
pub use ws_ticket::{TicketErr, TicketStore};
