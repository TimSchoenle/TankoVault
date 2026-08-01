//! Framework-agnostic authentication primitives (design §16): argon2id password hashing, HS256
//! access tokens, opaque refresh tokens, and AES-256-GCM sealing for tokens at rest. Every
//! credential type is `secrecy`-wrapped; non-secret derived values (PHC hash, token digest,
//! ciphertext) keep plain types deliberately.

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
