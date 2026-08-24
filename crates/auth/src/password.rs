//! Argon2id password hashing.

use crate::error::AuthError;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use secrecy::{ExposeSecret as _, SecretSlice, SecretString};

/// Build an argon2id hasher keyed with a server-side `pepper` (held outside the database).
///
/// Passed as argon2's *secret* input, not concatenated with the password — the keyed-hash
/// construction means a database leak alone cannot be brute-forced offline. An empty pepper
/// reproduces [`Argon2::default`], so pre-pepper hashes keep verifying.
fn hasher(pepper: &SecretSlice<u8>) -> Result<Argon2<'_>, AuthError> {
    Argon2::new_with_secret(
        pepper.expose_secret(),
        Algorithm::Argon2id,
        Version::V0x13,
        Params::default(),
    )
    .map_err(|_| AuthError::Hashing)
}

/// Hash a plaintext password into an argon2id PHC string, salt embedded, keyed by `pepper`.
///
/// The pepper is not embedded and must be supplied again to [`verify_password`]; pass an
/// empty [`SecretSlice`] to hash without one.
///
/// Returns a plain `String`, not a secret wrapper: a PHC hash isn't itself a secret, and
/// wrapping it would blur what `expose_secret()` means at neighbouring call sites.
///
/// # Errors
/// [`AuthError::Hashing`] if the pepper is unusable or the KDF fails.
pub fn hash_password(
    password: &SecretString,
    pepper: &SecretSlice<u8>,
) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    hasher(pepper)?
        .hash_password(password.expose_secret().as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| AuthError::Hashing)
}

/// Verify `password` against a stored argon2id PHC `hash`, keyed by the same `pepper`.
///
/// Returns `Ok(false)` on a mismatch (wrong password or wrong pepper), `Err` only when the
/// hash is unparseable or the pepper is unusable.
///
/// # Errors
/// [`AuthError::MalformedHash`] for an invalid PHC string; [`AuthError::Hashing`] for an
/// unusable pepper.
pub fn verify_password(
    password: &SecretString,
    hash: &str,
    pepper: &SecretSlice<u8>,
) -> Result<bool, AuthError> {
    let parsed = PasswordHash::new(hash).map_err(|_| AuthError::MalformedHash)?;
    Ok(hasher(pepper)?
        .verify_password(password.expose_secret().as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pepper(value: &[u8]) -> SecretSlice<u8> {
        SecretSlice::from(value.to_vec())
    }

    fn default_pepper() -> SecretSlice<u8> {
        pepper(b"a-server-side-pepper")
    }

    #[test]
    fn hash_verifies_correct_password() {
        let password = SecretString::from("correct horse battery staple");
        let hash = hash_password(&password, &default_pepper()).unwrap();
        assert!(verify_password(&password, &hash, &default_pepper()).unwrap());
    }

    #[test]
    fn hash_rejects_wrong_password() {
        let hash = hash_password(&SecretString::from("s3cret"), &default_pepper()).unwrap();
        assert!(!verify_password(&SecretString::from("guess"), &hash, &default_pepper()).unwrap());
    }

    #[test]
    fn distinct_salts_produce_distinct_hashes() {
        let password = SecretString::from("same");
        let a = hash_password(&password, &default_pepper()).unwrap();
        let b = hash_password(&password, &default_pepper()).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn malformed_hash_errors() {
        assert!(
            verify_password(
                &SecretString::from("x"),
                "not-a-phc-string",
                &default_pepper()
            )
            .is_err()
        );
    }

    #[test]
    fn wrong_pepper_rejects_password() {
        // A pepper's whole point: the stored hash is useless without it.
        let password = SecretString::from("s3cret");
        let hash = hash_password(&password, &default_pepper()).unwrap();
        assert!(!verify_password(&password, &hash, &pepper(b"different-pepper")).unwrap());
    }

    #[test]
    fn empty_pepper_is_backward_compatible() {
        // No configured pepper means hashing/verifying with an empty one; must keep working.
        let password = SecretString::from("s3cret");
        let hash = hash_password(&password, &pepper(b"")).unwrap();
        assert!(verify_password(&password, &hash, &pepper(b"")).unwrap());
    }
}
