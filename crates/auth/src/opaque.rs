//! Opaque bearer handles, and the digest they are stored as.
//!
//! Four credentials in this system share one shape: a refresh token, the handle a pending
//! sign-in is resumed by, a step-up grant, and a recovery code. None is user-chosen, all are
//! high-entropy, and all are stored as a digest so the row is not itself a usable credential.
//!
//! They share this module because the *stored representation* has to be identical across them
//! — it is what a lookup matches on. A second, subtly different encoder (upper-case hex,
//! base64, a different digest) would not fail a test; it would simply never find the row, and
//! the symptom is "sign-in stopped working" with a green test suite.

use base64::Engine as _;
use rand::Rng as _;
use secrecy::{ExposeSecret as _, SecretString};
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;

/// Entropy behind a generated handle, in bytes.
const HANDLE_BYTES: usize = 32;

/// Generate a fresh opaque handle (URL-safe base64, unpadded).
///
/// The raw value goes to the client and is never persisted; [`hash_handle`] of it is.
#[must_use]
pub fn generate_handle() -> SecretString {
    let mut bytes = [0u8; HANDLE_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    SecretString::from(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// The stored representation of a handle: lower-case hex SHA-256.
///
/// Not wrapped in a `secrecy` type: the digest discloses nothing about its input, and wrapping
/// it would put an `expose_secret()` on every database call that stores or looks one up.
///
/// A fast hash on purpose — these are 256-bit server-generated tokens, so there is no
/// dictionary for a slow KDF to defend against, and the lookup has to be an indexed equality
/// against a column.
#[must_use]
pub fn hash_handle(raw: &SecretString) -> String {
    sha256_hex(raw.expose_secret().as_bytes())
}

/// Lower-case hex SHA-256 of `bytes`, shared by [`hash_handle`] and the recovery-code hasher.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{generate_handle, hash_handle};
    use secrecy::ExposeSecret as _;

    #[test]
    fn a_handle_is_url_safe_and_hashes_to_stable_lower_case_hex() {
        let handle = generate_handle();
        assert!(
            handle
                .expose_secret()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "a handle travels in a JSON body and a header; it must need no escaping"
        );

        let digest = hash_handle(&handle);
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(
            digest,
            hash_handle(&handle),
            "hashing must be deterministic"
        );
    }
}
