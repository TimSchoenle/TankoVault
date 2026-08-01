//! Authenticated symmetric encryption (AES-256-GCM) for secrets at rest (design §16): OAuth
//! tokens are sealed before they touch the database, so a database compromise alone cannot
//! disclose them. Named `Sealer`, not `SecretBox`, to avoid confusion with [`secrecy::SecretBox`].

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
    /// Build from a raw 32-byte key. Prefer [`from_key_bytes`](Self::from_key_bytes) or
    /// [`from_base64_key`](Self::from_base64_key), which zeroize the key; this entry point
    /// stays for tests and fixed-size KMS callers.
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

    /// Build from a base64 (standard alphabet) encoded 32-byte key.
    ///
    /// Decodes into a [`SecretSlice`], not a bare `Vec<u8>`, so the intermediate copy is
    /// wiped rather than left in a freed allocation.
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

    /// Seal `plaintext`, returning `nonce (12 bytes) || ciphertext-with-tag` — self-describing,
    /// so [`open`](Self::open) needs only the key. Not wrapped: this is ciphertext, headed for
    /// a database column and logged only as a length.
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

    /// Seal the UTF-8 bytes of a secret string — the convenience most callers want, since
    /// what gets sealed here is an OAuth token.
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the AEAD provider fails.
    pub fn seal_string(&self, plaintext: &SecretString) -> Result<Vec<u8>, AuthError> {
        self.seal(plaintext.expose_secret().as_bytes())
    }

    /// Open a value produced by [`seal`](Self::seal). Returns a [`SecretSlice`], not a
    /// `Vec<u8>` — worth encrypting means worth not leaving unwrapped. Callers that need
    /// text want [`open_string`](Self::open_string).
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
    /// [`AuthError::Crypto`] if authentication fails **or** the plaintext isn't valid UTF-8 —
    /// one variant, so an error message can't describe a secret's byte shape.
    pub fn open_string(&self, sealed: &[u8]) -> Result<SecretString, AuthError> {
        let opened = self.open(sealed)?;
        let text = core::str::from_utf8(opened.expose_secret()).map_err(|_| AuthError::Crypto)?;
        Ok(SecretString::from(text))
    }
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hand-written, not delegated to `secrecy`: this holds a key *schedule*, not key
        // bytes, so there's no `SecretBox` to redact instead.
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

    /// `open_string` folds invalid UTF-8 into [`AuthError::Crypto`] rather than a distinct
    /// variant, so an error can't describe a decrypted secret's byte shape.
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
