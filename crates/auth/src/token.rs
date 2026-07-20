//! JWT access tokens and opaque refresh tokens.

use crate::error::AuthError;
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use tankovault_domain::{UserId, UserRole};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::str::FromStr;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Claims embedded in a short-lived access token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessClaims {
    /// Subject — the user id (UUID string).
    pub sub: String,
    /// RBAC role token (`user`/`operator`/`admin`).
    pub role: String,
    /// Expiry (unix seconds).
    pub exp: i64,
    /// Issued-at (unix seconds).
    pub iat: i64,
    /// Token id (for tracing/audit).
    pub jti: String,
}

impl AccessClaims {
    /// Parse the subject into a typed [`UserId`].
    #[must_use]
    pub fn user_id(&self) -> Option<UserId> {
        Uuid::from_str(&self.sub).ok().map(UserId::from_uuid)
    }

    /// Parse the RBAC role, defaulting to the least-privileged role on error.
    #[must_use]
    pub fn role(&self) -> UserRole {
        UserRole::from_str(&self.role).unwrap_or(UserRole::User)
    }
}

/// Issue a signed HS256 access token valid for `ttl`.
///
/// # Errors
/// [`AuthError::TokenIssue`] on encoding failure.
pub fn issue_access_token(
    secret: &[u8],
    user_id: UserId,
    role: UserRole,
    ttl: Duration,
) -> Result<String, AuthError> {
    let now = OffsetDateTime::now_utc();
    let claims = AccessClaims {
        sub: user_id.to_string(),
        role: role.as_str().to_owned(),
        iat: now.unix_timestamp(),
        exp: (now + ttl).unix_timestamp(),
        jti: Uuid::now_v7().to_string(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|_| AuthError::TokenIssue)
}

/// Verify and decode an access token, enforcing expiry and HS256.
///
/// # Errors
/// [`AuthError::InvalidToken`] if the signature, algorithm, or expiry check fails.
pub fn verify_access_token(secret: &[u8], token: &str) -> Result<AccessClaims, AuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    decode::<AccessClaims>(token, &DecodingKey::from_secret(secret), &validation)
        .map(|data| data.claims)
        .map_err(|_| AuthError::InvalidToken)
}

/// Generate a fresh, high-entropy opaque refresh token (URL-safe base64, no padding).
/// The raw value is returned to the client (as an httpOnly cookie); only its hash is
/// persisted.
#[must_use]
pub fn generate_refresh_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// SHA-256 hash (hex) of a refresh token — the only representation stored server-side.
#[must_use]
pub fn hash_refresh_token(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_round_trips_with_role() {
        let secret = b"test-secret-please-rotate";
        let uid = UserId::new();
        let token =
            issue_access_token(secret, uid, UserRole::Operator, Duration::minutes(15)).unwrap();
        let claims = verify_access_token(secret, &token).unwrap();
        assert_eq!(claims.user_id(), Some(uid));
        assert_eq!(claims.role(), UserRole::Operator);
    }

    #[test]
    fn tampered_token_is_rejected() {
        let token = issue_access_token(
            b"secret-a",
            UserId::new(),
            UserRole::User,
            Duration::minutes(5),
        )
        .unwrap();
        assert!(verify_access_token(b"secret-b", &token).is_err());
    }

    #[test]
    fn expired_token_is_rejected() {
        // Well past the default 60s clock-skew leeway.
        let token = issue_access_token(
            b"secret",
            UserId::new(),
            UserRole::User,
            Duration::seconds(-120),
        )
        .unwrap();
        assert!(verify_access_token(b"secret", &token).is_err());
    }

    #[test]
    fn refresh_token_hash_is_stable_and_hides_raw() {
        let raw = generate_refresh_token();
        assert_eq!(hash_refresh_token(&raw), hash_refresh_token(&raw));
        assert_ne!(hash_refresh_token(&raw), raw);
        assert_eq!(hash_refresh_token(&raw).len(), 64); // 32 bytes hex
    }
}
