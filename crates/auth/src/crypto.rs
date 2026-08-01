//! Authenticated symmetric encryption for secrets at rest (design §16).
//!
//! [`Sealer`] wraps AES-256-GCM. External-provider credentials — `AniList` OAuth
//! access/refresh tokens — are sealed before they touch the database, so a database
//! compromise alone does not disclose them. A fresh random 96-bit nonce is generated per
//! message and prepended to the ciphertext, and the data-encryption key is supplied by the
//! caller from a secret store / KMS — never hard-coded and never persisted alongside the
//! ciphertext.
//!
//! The type was called `SecretBox` until `secrecy` was adopted workspace-wide, where
//! [`secrecy::SecretBox`] is the *container* for a secret value. Two types a use statement
//! apart, one meaning "this value is secret" and the other "this key seals other values", is
//! how an import gets written against the wrong one. Sealing is what this does, so `Sealer`
//! is what it is called.

use aes_gcm::aead::{Aead, KeyInit, Nonce};
use aes_gcm::{Aes256Gcm, Key};
use base64::Engine;
use rand::Rng as _;
use secrecy::{ExposeSecret as _, SecretSlice, SecretString};

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
pub struct Sealer {
    cipher: Aes256Gcm,
}

impl Sealer {
    /// Build from a raw 32-byte data-encryption key.
    ///
    /// The array is the caller's to zeroize; prefer [`from_key_bytes`](Self::from_key_bytes)
    /// or [`from_base64_key`](Self::from_base64_key), which keep the key inside a
    /// zeroize-on-drop wrapper for its whole life. This entry point stays for tests and for
    /// callers that already hold a fixed-size key from a KMS SDK.
    #[must_use]
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        let key = Key::<Aes256Gcm>::from(*key);
        Self {
            cipher: Aes256Gcm::new(&key),
        }
    }

    /// Build from key material of unchecked length, as delivered by a secret store.
    ///
    /// # Errors
    /// [`AuthError::InvalidKey`] if the material is not exactly 32 bytes.
    pub fn from_key_bytes(key: &SecretSlice<u8>) -> Result<Self, AuthError> {
        let key: &[u8; KEY_LEN] = key
            .expose_secret()
            .try_into()
            .map_err(|_| AuthError::InvalidKey)?;
        Ok(Self::new(key))
    }

    /// Build from a base64 (standard alphabet) encoded 32-byte key, as delivered by a
    /// secret store or environment variable.
    ///
    /// The decoded bytes land in a [`SecretSlice`] rather than a bare `Vec<u8>`, so the
    /// intermediate copy of the key is wiped when this function returns rather than being
    /// left in a freed allocation.
    ///
    /// # Errors
    /// [`AuthError::InvalidKey`] if the value is not valid base64 or does not decode to
    /// exactly 32 bytes.
    pub fn from_base64_key(encoded: &SecretString) -> Result<Self, AuthError> {
        let decoded: SecretSlice<u8> = base64::engine::general_purpose::STANDARD
            .decode(encoded.expose_secret().trim())
            .map_err(|_| AuthError::InvalidKey)?
            .into();
        Self::from_key_bytes(&decoded)
    }

    /// Seal `plaintext`, returning `nonce (12 bytes) || ciphertext-with-tag`. The output
    /// is self-describing: [`open`](Self::open) needs only the sealing key.
    ///
    /// The result is ciphertext, so it is deliberately *not* wrapped — it is what gets
    /// written to a database column and logged as a length.
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

    /// Seal the UTF-8 bytes of a secret string. The convenience half of [`seal`](Self::seal),
    /// which is what every caller in this workspace actually wants: the things sealed here are
    /// OAuth tokens, and a token is a [`SecretString`].
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the AEAD provider fails.
    pub fn seal_string(&self, plaintext: &SecretString) -> Result<Vec<u8>, AuthError> {
        self.seal(plaintext.expose_secret().as_bytes())
    }

    /// Open a value produced by [`seal`](Self::seal).
    ///
    /// The plaintext comes back in a [`SecretSlice`] because that is what it is: whatever was
    /// worth encrypting at rest is worth not leaving in a `Vec<u8>` afterwards. Callers that
    /// need text want [`open_string`](Self::open_string).
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the input is shorter than the nonce or authentication
    /// fails (wrong key, tampering, or truncation).
    pub fn open(&self, sealed: &[u8]) -> Result<SecretSlice<u8>, AuthError> {
        if sealed.len() < NONCE_LEN {
            return Err(AuthError::Crypto);
        }
        let (nonce_bytes, ciphertext) = sealed.split_at(NONCE_LEN);
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).map_err(|_| AuthError::Crypto)?;
        self.cipher
            .decrypt(&nonce, ciphertext)
            .map(SecretSlice::from)
            .map_err(|_| AuthError::Crypto)
    }

    /// Open a value produced by [`seal_string`](Self::seal_string) and decode it as UTF-8.
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if authentication fails **or** the plaintext is not valid UTF-8.
    /// The two are one variant on purpose: both mean "this ciphertext was not written by this
    /// key against this schema", and distinguishing them for a caller only produces an error
    /// message that describes the shape of a secret.
    pub fn open_string(&self, sealed: &[u8]) -> Result<SecretString, AuthError> {
        let opened = self.open(sealed)?;
        let text = core::str::from_utf8(opened.expose_secret()).map_err(|_| AuthError::Crypto)?;
        Ok(SecretString::from(text))
    }
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render key material. Hand-written rather than delegated to `secrecy`: what is
        // held is an `Aes256Gcm` key *schedule*, not the key bytes, so there is no
        // `SecretBox` to put it in — the redaction has to be written here.
        f.write_str("Sealer(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealer() -> Sealer {
        Sealer::new(&[7u8; KEY_LEN])
    }

    #[test]
    fn seal_open_round_trips() {
        let s = sealer();
        let secret = b"anilist-oauth-access-token";
        let sealed = s.seal(secret).unwrap();
        assert_ne!(
            &sealed[NONCE_LEN..],
            secret,
            "ciphertext must not equal plaintext"
        );
        assert_eq!(s.open(&sealed).unwrap().expose_secret(), secret);
    }

    #[test]
    fn nonce_is_randomised_per_message() {
        let s = sealer();
        let a = s.seal(b"same").unwrap();
        let b = s.seal(b"same").unwrap();
        assert_ne!(
            a, b,
            "identical plaintext must not produce identical ciphertext"
        );
    }

    #[test]
    fn wrong_key_fails_authentication() {
        let sealed = sealer().seal(b"secret").unwrap();
        let other = Sealer::new(&[9u8; KEY_LEN]);
        assert!(matches!(other.open(&sealed), Err(AuthError::Crypto)));
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let s = sealer();
        let mut sealed = s.seal(b"secret").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(matches!(s.open(&sealed), Err(AuthError::Crypto)));
    }

    #[test]
    fn truncated_input_is_rejected() {
        let s = sealer();
        assert!(matches!(s.open(&[0u8; 4]), Err(AuthError::Crypto)));
    }

    #[test]
    fn base64_key_round_trips_and_rejects_bad_input() {
        let raw = [3u8; KEY_LEN];
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        let s = Sealer::from_base64_key(&SecretString::from(encoded)).unwrap();
        let sealed = s.seal(b"x").unwrap();
        assert_eq!(s.open(&sealed).unwrap().expose_secret(), b"x");

        assert!(matches!(
            Sealer::from_base64_key(&SecretString::from("not-base64!!")),
            Err(AuthError::InvalidKey)
        ));
        // Valid base64 but wrong length.
        let short = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        assert!(matches!(
            Sealer::from_base64_key(&SecretString::from(short)),
            Err(AuthError::InvalidKey)
        ));
    }

    #[test]
    fn string_round_trip_carries_the_secret_wrapper_through() {
        let s = sealer();
        let token = SecretString::from("anilist-refresh-token");
        let sealed = s.seal_string(&token).unwrap();
        assert_eq!(
            s.open_string(&sealed).unwrap().expose_secret(),
            "anilist-refresh-token"
        );
    }

    /// `open_string` folds "not valid UTF-8" into [`AuthError::Crypto`] rather than reporting
    /// it separately. Both answers mean the same thing operationally — this ciphertext does
    /// not belong to this key and schema — and a distinct variant would let an error message
    /// describe the byte shape of a decrypted secret.
    #[test]
    fn non_utf8_plaintext_is_a_crypto_error_not_a_decoding_one() {
        let s = sealer();
        let sealed = s.seal(&[0xff, 0xfe, 0xfd]).unwrap();
        assert!(matches!(s.open_string(&sealed), Err(AuthError::Crypto)));
    }

    /// The whole point of the type is that a `tracing::debug!(?state)` cannot print the key.
    #[test]
    fn debug_is_redacted() {
        assert_eq!(format!("{:?}", sealer()), "Sealer(<redacted>)");
    }
}
