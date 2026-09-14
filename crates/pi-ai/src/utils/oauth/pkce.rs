//! Port of packages/ai/src/utils/oauth/pkce.ts
//!
//! The TypeScript uses the Web Crypto API (`crypto.getRandomValues` +
//! `crypto.subtle.digest("SHA-256")`). The Rust port uses `rand` and `sha2`,
//! which are the same primitives behind those Web Crypto calls.

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

fn base64url_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD_NO_PAD
        .encode(bytes)
        .replace('+', "-")
        .replace('/', "_")
}

/// Generate PKCE code verifier and challenge.
/// Uses SHA-256 for cross-platform compatibility.
pub async fn generate_pkce() -> (String, String) {
    let mut verifier_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut verifier_bytes);
    let verifier = base64url_encode(&verifier_bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    let challenge = base64url_encode(&hash);

    (verifier, challenge)
}

/// `generatePKCE()` returning the TypeScript object shape.
pub async fn generate_pkce_object() -> PkcePair {
    let (verifier, challenge) = generate_pkce().await;
    PkcePair { verifier, challenge }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verifier_is_43_chars_and_challenge_is_sha256_base64url() {
        let pair = generate_pkce_object().await;
        // 32 random bytes base64url without padding -> 43 characters.
        assert_eq!(pair.verifier.len(), 43);
        assert!(!pair.verifier.contains('+') && !pair.verifier.contains('/') && !pair.verifier.contains('='));
        assert_eq!(pair.challenge.len(), 43);

        let mut hasher = Sha256::new();
        hasher.update(pair.verifier.as_bytes());
        let expected = base64url_encode(&hasher.finalize());
        assert_eq!(pair.challenge, expected);
    }

    #[tokio::test]
    async fn each_call_generates_a_new_verifier() {
        let first = generate_pkce_object().await;
        let second = generate_pkce_object().await;
        assert_ne!(first.verifier, second.verifier);
    }

    #[test]
    fn base64url_encoding_matches_rfc_example() {
        // RFC 7636 appendix B verifier/challenge pair.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        assert_eq!(
            base64url_encode(&hasher.finalize()),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}
