use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Compute the SHA-256 hex digest of a token.
#[tracing::instrument(skip(token))]
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

/// Verify a provided token against a stored hash using constant-time
/// comparison to prevent timing attacks.
#[tracing::instrument(skip(provided, stored_hash))]
pub fn verify_token(provided: &str, stored_hash: &str) -> bool {
    let provided_hash = hash_token(provided);
    let provided_bytes = provided_hash.as_bytes();
    let stored_bytes = stored_hash.as_bytes();

    if provided_bytes.len() != stored_bytes.len() {
        return false;
    }

    provided_bytes.ct_eq(stored_bytes).into()
}

/// Simple hex encoding for SHA-256 output (avoids adding hex crate dependency).
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().fold(String::new(), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_produces_consistent_output() {
        let hash1 = hash_token("test-token");
        let hash2 = hash_token("test-token");
        assert_eq!(hash1, hash2);
        // SHA-256 produces 64 hex characters
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn different_tokens_produce_different_hashes() {
        let hash1 = hash_token("token-a");
        let hash2 = hash_token("token-b");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn verify_valid_token() {
        let token = "my-secret-token";
        let hash = hash_token(token);
        assert!(verify_token(token, &hash));
    }

    #[test]
    fn verify_invalid_token() {
        let hash = hash_token("correct-token");
        assert!(!verify_token("wrong-token", &hash));
    }

    #[test]
    fn verify_rejects_malformed_hash() {
        assert!(!verify_token("token", "not-a-valid-hash"));
    }
}
