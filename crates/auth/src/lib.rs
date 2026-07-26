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
//! - [`SecretBox`] — AES-256-GCM sealing for external-provider tokens at rest (§16).

mod crypto;
mod error;
mod password;
mod token;

pub use crypto::SecretBox;
pub use error::AuthError;
pub use password::{hash_password, verify_password};
pub use token::{
    AccessClaims, generate_refresh_token, hash_refresh_token, issue_access_token,
    verify_access_token,
};
