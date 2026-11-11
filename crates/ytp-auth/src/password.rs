//! Argon2id password hashing. Work runs on the blocking pool.

use std::sync::OnceLock;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use ytp_core::{Error, Result};

pub const MIN_LEN: usize = 8;
pub const MAX_LEN: usize = 1024;

pub fn validate(password: &str) -> Result<()> {
    let n = password.chars().count();
    if n < MIN_LEN {
        return Err(Error::invalid(format!(
            "password must be at least {MIN_LEN} characters"
        )));
    }
    if password.len() > MAX_LEN {
        return Err(Error::invalid("password is too long"));
    }
    Ok(())
}

pub fn hash_blocking(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(Error::internal)
}

pub fn verify_blocking(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

pub async fn hash(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || hash_blocking(&password))
        .await
        .map_err(Error::internal)?
}

pub async fn verify(password: String, phc: String) -> bool {
    tokio::task::spawn_blocking(move || verify_blocking(&password, &phc))
        .await
        .unwrap_or(false)
}

/// A real hash of nothing in particular, verified against when the username
/// does not exist so that both paths cost one Argon2 run.
pub fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_blocking("not-a-real-password-just-burning-time").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let h = hash_blocking("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_blocking("correct horse", &h));
        assert!(!verify_blocking("wrong", &h));
        assert!(!verify_blocking("x", "garbage"));
        assert!(validate("short").is_err());
        assert!(validate("long enough").is_ok());
    }
}
