//! Server-side authentication primitives (RFC auth-overhaul §Phase 1).
//!
//! This module is the collection point for admin bearer auth, ws-ticket
//! bindings, admin-token hashing, and the deprecated per-connection agent
//! token path. Phase 2 will add `auth_mw` + the REST routes, Phase 3 will
//! replace the legacy agent-token check with the HMAC challenge-response
//! flow and then delete `legacy.rs`.

pub mod admin_token;
pub mod agent_auth;
pub mod bearer;
pub mod legacy;
pub mod oidc;
pub mod session;
pub mod ws_ticket;

// Legacy re-exports used throughout the server crate. The legacy helpers
// remain the entry point for the pre-v2 `Register { token }` path until
// Phase 3 replaces it.
pub use legacy::{hash_token, verify_token};

// Re-exports for convenient imports from `auth_mw`, `routes::auth`, and
// tests. Phase 2 consumers now reference them directly via these paths.
// `AuthErr` stays qualified through `bearer::AuthErr` because only the
// middleware touches the error type and keeping it scoped makes the
// oracle-collapse path locally obvious. If a future consumer needs
// `AuthErr` at crate-auth level, add it here with a concrete caller.
pub use bearer::AuthContext;
pub use oidc::OidcFlowStore;
pub use ws_ticket::{TicketErr, TicketStore};
