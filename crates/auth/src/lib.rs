//! # tankovault-auth
//!
//! Authentication primitives (design §16), framework-agnostic so both the API's Axum
//! extractors and tests can use them:
//! - [`hash_password`]/[`verify_password`] — argon2id.
//! - [`issue_access_token`]/[`verify_access_token`] — short-lived HS256 JWT.
//! - [`generate_refresh_token`]/[`hash_refresh_token`] — opaque token; only its SHA-256
//!   hash is ever stored, enabling rotation + reuse detection at the DB layer.
//! - [`AccessClaims`] carries **identity only**, never privileges: authorization resolves the
//!   caller's `tankovault_domain::Permission` grants per request, so a revocation takes effect
//!   immediately instead of outliving the access token that embedded it.
//! - [`Sealer`] — AES-256-GCM sealing for external-provider tokens at rest (§16).
//!
//! # Secrets are typed
//!
//! Nothing in this crate accepts or returns a bare `String` where the value is a credential.
//! The signing key and the pepper are [`secrecy::SecretSlice<u8>`], the plaintext password and
//! the minted tokens are [`secrecy::SecretString`], and reading any of them is an explicit
//! `expose_secret()`. Values that are *not* secret keep their plain types deliberately — the
//! argon2 PHC string, the SHA-256 refresh-token digest, and the AEAD ciphertext are all things
//! that get written to the database and logged, and wrapping them would make the wrapper mean
//! "this came from `crates/auth`" instead of "this must not leak".

mod crypto;
mod error;
mod password;
mod token;

pub use crypto::Sealer;
pub use error::AuthError;
pub use password::{hash_password, verify_password};
pub use token::{
    AccessClaims, generate_refresh_token, hash_refresh_token, issue_access_token,
    verify_access_token,
};
