//! Owns AES-256-GCM sealing for secrets at rest (`docs/ARCHITECTURE.md` §16).
//!
//! OAuth tokens are sealed before they reach the database, so a database compromise alone does
//! not disclose them. The type is `Sealer`, not `SecretBox`, to avoid confusion with
//! [`secrecy::SecretBox`].

use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};
use aes_gcm::{Aes256Gcm, Key};
use base64::Engine;
use rand::RngExt as _;
use secrecy::{ExposeSecret as _, SecretSlice, SecretString};

use crate::AuthError;

/// The AES-GCM nonce length: 96 bits.
const NONCE_LEN: usize = 12;
/// The AES-256 key length.
const KEY_LEN: usize = 32;
/// Leading byte of a value from [`Sealer::seal_bound`]; [`Sealer::seal`] output has no header.
const BOUND_V1: u8 = 1;

/// Seals and opens values with AES-256-GCM under one data-encryption key.
///
/// Cheap to clone, because the key schedule is shared, so one instance lives in service state
/// and serves every request.
#[derive(Clone)]
pub struct Sealer {
    cipher: Aes256Gcm,
}

impl Sealer {
    /// Builds a sealer from a raw 32-byte key.
    ///
    /// Prefer [`from_key_bytes`](Self::from_key_bytes) or
    /// [`from_base64_key`](Self::from_base64_key), which take the key as a `secrecy` type that
    /// zeroizes it when its owner drops it; this one serves tests and fixed-size KMS callers.
    #[must_use]
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        let key = Key::<Aes256Gcm>::from(*key);
        Self {
            cipher: Aes256Gcm::new(&key),
        }
    }

    /// Builds a sealer from key material of unchecked length, as a secret store delivers it.
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

    /// Builds a sealer from a 32-byte key in standard base64.
    ///
    /// It decodes into a [`SecretSlice`], so the intermediate copy is wiped instead of left in a
    /// freed allocation.
    ///
    /// # Errors
    /// [`AuthError::InvalidKey`] if the value is not valid base64 or does not decode to exactly 32
    /// bytes.
    pub fn from_base64_key(encoded: &SecretString) -> Result<Self, AuthError> {
        let decoded: SecretSlice<u8> = base64::engine::general_purpose::STANDARD
            .decode(encoded.expose_secret().trim())
            .map_err(|_| AuthError::InvalidKey)?
            .into();
        Self::from_key_bytes(&decoded)
    }

    /// Seals `plaintext`, returning the 12-byte nonce followed by the ciphertext and tag.
    ///
    /// The output is self-describing, so [`open`](Self::open) needs only the key. It is not
    /// wrapped: it is ciphertext, headed for a database column.
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the AEAD provider fails, for example on allocation.
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, AuthError> {
        // Drawn whole from the CSPRNG: a nonce repeated under one GCM key discloses the key
        // stream and lets an attacker forge tags.
        let nonce_bytes: [u8; NONCE_LEN] = rand::rng().random();
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

    /// Seals the UTF-8 bytes of a secret string, such as an OAuth token.
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the AEAD provider fails.
    pub fn seal_string(&self, plaintext: &SecretString) -> Result<Vec<u8>, AuthError> {
        self.seal(plaintext.expose_secret().as_bytes())
    }

    /// Seals `plaintext` bound to `aad`: `[BOUND_V1] || nonce || ciphertext+tag`.
    ///
    /// The associated data names the value's owner, so a ciphertext copied into another row
    /// fails to open there even though the key is the same.
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the AEAD provider fails.
    pub fn seal_bound(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, AuthError> {
        let nonce_bytes: [u8; NONCE_LEN] = rand::rng().random();
        let nonce = Nonce::<Aes256Gcm>::from(nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| AuthError::Crypto)?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        out.push(BOUND_V1);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Seals a secret string bound to `aad`; see [`seal_bound`](Self::seal_bound).
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the AEAD provider fails.
    pub fn seal_string_bound(
        &self,
        plaintext: &SecretString,
        aad: &[u8],
    ) -> Result<Vec<u8>, AuthError> {
        self.seal_bound(plaintext.expose_secret().as_bytes(), aad)
    }

    /// Opens a value produced by [`seal_bound`](Self::seal_bound) under the same `aad`.
    ///
    /// Strict: an unbound [`seal`](Self::seal) value is refused, so a caller that accepts
    /// legacy values has to fall back to [`open`](Self::open) explicitly.
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the header is missing, the input is truncated, or authentication
    /// fails (wrong key, wrong `aad`, tampering).
    pub fn open_bound(&self, sealed: &[u8], aad: &[u8]) -> Result<SecretSlice<u8>, AuthError> {
        let Some((&BOUND_V1, rest)) = sealed.split_first() else {
            return Err(AuthError::Crypto);
        };
        if rest.len() < NONCE_LEN {
            return Err(AuthError::Crypto);
        }
        let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).map_err(|_| AuthError::Crypto)?;
        self.cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map(SecretSlice::from)
            .map_err(|_| AuthError::Crypto)
    }

    /// Opens a [`seal_string_bound`](Self::seal_string_bound) value and decodes it as UTF-8.
    ///
    /// # Errors
    /// [`AuthError::Crypto`], for the same reasons as [`open_string`](Self::open_string).
    pub fn open_string_bound(&self, sealed: &[u8], aad: &[u8]) -> Result<SecretString, AuthError> {
        utf8_secret(&self.open_bound(sealed, aad)?)
    }

    /// Opens a value produced by [`seal`](Self::seal).
    ///
    /// The plaintext is returned as a [`SecretSlice`]; for text, use
    /// [`open_string`](Self::open_string).
    ///
    /// # Errors
    /// [`AuthError::Crypto`] if the input is shorter than the nonce or authentication fails (wrong
    /// key, tampering or truncation).
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

    /// Opens a value produced by [`seal_string`](Self::seal_string) and decodes it as UTF-8.
    ///
    /// Invalid UTF-8 is reported as [`AuthError::Crypto`] too, so an error cannot describe a
    /// secret's byte shape.
    ///
    /// # Errors
    /// [`AuthError::Crypto`].
    pub fn open_string(&self, sealed: &[u8]) -> Result<SecretString, AuthError> {
        utf8_secret(&self.open(sealed)?)
    }
}

/// Decodes opened bytes as UTF-8, reporting failure as [`AuthError::Crypto`].
fn utf8_secret(opened: &SecretSlice<u8>) -> Result<SecretString, AuthError> {
    let text = core::str::from_utf8(opened.expose_secret()).map_err(|_| AuthError::Crypto)?;
    Ok(SecretString::from(text))
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hand-written: this holds a key schedule, not key bytes, so there is no `SecretBox`
        // to delegate the redaction to.
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

    /// `open_string` reports invalid UTF-8 as [`AuthError::Crypto`], so an error cannot
    /// describe a decrypted secret's byte shape.
    #[test]
    fn non_utf8_plaintext_is_a_crypto_error_not_a_decoding_one() {
        let s = sealer();
        let sealed = s.seal(&[0xff, 0xfe, 0xfd]).unwrap();
        assert!(matches!(s.open_string(&sealed), Err(AuthError::Crypto)));
    }

    /// The bug: values were sealed without associated data, so a ciphertext copied into another
    /// user's row opened there. A bound value opens only under the `aad` it was sealed with.
    #[test]
    fn a_bound_value_opens_only_under_its_own_aad() {
        let s = sealer();
        let sealed = s.seal_bound(b"token", b"user-a/anilist").unwrap();
        assert_eq!(
            s.open_bound(&sealed, b"user-a/anilist")
                .unwrap()
                .expose_secret(),
            b"token"
        );
        for wrong in [&b"user-b/anilist"[..], b"user-a/mal", b""] {
            assert!(matches!(
                s.open_bound(&sealed, wrong),
                Err(AuthError::Crypto)
            ));
        }
        // Nor does it open as an unbound value.
        assert!(matches!(s.open(&sealed), Err(AuthError::Crypto)));
    }

    /// Strict opening refuses an unbound value, so a legacy fallback is always explicit.
    #[test]
    fn open_bound_refuses_an_unbound_value() {
        let s = sealer();
        let legacy = s.seal(b"token").unwrap();
        assert!(matches!(s.open_bound(&legacy, b""), Err(AuthError::Crypto)));
        assert!(matches!(s.open_bound(&[], b""), Err(AuthError::Crypto)));
        assert!(matches!(
            s.open_bound(&[BOUND_V1, 0, 0], b""),
            Err(AuthError::Crypto)
        ));
    }

    /// A `tracing::debug!(?state)` cannot print the key.
    #[test]
    fn debug_is_redacted() {
        assert_eq!(format!("{:?}", sealer()), "Sealer(<redacted>)");
    }
}
