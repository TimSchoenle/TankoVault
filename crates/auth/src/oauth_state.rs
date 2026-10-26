//! Signed OAuth `state` values, binding a consent round trip to the user who started it.
//!
//! Without a `state` an attacker can complete *their own* consent, stop before the redirect,
//! and get a victim's browser to land on the callback with the attacker's `code`: the victim's
//! account is then linked to the attacker's tracker and every later push writes the victim's
//! reading history into it (RFC 6749 §10.12). The value here is stateless — an expiry plus an
//! HMAC over the user, the provider and that expiry — so a `state` minted for one account is
//! refused for any other, and nothing has to be stored between the two requests.

use base64::Engine as _;
use hmac::{Hmac, KeyInit as _, Mac as _};
use secrecy::{ExposeSecret as _, SecretSlice};
use sha2::Sha256;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::AuthError;

/// How long a consent round trip may take.
pub const OAUTH_STATE_TTL: time::Duration = time::Duration::minutes(10);

/// Separates this MAC's key from every other use of the signing secret (the HS256 access token
/// is keyed by the same bytes).
const KEY_LABEL: &[u8] = b"tankovault oauth-state v1";

const EXPIRY_LEN: usize = 8;
const MAC_LEN: usize = 32;

type HmacSha256 = Hmac<Sha256>;

/// Mint a `state` for `user_id` linking `provider`, valid for [`OAUTH_STATE_TTL`] from `now`.
#[must_use]
pub fn mint_oauth_state(
    secret: &SecretSlice<u8>,
    user_id: Uuid,
    provider: &str,
    now: OffsetDateTime,
) -> String {
    let expires = (now + OAUTH_STATE_TTL).unix_timestamp().to_be_bytes();
    let mac = tag(secret, user_id, provider, &expires)
        .finalize()
        .into_bytes();
    let mut raw = [0u8; EXPIRY_LEN + MAC_LEN];
    raw[..EXPIRY_LEN].copy_from_slice(&expires);
    raw[EXPIRY_LEN..].copy_from_slice(&mac);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// Check that `state` was minted by [`mint_oauth_state`] for this user and provider and has not
/// expired.
///
/// # Errors
/// [`AuthError::InvalidToken`] for a malformed, forged, foreign or expired value.
pub fn verify_oauth_state(
    secret: &SecretSlice<u8>,
    state: &str,
    user_id: Uuid,
    provider: &str,
    now: OffsetDateTime,
) -> Result<(), AuthError> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(state)
        .map_err(|_| AuthError::InvalidToken)?;
    if raw.len() != EXPIRY_LEN + MAC_LEN {
        return Err(AuthError::InvalidToken);
    }
    let (expires, mac) = raw.split_at(EXPIRY_LEN);
    // Constant-time; checked before the expiry so a forged value learns nothing from timing.
    tag(secret, user_id, provider, expires)
        .verify_slice(mac)
        .map_err(|_| AuthError::InvalidToken)?;
    let expires = i64::from_be_bytes(expires.try_into().map_err(|_| AuthError::InvalidToken)?);
    if now.unix_timestamp() >= expires {
        return Err(AuthError::InvalidToken);
    }
    Ok(())
}

fn tag(secret: &SecretSlice<u8>, user_id: Uuid, provider: &str, expires: &[u8]) -> HmacSha256 {
    // HMAC takes a key of any length, so neither error arm is reachable.
    let mut derive = HmacSha256::new_from_slice(secret.expose_secret())
        .unwrap_or_else(|_| unreachable!("HMAC accepts a key of any length"));
    derive.update(KEY_LABEL);
    let key = derive.finalize().into_bytes();
    let mut mac = HmacSha256::new_from_slice(&key)
        .unwrap_or_else(|_| unreachable!("HMAC accepts a key of any length"));
    mac.update(user_id.as_bytes());
    // Length-prefixed so no (provider, expiry) pair can be re-split into another.
    mac.update(
        &u64::try_from(provider.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    mac.update(provider.as_bytes());
    mac.update(expires);
    mac
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> SecretSlice<u8> {
        SecretSlice::from(b"0123456789abcdef0123456789abcdef".to_vec())
    }

    const NOW: OffsetDateTime = time::macros::datetime!(2026-10-26 12:00 UTC);

    #[test]
    fn a_state_verifies_for_the_user_and_provider_it_was_minted_for() {
        let user = Uuid::now_v7();
        let state = mint_oauth_state(&secret(), user, "anilist", NOW);
        verify_oauth_state(&secret(), &state, user, "anilist", NOW).expect("verifies");
    }

    /// The link-CSRF this exists for: an attacker's own `state` must not complete a link on the
    /// victim's session.
    #[test]
    fn a_state_minted_for_another_account_is_refused() {
        let attacker = Uuid::now_v7();
        let victim = Uuid::now_v7();
        let state = mint_oauth_state(&secret(), attacker, "anilist", NOW);
        assert!(verify_oauth_state(&secret(), &state, victim, "anilist", NOW).is_err());
    }

    #[test]
    fn a_state_is_bound_to_its_provider_expiry_and_key() {
        let user = Uuid::now_v7();
        let state = mint_oauth_state(&secret(), user, "anilist", NOW);
        assert!(verify_oauth_state(&secret(), &state, user, "mal", NOW).is_err());
        assert!(
            verify_oauth_state(&secret(), &state, user, "anilist", NOW + OAUTH_STATE_TTL).is_err()
        );
        let other = SecretSlice::from(b"another key entirely, 32 bytes!!".to_vec());
        assert!(verify_oauth_state(&other, &state, user, "anilist", NOW).is_err());
    }

    #[test]
    fn a_malformed_or_tampered_state_is_refused() {
        let user = Uuid::now_v7();
        assert!(verify_oauth_state(&secret(), "", user, "anilist", NOW).is_err());
        assert!(verify_oauth_state(&secret(), "not base64!", user, "anilist", NOW).is_err());
        let mut raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(mint_oauth_state(&secret(), user, "anilist", NOW))
            .expect("decodes");
        // Push the expiry out by a day: the MAC no longer matches.
        raw[7] ^= 0x01;
        let forged = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        assert!(verify_oauth_state(&secret(), &forged, user, "anilist", NOW).is_err());
    }
}
