//! Random secrets and their at-rest hashes.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const TOKEN_PREFIX: &str = "ytp_";

/// 32 random bytes, base64url without padding (43 chars).
pub fn random_secret() -> String {
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

pub fn new_api_token() -> String {
    format!("{TOKEN_PREFIX}{}", random_secret())
}

/// Hex SHA-256. Secrets are high-entropy, so a fast hash is the right tool:
/// it only has to make a leaked database useless for impersonation.
pub fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        let s = random_secret();
        assert_eq!(s.len(), 43);
        assert_ne!(s, random_secret());
        assert!(new_api_token().starts_with("ytp_"));
        assert_eq!(hash_secret("x").len(), 64);
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
    }
}
