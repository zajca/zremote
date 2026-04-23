//! Admin token primitives: generate, hash, verify.
//!
//! The admin token is the bootstrap credential: 32 random bytes encoded as
//! base64url. Only its SHA-256 hex digest is persisted in `admin_config`.
//! Constant-time comparison of hex digests prevents timing attacks.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Generate a new admin token: 32 CSPRNG bytes, base64url (no padding).
///
/// Panics only if the operating system CSPRNG is unavailable, which the
/// RFC treats as an unrecoverable environment failure — there is no safe
/// fallback for producing an auth credential.
#[must_use]
pub fn generate() -> String {
    let mut bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .expect("OS CSPRNG must be available for auth token generation");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute the SHA-256 hex digest of an admin token.
#[must_use]
pub fn hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Verify a presented token against a stored SHA-256 hex digest, in
/// constant time relative to the digest length.
#[must_use]
pub fn verify(provided: &str, stored_hash: &str) -> bool {
    let provided_hash = hash(provided);
    let p = provided_hash.as_bytes();
    let s = stored_hash.as_bytes();
    if p.len() != s.len() {
        return false;
    }
    p.ct_eq(s).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_returns_unique_values() {
        let a = generate();
        let b = generate();
        assert_ne!(a, b);
        // base64url of 32 bytes with no padding is 43 chars.
        assert_eq!(a.len(), 43);
    }

    #[test]
    fn hash_is_deterministic_and_64_hex() {
        let h1 = hash("tok");
        let h2 = hash("tok");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn verify_accepts_correct_token() {
        let tok = generate();
        let h = hash(&tok);
        assert!(verify(&tok, &h));
    }

    #[test]
    fn verify_rejects_wrong_token() {
        let h = hash("right");
        assert!(!verify("wrong", &h));
    }

    #[test]
    fn verify_rejects_length_mismatched_hash() {
        assert!(!verify("any-token", "short"));
    }
}
