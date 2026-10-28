//! Owns the framework-agnostic authentication primitives (`docs/ARCHITECTURE.md` §16): argon2id
//! password hashing, HS256 access tokens, opaque refresh tokens, AES-256-GCM sealing for tokens
//! at rest, and the second factors (RFC 6238 TOTP, recovery codes).
//!
//! Every credential is `secrecy`-wrapped; derived values that are not secrets (a PHC hash, a
//! token digest, ciphertext) keep plain types on purpose. Nothing here knows about HTTP, axum or
//! the database. The `WebAuthn` relying party lives in `services/api`, because `services/sync`,
//! `xtask` and the fuzz targets depend on this crate and never verify a credential.

mod crypto;
mod error;
mod opaque;
mod password;
pub mod recovery;
mod token;
pub mod totp;

pub use crypto::Sealer;
pub use error::AuthError;
pub use opaque::{generate_handle, hash_handle};
pub use password::{hash_password, verify_password};
pub use token::{
    AccessClaims, generate_refresh_token, hash_refresh_token, issue_access_token,
    verify_access_token,
};
