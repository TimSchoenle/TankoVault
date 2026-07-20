//! Argon2id password hashing.

use crate::error::AuthError;
use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

/// Hash a plaintext password into an argon2id PHC string (salt embedded).
///
/// Uses argon2's default (tuned) parameters. The returned string is what the DB stores.
///
/// # Errors
/// [`AuthError::Hashing`] if the KDF fails.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| AuthError::Hashing)
}

/// Verify `password` against a stored argon2id PHC `hash`.
///
/// Returns `Ok(false)` on a mismatch and `Err` only when the stored hash is unparseable.
///
/// # Errors
/// [`AuthError::MalformedHash`] if `hash` is not a valid PHC string.
pub fn verify_password(password: &str, hash: &str) -> Result<bool, AuthError> {
    let parsed = PasswordHash::new(hash).map_err(|_| AuthError::MalformedHash)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_verifies_correct_password() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
    }

    #[test]
    fn hash_rejects_wrong_password() {
        let hash = hash_password("s3cret").unwrap();
        assert!(!verify_password("guess", &hash).unwrap());
    }

    #[test]
    fn distinct_salts_produce_distinct_hashes() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn malformed_hash_errors() {
        assert!(verify_password("x", "not-a-phc-string").is_err());
    }
}
