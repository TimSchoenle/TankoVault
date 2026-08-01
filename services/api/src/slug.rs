//! The only type a caller-supplied provider slug may reach an upstream URL through.
//!
//! # The hole this closes (CodeQL `rust/request-forgery`)
//!
//! Every sync proxy handler used to take `Path(provider): Path<String>` and interpolate it
//! straight into the upstream path — `format!("/v1/sync/{provider}/push")` — which
//! [`crate::upstream::Upstream::url`] then appends to the peer's base URL. `axum`'s `Path`
//! extractor percent-decodes before the handler sees the value, so `provider` was an
//! *arbitrary string chosen by the client*, and three characters in it changed which endpoint
//! the API called:
//!
//! - **`/` and `..`.** `%2e%2e%2f%2e%2e%2fadmin` decodes to `../../admin`, and the URL parser
//!   resolves the dot segments before the request goes out. The client picks the path on the
//!   internal service — and [`crate::upstream::Upstream`] attaches `X-Internal-Token` to it,
//!   so the reachable surface is every authenticated internal endpoint, not just `/v1/sync/*`.
//! - **`?` and `#`.** Either one truncates everything the handler appended afterwards. On
//!   `/v1/sync/{provider}/settings/{user_id}` that severs the *user id* from the path, so the
//!   peer sees a request whose subject is whatever the query string now says.
//!
//! The host could not be moved — the base URL is configuration — so this was never open SSRF.
//! It was still the client choosing an internal path and having the API present its own
//! credentials on it, which is the same class of bug and is not something a call-site `if`
//! should be relied on to prevent: [`crate::upstream`]'s own module doc makes the point that a
//! per-call-site convention "would be forgotten exactly once, which is all it takes". Hence a
//! type. A [`ProviderSlug`] cannot be constructed from a string that is not a bare token, so a
//! handler that takes one cannot be the site of this bug, and a new proxy handler inherits the
//! guarantee by writing down the parameter type.
//!
//! The alphabet is [`tankovault_contracts::is_valid_provider_slug`] rather than a second
//! private regex: it is already the rule for what a provider may be *called* (the slug becomes
//! a NATS subject token and a durable consumer name, checked on `POST /v1/admin/providers`), so
//! a slug that fails here could never have named a real provider anyway. Rejecting at the edge
//! turns a `502` from a confused peer into a `400` that says what was wrong.

use serde::{Deserialize, Deserializer, de};
use std::fmt;

/// A provider slug that has been checked against
/// [`tankovault_contracts::is_valid_provider_slug`] and is therefore safe to interpolate into
/// an upstream URL path or query string.
///
/// Deliberately holds no public constructor from `String`: [`Deserialize`] is the only way in,
/// which is what makes "validated" a property of the type rather than of the caller. `Display`
/// is the way out, so existing `format!` call sites read exactly as they did before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSlug(String);

impl fmt::Display for ProviderSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProviderSlug {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if tankovault_contracts::is_valid_provider_slug(&raw) {
            Ok(Self(raw))
        } else {
            // The rejected value is *not* echoed. It is attacker-chosen, unbounded, and the
            // message ends up in a `400` body and in access logs; naming the legal alphabet
            // tells an honest client everything it needs and tells an attacker nothing.
            Err(de::Error::custom(
                "provider slug must be non-empty and contain only letters, digits, '-' or '_'",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProviderSlug;

    fn parse(raw: &str) -> Result<ProviderSlug, serde_json::Error> {
        serde_json::from_value(serde_json::Value::String(raw.to_owned()))
    }

    /// The characters that made this a security fix, each named individually so a future
    /// relaxation of the alphabet cannot quietly re-open one of them.
    ///
    /// `..` and `/` let the client resolve to a different path on the internal service, which
    /// the API calls with `X-Internal-Token` attached; `?` and `#` truncate the path the
    /// handler built, severing the user id from
    /// `/v1/sync/{provider}/settings/{user_id}`.
    #[test]
    fn a_slug_may_not_carry_a_character_that_changes_the_upstream_url() {
        for hostile in [
            "../../admin",
            "a/b",
            "..",
            "anilist?",
            "anilist#",
            "anilist/../..",
            "%2e%2e",
            "ani list",
            "",
        ] {
            assert!(
                parse(hostile).is_err(),
                "{hostile:?} must not become a ProviderSlug"
            );
        }
    }

    #[test]
    fn a_real_provider_slug_still_parses() {
        assert_eq!(parse("anilist").unwrap().to_string(), "anilist");
        assert_eq!(parse("manga-dex_2").unwrap().to_string(), "manga-dex_2");
    }

    /// The rejection must not quote what was sent: it lands in a `400` body and the access log.
    #[test]
    fn the_rejection_does_not_echo_the_rejected_value() {
        let err = parse("../../etc/passwd").expect_err("a traversal slug is rejected");
        assert!(
            !err.to_string().contains(".."),
            "the error must not echo the caller's value: {err}"
        );
    }
}
