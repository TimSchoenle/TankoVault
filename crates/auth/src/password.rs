//! Argon2id password hashing.

use crate::error::AuthError;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// Build an argon2id hasher keyed with a server-side `pepper` — a secret held outside the
/// database (config/KMS/env), never alongside the hashes.
///
/// The pepper is supplied as argon2's *secret* input rather than concatenated with the
/// password: this is the keyed-hash construction argon2 provides for exactly this purpose,
/// so a database leak on its own — without the pepper — cannot be brute-forced offline. An
/// **empty** pepper reproduces the parameters of [`Argon2::default`], so hashes written
/// before any pepper was configured keep verifying (pass an empty pepper for them).
fn hasher(pepper: &[u8]) -> Result<Argon2<'_>, AuthError> {
    Argon2::new_with_secret(
        pepper,
        Algorithm::Argon2id,
        Version::V0x13,
        Params::default(),
    )
    .map_err(|_| AuthError::Hashing)
}

/// Hash a plaintext password into an argon2id PHC string (salt embedded).
///
/// Uses argon2's default (tuned) parameters, keyed by `pepper` (see the private `hasher`). The
/// returned string is what the DB stores; the pepper is **not** embedded and must be
/// supplied again to [`verify_password`]. Pass an empty slice to hash without a pepper.
///
/// # Errors
/// [`AuthError::Hashing`] if the pepper is unusable or the KDF fails.
pub fn hash_password(password: &str, pepper: &[u8]) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    hasher(pepper)?
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| AuthError::Hashing)
}

/// Verify `password` against a stored argon2id PHC `hash`, keyed by the same `pepper` used
/// to produce it.
///
/// Returns `Ok(false)` on a mismatch (wrong password *or* wrong pepper) and `Err` only when
/// the stored hash is unparseable or the pepper is unusable.
///
/// # Errors
/// [`AuthError::MalformedHash`] if `hash` is not a valid PHC string;
/// [`AuthError::Hashing`] if the pepper is unusable.
pub fn verify_password(password: &str, hash: &str, pepper: &[u8]) -> Result<bool, AuthError> {
    let parsed = PasswordHash::new(hash).map_err(|_| AuthError::MalformedHash)?;
    Ok(hasher(pepper)?
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEPPER: &[u8] = b"a-server-side-pepper";

    #[test]
    fn hash_verifies_correct_password() {
        let hash = hash_password("correct horse battery staple", PEPPER).unwrap();
        assert!(verify_password("correct horse battery staple", &hash, PEPPER).unwrap());
    }

    #[test]
    fn hash_rejects_wrong_password() {
        let hash = hash_password("s3cret", PEPPER).unwrap();
        assert!(!verify_password("guess", &hash, PEPPER).unwrap());
    }

    #[test]
    fn distinct_salts_produce_distinct_hashes() {
        let a = hash_password("same", PEPPER).unwrap();
        let b = hash_password("same", PEPPER).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn malformed_hash_errors() {
        assert!(verify_password("x", "not-a-phc-string", PEPPER).is_err());
    }

    #[test]
    fn wrong_pepper_rejects_password() {
        // The whole point of a pepper: the stored hash is useless to an attacker who has the
        // database but not the pepper.
        let hash = hash_password("s3cret", PEPPER).unwrap();
        assert!(!verify_password("s3cret", &hash, b"different-pepper").unwrap());
    }

    #[test]
    fn empty_pepper_is_backward_compatible() {
        // A deployment that never configured a pepper hashes and verifies with an empty one;
        // this must keep working so existing stored hashes remain valid.
        let hash = hash_password("s3cret", b"").unwrap();
        assert!(verify_password("s3cret", &hash, b"").unwrap());
    }
}
