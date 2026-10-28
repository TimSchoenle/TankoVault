//! Owns JWT access tokens and opaque refresh tokens.

use crate::error::AuthError;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use secrecy::{ExposeSecret as _, SecretSlice, SecretString};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tankovault_domain::UserId;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// The claims in a short-lived access token.
///
/// Carries **no authorization state**: a claim is fixed at minting, so an embedded privilege
/// would keep working after revocation until expiry. Authorization resolves the caller's grants
/// from the database per request. [`Self::name`] is cosmetic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessClaims {
    /// The subject: the user id as a UUID string.
    pub sub: String,
    /// The username, so the client can show it without a round trip. Cosmetic; empty when
    /// absent.
    #[serde(default)]
    pub name: String,
    /// The expiry, in unix seconds.
    pub exp: i64,
    /// The issue time, in unix seconds.
    pub iat: i64,
    /// The token id, for tracing and audit.
    pub jti: String,
}

impl AccessClaims {
    /// Parses the subject into a typed [`UserId`].
    #[must_use]
    pub fn user_id(&self) -> Option<UserId> {
        Uuid::from_str(&self.sub).ok().map(UserId::from_uuid)
    }
}

/// Issues a signed HS256 access token valid for `ttl`.
///
/// A minted token is a bearer credential as sensitive as the password that bought it, so it is
/// a [`SecretString`] and stays out of tracing fields and error bodies.
///
/// # Errors
/// [`AuthError::TokenIssue`] if encoding fails.
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

/// Verifies and decodes an access token, enforcing expiry and a pinned HS256.
///
/// The algorithm is pinned, never read from the token's header: trusting the header is the JWT
/// confusion attack (`alg` swapped to `none` or `RS256`). This is the whole of the API's
/// authentication; there is no second check behind it.
///
/// ```
/// use secrecy::{ExposeSecret, SecretSlice};
/// use tankovault_auth::{issue_access_token, verify_access_token};
/// use tankovault_domain::UserId;
/// use time::Duration;
///
/// // A `SecretSlice<u8>` cannot be `dbg!`ed, and every read is a visible `expose_secret()`.
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
/// // Expiry is enforced only here; the caller chooses the TTL, so issuing an expired token
/// // is legal.
/// let stale = issue_access_token(&secret, user, "alice", Duration::seconds(-120))?;
/// assert!(verify_access_token(&secret, stale.expose_secret()).is_err());
///
/// // A token one second past `exp` still verifies, under `jsonwebtoken`'s 60s clock-skew
/// // leeway: replicas share the secret but not a clock. Tokens outlive their TTL by up to a
/// // minute, so a TTL near that margin needs the leeway revisited.
/// let barely_stale = issue_access_token(&secret, user, "alice", Duration::seconds(-1))?;
/// assert!(verify_access_token(&secret, barely_stale.expose_secret()).is_ok());
///
/// // Garbage is rejected. A validly signed token with a non-UUID `sub` verifies and yields
/// // no user, which is why `user_id()` returns an `Option`.
/// assert!(verify_access_token(&secret, "not.a.token").is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// `token` is a `&str`: it is borrowed from an `Authorization` header per request, and the
/// caller can neither own nor zeroize it, so wrapping would cost an allocation for no
/// protection. The long-lived signing key is wrapped.
///
/// # Errors
/// [`AuthError::InvalidToken`] if the signature, algorithm or expiry check fails.
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

/// Generates a fresh opaque refresh token.
///
/// The raw value goes to the client in an httpOnly cookie, and only its hash is persisted. It
/// outlives an access token by weeks, so it is the more valuable of the two to leak.
#[must_use]
pub fn generate_refresh_token() -> SecretString {
    crate::opaque::generate_handle()
}

/// Returns the hex SHA-256 of a refresh token, the only form stored server-side.
///
/// Delegates to [`crate::opaque::hash_handle`]: every opaque credential must share one stored
/// encoding, so there is exactly one implementation of it.
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
        // A token cannot be un-issued, so no role or permission claim may travel in one.
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
