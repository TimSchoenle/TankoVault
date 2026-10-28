//! Argon2id password hashing.

use crate::error::AuthError;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher as _, PasswordVerifier as _};
use argon2::{Algorithm, Argon2, Params, Version};
use secrecy::{ExposeSecret as _, SecretSlice, SecretString};

/// Builds an argon2id hasher keyed with a server-side `pepper`, held outside the database.
///
/// The pepper is argon2's secret input, not concatenated with the password, so with a strong
/// pepper a database leak alone cannot be brute-forced offline. An empty pepper reproduces
/// [`Argon2::default`], so hashes made without one keep verifying, and those hashes have no such
/// protection.
fn hasher(pepper: &SecretSlice<u8>) -> Result<Argon2<'_>, AuthError> {
    Argon2::new_with_secret(
        pepper.expose_secret(),
        Algorithm::Argon2id,
        Version::V0x13,
        Params::default(),
    )
    .map_err(|_| AuthError::Hashing)
}

/// Hashes a password into an argon2id PHC string with an embedded salt, keyed by `pepper`.
///
/// The pepper is not embedded and must be supplied again to [`verify_password`]; an empty
/// [`SecretSlice`] hashes without one. The result is a plain `String` because a PHC hash is not
/// a secret.
///
/// # Errors
/// [`AuthError::Hashing`] if the pepper is unusable or the KDF fails.
pub fn hash_password(
    password: &SecretString,
    pepper: &SecretSlice<u8>,
) -> Result<String, AuthError> {
    hasher(pepper)?
        .hash_password(password.expose_secret().as_bytes())
        .map(|h| h.to_string())
        .map_err(|_| AuthError::Hashing)
}

/// Verifies `password` against a stored argon2id PHC `hash`, keyed by the same `pepper`.
///
/// A wrong password or a wrong pepper is `Ok(false)`.
///
/// # Errors
/// [`AuthError::MalformedHash`] for an invalid PHC string, [`AuthError::Hashing`] for an unusable
/// pepper.
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
        let password = SecretString::from("s3cret");
        let hash = hash_password(&password, &default_pepper()).unwrap();
        assert!(!verify_password(&password, &hash, &pepper(b"different-pepper")).unwrap());
    }

    #[test]
    fn stored_hash_from_argon2_0_5_still_verifies() {
        // Written by `argon2` 0.5.3 with the default pepper; such hashes must keep verifying.
        const STORED: &str = "$argon2id$v=19$m=19456,t=2,p=1$dGFua292YXVsdC1hcmdvbjI$\
                              uY7LSPondWiKAJLZqFN2idQ3i9cru3g3FAtV9VSf/u4";
        let password = SecretString::from("correct horse battery staple");
        assert!(verify_password(&password, STORED, &default_pepper()).unwrap());
        assert!(!verify_password(&password, STORED, &pepper(b"")).unwrap());
    }

    #[test]
    fn empty_pepper_is_backward_compatible() {
        let password = SecretString::from("s3cret");
        let hash = hash_password(&password, &pepper(b"")).unwrap();
        assert!(verify_password(&password, &hash, &pepper(b"")).unwrap());
    }
}
