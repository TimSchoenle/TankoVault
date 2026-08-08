//! Framework-agnostic authentication primitives (design §16): argon2id password hashing, HS256
//! access tokens, opaque refresh tokens, AES-256-GCM sealing for tokens at rest, and the
//! second-factor primitives (RFC 6238 TOTP, recovery codes). Every credential type is
//! `secrecy`-wrapped; non-secret derived values (PHC hash, token digest, ciphertext) keep plain
//! types deliberately.
//!
//! Nothing here knows about HTTP, axum or the database. The relying-party half of `WebAuthn` is
//! deliberately *not* here — it lives in `services/api`, because this crate is a dependency of
//! `services/sync`, `xtask` and the fuzz targets, none of which will ever verify a credential.

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
