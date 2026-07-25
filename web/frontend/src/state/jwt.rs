//! Reading claims out of an access token.
//!
//! Every decode here is **unverified** — the payload segment is base64-decoded and parsed,
//! and the signature is ignored. That is safe because nothing security-relevant hangs off
//! it: the server re-verifies the token on every request and RBAC-gates every admin route.
//! These claims only decide what the UI bothers to *render* (which rail links to show, which
//! name to greet, when to schedule the next silent refresh). A forged token buys a user a
//! Console link that answers 403 the moment they click it.

use base64::Engine;
use serde_json::Value;

/// Decode a JWT's payload segment. `None` for anything that isn't a well-formed,
/// base64url-encoded JSON object in the second dot-separated position.
fn payload(token: &str) -> Option<Value> {
    let encoded = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The `role` claim, or `None` when absent or undecodable.
pub(crate) fn role(token: &str) -> Option<String> {
    payload(token)?
        .get("role")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A human-facing display name — the first of `username`, `name`, `sub` that carries a
/// non-blank string.
pub(crate) fn username(token: &str) -> Option<String> {
    let claims = payload(token)?;
    ["username", "name", "sub"]
        .iter()
        .find_map(|key| {
            // The blank check belongs *inside* the search, not after it. Applied afterwards
            // it makes a present-but-empty `username` claim swallow the whole lookup, so the
            // reader is greeted as nothing at all rather than falling through to `sub`.
            claims
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
        })
        .map(str::to_owned)
}

/// The `exp` claim as unix seconds.
pub(crate) fn expires_at(token: &str) -> Option<i64> {
    payload(token)?.get("exp").and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    /// Build an unsigned token whose payload is `claims`. The signature segment is junk on
    /// purpose — these decoders must not care.
    fn token(claims: &str) -> String {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims);
        format!("header.{payload}.signature")
    }

    #[test]
    fn reads_claims_from_a_well_formed_token() {
        let t = token(r#"{"role":"admin","username":"kaz","exp":1700000000}"#);
        assert_eq!(role(&t).as_deref(), Some("admin"));
        assert_eq!(username(&t).as_deref(), Some("kaz"));
        assert_eq!(expires_at(&t), Some(1_700_000_000));
    }

    #[test]
    fn prefers_username_then_name_then_sub() {
        assert_eq!(
            username(&token(r#"{"name":"Kaz","sub":"uuid"}"#)).as_deref(),
            Some("Kaz")
        );
        assert_eq!(
            username(&token(r#"{"sub":"uuid"}"#)).as_deref(),
            Some("uuid")
        );
    }

    #[test]
    fn ignores_blank_display_names() {
        assert_eq!(
            username(&token(r#"{"username":"   ","sub":"x"}"#)).as_deref(),
            Some("x")
        );
    }

    #[test]
    fn malformed_tokens_decode_to_nothing_rather_than_panicking() {
        for bad in ["", "not-a-jwt", "a.!!!.c", "a.eyJib2d1cw.c"] {
            assert!(role(bad).is_none());
            assert!(username(bad).is_none());
            assert!(expires_at(bad).is_none());
        }
    }
}
