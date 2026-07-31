//! Authenticated symmetric encryption for secrets at rest (design §16).
//!
//! [`SecretBox`] wraps AES-256-GCM. External-provider credentials — `AniList` OAuth
//! access/refresh tokens — are sealed before they touch the database, so a database
//! compromise alone does not disclose them. A fresh random 96-bit nonce is generated per
//! message and prepended to the ciphertext, and the data-encryption key is supplied by the
//! caller from a secret store / KMS — never hard-coded and never persisted alongside the
//! ciphertext.

use aes_gcm::aead::{Aead, KeyInit, Nonce};
use aes_gcm::{Aes256Gcm, Key};
use base64::Engine;
use rand::Rng as _;

use crate::AuthError;

/// AES-GCM standard nonce length (96 bits).
const NONCE_LEN: usize = 12;
/// AES-256 key length.
const KEY_LEN: usize = 32;

/// An AES-256-GCM sealing box over a single data-encryption key.
///
/// Cheap to clone (the underlying key schedule is shared), so a single instance can be
/// held in service state and used across requests.
#[derive(Clone)]
pub struct SecretBox {
    cipher: Aes256Gcm,
}

impl SecretBox {
    /// Build from a raw 32-byte data-encryption key.
    #[must_use]
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        let key = Key::<Aes256Gcm>::from(*key);
        Self {
            cipher: Aes256Gcm::new(&key),
        }
    }

    /// Build from a base64 (standard alphabet) encoded 32-byte key, as delivered by a
    /// secret store or environment variable.
    ///
    /// # Errors
    /// [`AuthError::InvalidKey`] if the value is not valid base64 or does not decode to
    /// exactly 32 bytes.
    pub fn from_base64_key(encoded: &str) -> Result<Self, AuthError> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| AuthError::InvalidKey)?;
        let key: [u8; KEY_LEN] = bytes.try_into().map_err(|_| AuthError::InvalidKey)?;
        Ok(Self::new(&key))
    }

    /// Seal `plaintext`, returning `nonce (12 bytes) || ciphertext-with-tag`. The output
    /// is self-describing: [`open`](Self::open) needs only the sealing key.
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the AEAD provider fails (e.g. allocation).
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, AuthError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::<Aes256Gcm>::from(nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| AuthError::Crypto)?;
        let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Open a value produced by [`seal`](Self::seal).
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the input is shorter than the nonce or authentication
    /// fails (wrong key, tampering, or truncation).
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, AuthError> {
        if sealed.len() < NONCE_LEN {
            return Err(AuthError::Crypto);
        }
        let (nonce_bytes, ciphertext) = sealed.split_at(NONCE_LEN);
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).map_err(|_| AuthError::Crypto)?;
        self.cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|_| AuthError::Crypto)
    }
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render key material.
        f.write_str("SecretBox(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_() -> SecretBox {
        SecretBox::new(&[7u8; KEY_LEN])
    }

    #[test]
    fn seal_open_round_trips() {
        let sb = box_();
        let secret = b"anilist-oauth-access-token";
        let sealed = sb.seal(secret).unwrap();
        assert_ne!(
            &sealed[NONCE_LEN..],
            secret,
            "ciphertext must not equal plaintext"
        );
        assert_eq!(sb.open(&sealed).unwrap(), secret);
    }

    #[test]
    fn nonce_is_randomised_per_message() {
        let sb = box_();
        let a = sb.seal(b"same").unwrap();
        let b = sb.seal(b"same").unwrap();
        assert_ne!(
            a, b,
            "identical plaintext must not produce identical ciphertext"
        );
    }

    #[test]
    fn wrong_key_fails_authentication() {
        let sealed = box_().seal(b"secret").unwrap();
        let other = SecretBox::new(&[9u8; KEY_LEN]);
        assert!(matches!(other.open(&sealed), Err(AuthError::Crypto)));
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let sb = box_();
        let mut sealed = sb.seal(b"secret").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(matches!(sb.open(&sealed), Err(AuthError::Crypto)));
    }

    #[test]
    fn truncated_input_is_rejected() {
        let sb = box_();
        assert!(matches!(sb.open(&[0u8; 4]), Err(AuthError::Crypto)));
    }

    #[test]
    fn base64_key_round_trips_and_rejects_bad_input() {
        let raw = [3u8; KEY_LEN];
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        let sb = SecretBox::from_base64_key(&encoded).unwrap();
        let sealed = sb.seal(b"x").unwrap();
        assert_eq!(sb.open(&sealed).unwrap(), b"x");

        assert!(matches!(
            SecretBox::from_base64_key("not-base64!!"),
            Err(AuthError::InvalidKey)
        ));
        // Valid base64 but wrong length.
        let short = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        assert!(matches!(
            SecretBox::from_base64_key(&short),
            Err(AuthError::InvalidKey)
        ));
    }
}
