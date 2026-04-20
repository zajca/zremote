//! In-memory ws-ticket store. GUI → `POST /api/auth/ws-ticket` with
//! `{ route, resource_id }` → opaque 32-byte token, 30 s TTL, single-use.
//! On `GET /ws/<route>/:id` the ticket is redeemed (via the
//! `Sec-WebSocket-Protocol: zremote.ticket.<base64url>` header) and must
//! match the registered route + resource.
//!
//! This Phase 1 slice provides the data structure + issue/redeem API. The
//! HTTP handler and WebSocket middleware that call it land in Phase 2 and
//! Phase 3.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Default ticket TTL. RFC §4 — 30 seconds.
pub const DEFAULT_TICKET_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct TicketEntry {
    pub session_id: Uuid,
    pub route: String,
    pub resource_id: Option<String>,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct RedeemedTicket {
    pub session_id: Uuid,
}

#[derive(Debug)]
pub enum TicketErr {
    Unknown,
    Expired,
    WrongRoute,
    WrongResource,
}

impl std::fmt::Display for TicketErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("unknown ws ticket"),
            Self::Expired => f.write_str("ws ticket expired"),
            Self::WrongRoute => f.write_str("ws ticket bound to a different route"),
            Self::WrongResource => f.write_str("ws ticket bound to a different resource"),
        }
    }
}

impl std::error::Error for TicketErr {}

/// Shared ticket store. Keyed by SHA-256 hex of the raw ticket string so a
/// lock on the store never holds plaintext.
#[derive(Clone, Default)]
pub struct TicketStore {
    inner: Arc<Mutex<HashMap<String, TicketEntry>>>,
    ttl: Duration,
}

impl std::fmt::Debug for TicketStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TicketStore")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl TicketStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl: DEFAULT_TICKET_TTL,
        }
    }

    #[must_use]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    /// Issue a new ticket bound to a (session, route, optional resource).
    /// Returns `(plaintext_ticket, expires_at_instant)`.
    pub fn issue_ticket(
        &self,
        session_id: Uuid,
        route: impl Into<String>,
        resource_id: Option<String>,
    ) -> (String, Instant) {
        let mut bytes = [0u8; 32];
        OsRng
            .try_fill_bytes(&mut bytes)
            .expect("OS CSPRNG must be available for ws-ticket generation");
        let ticket = URL_SAFE_NO_PAD.encode(bytes);
        let hash = hash_ticket(&ticket);
        let expires_at = Instant::now() + self.ttl;
        let entry = TicketEntry {
            session_id,
            route: route.into(),
            resource_id,
            expires_at,
        };
        // Lock poisoning here means an earlier panic corrupted the store; we
        // refuse to keep reusing it rather than masking the bug.
        let mut guard = self.inner.lock().expect("ws ticket store lock poisoned");
        guard.insert(hash, entry);
        (ticket, expires_at)
    }

    /// Redeem a ticket. Removes the entry unconditionally on lookup (single-use).
    /// Validates that it matches the expected route + resource and has not
    /// expired.
    pub fn redeem_ticket(
        &self,
        ticket: &str,
        expected_route: &str,
        expected_resource: Option<&str>,
    ) -> Result<RedeemedTicket, TicketErr> {
        let hash = hash_ticket(ticket);
        let entry = {
            let mut guard = self.inner.lock().expect("ws ticket store lock poisoned");
            guard.remove(&hash)
        };
        let entry = entry.ok_or(TicketErr::Unknown)?;

        if Instant::now() >= entry.expires_at {
            return Err(TicketErr::Expired);
        }
        if entry.route != expected_route {
            return Err(TicketErr::WrongRoute);
        }
        if entry.resource_id.as_deref() != expected_resource {
            return Err(TicketErr::WrongResource);
        }
        Ok(RedeemedTicket {
            session_id: entry.session_id,
        })
    }

    /// Drop all entries that have expired. Intended for periodic cleanup.
    pub fn purge_expired(&self) -> usize {
        let now = Instant::now();
        let mut guard = self.inner.lock().expect("ws ticket store lock poisoned");
        let before = guard.len();
        guard.retain(|_, entry| entry.expires_at > now);
        before - guard.len()
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("ws ticket store lock poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn hash_ticket(ticket: &str) -> String {
    let mut h = Sha256::new();
    h.update(ticket.as_bytes());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_then_redeem_ok() {
        let store = TicketStore::new();
        let sid = Uuid::new_v4();
        let (ticket, _exp) = store.issue_ticket(sid, "terminal", Some("r-1".to_string()));
        let redeemed = store
            .redeem_ticket(&ticket, "terminal", Some("r-1"))
            .unwrap();
        assert_eq!(redeemed.session_id, sid);
    }

    #[test]
    fn redeem_twice_second_fails() {
        let store = TicketStore::new();
        let sid = Uuid::new_v4();
        let (ticket, _) = store.issue_ticket(sid, "terminal", Some("r".into()));
        store.redeem_ticket(&ticket, "terminal", Some("r")).unwrap();
        let err = store
            .redeem_ticket(&ticket, "terminal", Some("r"))
            .unwrap_err();
        assert!(matches!(err, TicketErr::Unknown));
    }

    #[test]
    fn redeem_expired_fails() {
        // Use a zero-ish TTL so the Instant is immediately in the past.
        let store = TicketStore::with_ttl(Duration::from_nanos(1));
        let sid = Uuid::new_v4();
        let (ticket, _) = store.issue_ticket(sid, "events", None);
        // Sleep a tiny amount to guarantee expiry.
        std::thread::sleep(Duration::from_millis(2));
        let err = store.redeem_ticket(&ticket, "events", None).unwrap_err();
        assert!(matches!(err, TicketErr::Expired));
    }

    #[test]
    fn redeem_wrong_route_fails() {
        let store = TicketStore::new();
        let sid = Uuid::new_v4();
        let (ticket, _) = store.issue_ticket(sid, "terminal", Some("r".into()));
        let err = store
            .redeem_ticket(&ticket, "events", Some("r"))
            .unwrap_err();
        assert!(matches!(err, TicketErr::WrongRoute));
    }

    #[test]
    fn redeem_wrong_resource_fails() {
        let store = TicketStore::new();
        let sid = Uuid::new_v4();
        let (ticket, _) = store.issue_ticket(sid, "terminal", Some("r1".into()));
        let err = store
            .redeem_ticket(&ticket, "terminal", Some("r2"))
            .unwrap_err();
        assert!(matches!(err, TicketErr::WrongResource));
    }

    #[test]
    fn redeem_unknown_ticket_fails() {
        let store = TicketStore::new();
        let err = store
            .redeem_ticket("not-a-real-ticket", "terminal", None)
            .unwrap_err();
        assert!(matches!(err, TicketErr::Unknown));
    }

    #[test]
    fn purge_expired_drops_only_expired() {
        let store = TicketStore::with_ttl(Duration::from_nanos(1));
        let sid = Uuid::new_v4();
        let (_t1, _) = store.issue_ticket(sid, "a", None);
        let (_t2, _) = store.issue_ticket(sid, "b", None);
        std::thread::sleep(Duration::from_millis(2));
        // Issue one more with a longer TTL using a separate store to prove
        // the cleanup is time-driven, not blanket.
        let store_long = TicketStore::with_ttl(Duration::from_secs(60));
        let (_t3, _) = store_long.issue_ticket(sid, "c", None);

        assert_eq!(store.purge_expired(), 2);
        assert_eq!(store_long.purge_expired(), 0);
        assert!(store.is_empty());
        assert_eq!(store_long.len(), 1);
    }
}
