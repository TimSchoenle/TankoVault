//! JWT access tokens and opaque refresh tokens.

use crate::error::AuthError;
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::str::FromStr;
use tankovault_domain::UserId;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Claims embedded in a short-lived access token.
///
/// # What is deliberately *not* here
///
/// The token carries **no authorization state**. It used to carry an RBAC role, and the API
/// authorized against that claim. That is a correctness problem, not a style one: a claim is
/// fixed when the token is minted, so revoking a privilege left the holder exercising it until
/// their access token expired, with no in-band way to shorten that window. Authorization now
/// resolves the caller's permission grants from the database on each request
/// (`tankovault_db::repo::permissions::resolve`), which is the only way "revoke now" can mean
/// now.
///
/// [`Self::name`] stays because it is cosmetic and the client is welcome to a stale display
/// name; nothing is decided on its basis.
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
/// # Errors
/// [`AuthError::TokenIssue`] on encoding failure.
pub fn issue_access_token(
    secret: &[u8],
    user_id: UserId,
    username: &str,
    ttl: Duration,
) -> Result<String, AuthError> {
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
        &EncodingKey::from_secret(secret),
    )
    .map_err(|_| AuthError::TokenIssue)
}

/// Verify and decode an access token, enforcing expiry and HS256.
///
/// The algorithm is **pinned**, not read from the token's own header. A verifier that trusts
/// the header is the classic JWT confusion attack: the holder swaps `alg` for `none`, or for
/// `RS256` so the HMAC secret is treated as an RSA public key, and signs their own claims.
/// `crates/db` never sees this token — it is the whole of the API's authentication — so there
/// is no second check behind it.
///
/// ```
/// use tankovault_auth::{issue_access_token, verify_access_token};
/// use tankovault_domain::UserId;
/// use time::Duration;
///
/// let secret = b"a-test-secret-not-a-real-one";
/// let user = UserId::new();
/// let token = issue_access_token(secret, user, "alice", Duration::minutes(15))?;
///
/// // The round trip: the subject survives as a typed id, and the display name rides along so
/// // the client can render it without a round-trip.
/// let claims = verify_access_token(secret, &token)?;
/// assert_eq!(claims.user_id(), Some(user));
/// assert_eq!(claims.name, "alice");
/// assert!(claims.exp > claims.iat);
///
/// // A different secret is a rejection, not a different answer.
/// assert!(verify_access_token(b"some-other-secret", &token).is_err());
///
/// // Expiry is enforced here and nowhere else — issuing an already-expired token is legal,
/// // because the caller chooses the TTL.
/// let stale = issue_access_token(secret, user, "alice", Duration::seconds(-120))?;
/// assert!(verify_access_token(secret, &stale).is_err());
///
/// // …but a token one second past `exp` still verifies. This looks like the expiry check
/// // failing and is `jsonwebtoken`'s 60-second clock-skew leeway, inherited rather than
/// // chosen: replicas share the signing secret but not a clock, so a token issued by one and
/// // verified by another must survive a little drift. The practical effect is that every
/// // access token outlives its stated TTL by up to a minute — acceptable against a 15-minute
/// // TTL, and the number to revisit before anyone shortens that TTL towards it.
/// let barely_stale = issue_access_token(secret, user, "alice", Duration::seconds(-1))?;
/// assert!(verify_access_token(secret, &barely_stale).is_ok());
///
/// // Garbage is a rejection. Note what is *not* rejected: `sub` is a string in the claims, so
/// // a validly-signed token whose subject is not a UUID verifies and then yields no user —
/// // which is why `user_id()` returns `Option` rather than panicking, and why every caller
/// // has to treat an unparseable subject as "no principal".
/// assert!(verify_access_token(secret, "not.a.token").is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
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
    fn access_token_round_trips_its_identity_claims() {
        let secret = b"test-secret-please-rotate";
        let uid = UserId::new();
        let token = issue_access_token(secret, uid, "aster", Duration::minutes(15)).unwrap();
        let claims = verify_access_token(secret, &token).unwrap();
        assert_eq!(claims.user_id(), Some(uid));
        assert_eq!(claims.name, "aster");
    }

    #[test]
    fn the_token_carries_no_authorization_claim() {
        // Pins the decision in `AccessClaims`' docs: privileges must not be able to travel in
        // a token, because a token cannot be un-issued. If a future change adds a role or
        // permission claim, this fails.
        let token =
            issue_access_token(b"secret", UserId::new(), "aster", Duration::minutes(5)).unwrap();
        let payload = token.split('.').nth(1).expect("a JWT has three parts");
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
        let token =
            issue_access_token(b"secret-a", UserId::new(), "aster", Duration::minutes(5)).unwrap();
        assert!(verify_access_token(b"secret-b", &token).is_err());
    }

    #[test]
    fn expired_token_is_rejected() {
        // Well past the default 60s clock-skew leeway.
        let token =
            issue_access_token(b"secret", UserId::new(), "aster", Duration::seconds(-120)).unwrap();
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
