//! JWT access tokens and opaque refresh tokens.

use crate::error::AuthError;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use secrecy::{ExposeSecret as _, SecretSlice, SecretString};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tankovault_domain::UserId;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Claims embedded in a short-lived access token.
///
/// Carries **no authorization state**: a claim is fixed at minting, so embedding a privilege
/// would let a revoked grant keep working until expiry. Authorization instead resolves the
/// caller's permission grants from the database per request. [`Self::name`] is cosmetic only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessClaims {
    /// Subject — the user id (UUID string).
    pub sub: String,
    /// Human-facing display name (username). Cosmetic only — lets the client show the
    /// current name without a round-trip. Absent on legacy tokens (defaults to empty).
    #[serde(default)]
    pub name: String,
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
}

/// Issue a signed HS256 access token valid for `ttl`.
///
/// Returns a [`SecretString`]: a minted token is a bearer credential as sensitive as the
/// password that bought it, so wrapping it keeps it out of tracing fields and error bodies.
///
/// # Errors
/// [`AuthError::TokenIssue`] on encoding failure.
pub fn issue_access_token(
    secret: &SecretSlice<u8>,
    user_id: UserId,
    username: &str,
    ttl: Duration,
) -> Result<SecretString, AuthError> {
    let now = OffsetDateTime::now_utc();
    let claims = AccessClaims {
        sub: user_id.to_string(),
        name: username.to_owned(),
        iat: now.unix_timestamp(),
        exp: (now + ttl).unix_timestamp(),
        jti: Uuid::now_v7().to_string(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.expose_secret()),
    )
    .map(SecretString::from)
    .map_err(|_| AuthError::TokenIssue)
}

/// Verify and decode an access token, enforcing expiry and pinned HS256.
///
/// The algorithm is pinned, not read from the token's header — trusting the header is the
/// classic JWT confusion attack (swap `alg` to `none` or `RS256`). This is the whole of the
/// API's authentication; there is no second check behind it.
///
/// ```
/// use secrecy::{ExposeSecret, SecretSlice};
/// use tankovault_auth::{issue_access_token, verify_access_token};
/// use tankovault_domain::UserId;
/// use time::Duration;
///
/// // `SecretSlice<u8>`, not `&[u8]`: it can't be `dbg!`ed, and every read below is a visible
/// // `expose_secret()`.
/// let secret = SecretSlice::from(b"a-test-secret-not-a-real-one".to_vec());
/// let user = UserId::new();
/// let token = issue_access_token(&secret, user, "alice", Duration::minutes(15))?;
///
/// let claims = verify_access_token(&secret, token.expose_secret())?;
/// assert_eq!(claims.user_id(), Some(user));
/// assert_eq!(claims.name, "alice");
/// assert!(claims.exp > claims.iat);
///
/// // A different secret is a rejection, not a different answer.
/// let other = SecretSlice::from(b"some-other-secret".to_vec());
/// assert!(verify_access_token(&other, token.expose_secret()).is_err());
///
/// // Expiry is enforced here only — issuing an already-expired token is legal; the caller
/// // chooses the TTL.
/// let stale = issue_access_token(&secret, user, "alice", Duration::seconds(-120))?;
/// assert!(verify_access_token(&secret, stale.expose_secret()).is_err());
///
/// // A token one second past `exp` still verifies: `jsonwebtoken`'s inherited 60s clock-skew
/// // leeway (replicas share the secret but not a clock). Tokens outlive their TTL by up to a
/// // minute — revisit before shortening TTL toward that margin.
/// let barely_stale = issue_access_token(&secret, user, "alice", Duration::seconds(-1))?;
/// assert!(verify_access_token(&secret, barely_stale.expose_secret()).is_ok());
///
/// // Garbage is rejected, but a validly-signed token with a non-UUID `sub` verifies and
/// // yields no user — hence `user_id()` returns `Option` rather than panicking.
/// assert!(verify_access_token(&secret, "not.a.token").is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// `token` is a `&str`, not [`SecretString`]: it arrives borrowed from an `Authorization`
/// header per request, and the caller neither owns nor can zeroize it, so wrapping would cost
/// an allocation for no protection. The long-lived signing key is wrapped; the transient
/// presented token is not.
///
/// # Errors
/// [`AuthError::InvalidToken`] if the signature, algorithm, or expiry check fails.
pub fn verify_access_token(
    secret: &SecretSlice<u8>,
    token: &str,
) -> Result<AccessClaims, AuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    decode::<AccessClaims>(
        token,
        &DecodingKey::from_secret(secret.expose_secret()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|_| AuthError::InvalidToken)
}

/// Generate a fresh, high-entropy opaque refresh token (URL-safe base64, no padding); the raw
/// value goes to the client (httpOnly cookie), only its hash is persisted.
///
/// Outlives an access token by weeks, so it's the more valuable of the two to leak — hence
/// [`SecretString`] on the way out.
#[must_use]
pub fn generate_refresh_token() -> SecretString {
    crate::opaque::generate_handle()
}

/// SHA-256 hash (hex) of a refresh token — the only representation stored server-side.
///
/// Not wrapped: the digest discloses nothing about the raw token, and wrapping it would put
/// `expose_secret()` on every database call.
///
/// Delegates to [`crate::opaque::hash_handle`], which every other opaque credential in this
/// system is stored through. The encoding is load-bearing across all of them — see that
/// module — so there is exactly one implementation of it.
#[must_use]
pub fn hash_refresh_token(raw: &SecretString) -> String {
    crate::opaque::hash_handle(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the claims test needs it, to decode a JWT payload the crate itself never parses.
    use base64::Engine as _;

    fn key(bytes: &[u8]) -> SecretSlice<u8> {
        SecretSlice::from(bytes.to_vec())
    }

    #[test]
    fn access_token_round_trips_its_identity_claims() {
        let secret = key(b"test-secret-please-rotate");
        let uid = UserId::new();
        let token = issue_access_token(&secret, uid, "aster", Duration::minutes(15)).unwrap();
        let claims = verify_access_token(&secret, token.expose_secret()).unwrap();
        assert_eq!(claims.user_id(), Some(uid));
        assert_eq!(claims.name, "aster");
    }

    #[test]
    fn the_token_carries_no_authorization_claim() {
        // Pins `AccessClaims`' contract: a token can't be un-issued, so no role/permission
        // claim may ever travel in one.
        let token = issue_access_token(
            &key(b"secret"),
            UserId::new(),
            "aster",
            Duration::minutes(5),
        )
        .unwrap();
        let payload = token
            .expose_secret()
            .split('.')
            .nth(1)
            .expect("a JWT has three parts");
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .expect("the payload is base64url");
        let json: serde_json::Value =
            serde_json::from_slice(&decoded).expect("the payload is JSON");
        let claims = json.as_object().expect("claims are an object");
        for forbidden in ["role", "roles", "perms", "permissions", "scope"] {
            assert!(
                !claims.contains_key(forbidden),
                "access tokens must not carry authorization state ({forbidden})"
            );
        }
    }

    #[test]
    fn tampered_token_is_rejected() {
        let token = issue_access_token(
            &key(b"secret-a"),
            UserId::new(),
            "aster",
            Duration::minutes(5),
        )
        .unwrap();
        assert!(verify_access_token(&key(b"secret-b"), token.expose_secret()).is_err());
    }

    #[test]
    fn expired_token_is_rejected() {
        // Well past the default 60s clock-skew leeway.
        let token = issue_access_token(
            &key(b"secret"),
            UserId::new(),
            "aster",
            Duration::seconds(-120),
        )
        .unwrap();
        assert!(verify_access_token(&key(b"secret"), token.expose_secret()).is_err());
    }

    #[test]
    fn refresh_token_hash_is_stable_and_hides_raw() {
        let raw = generate_refresh_token();
        assert_eq!(hash_refresh_token(&raw), hash_refresh_token(&raw));
        assert_ne!(hash_refresh_token(&raw), raw.expose_secret());
        assert_eq!(hash_refresh_token(&raw).len(), 64); // 32 bytes hex
    }

    /// Neither credential type this module mints may be printable: `secrecy`'s `Debug` renders
    /// the type, never the value.
    #[test]
    fn minted_credentials_are_redacted_in_debug() {
        let token = issue_access_token(
            &key(b"secret"),
            UserId::new(),
            "aster",
            Duration::minutes(5),
        )
        .unwrap();
        assert!(format!("{token:?}").contains("REDACTED"));
        assert!(!format!("{token:?}").contains(token.expose_secret()));

        let refresh = generate_refresh_token();
        assert!(format!("{refresh:?}").contains("REDACTED"));
        assert!(!format!("{refresh:?}").contains(refresh.expose_secret()));
    }
}
