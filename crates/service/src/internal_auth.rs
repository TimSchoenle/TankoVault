//! Service-to-service authentication for the internal tier.
//!
//! `sync`, `control-plane`, `render` and `challenge-solver` are not public services, but
//! "not public" was previously enforced only by network placement — and the shipped compose
//! file published every one of them on the host. Anyone who could reach the port could read
//! any user's sync state, rebind their linked provider account, trigger scans, or use the
//! renderer as an arbitrary-URL fetcher.
//!
//! This layer makes the trust explicit: a caller must present `X-Internal-Token` matching the
//! configured secret. The comparison is constant-time, so a wrong token leaks nothing about
//! how wrong it was, and the rejection is a bare `401` with no body — an unauthenticated
//! caller learns only that the door is shut.
//!
//! Health and readiness probes are mounted *outside* the stack this layer belongs to (see
//! [`crate::ops_router`]), so an orchestrator never needs the secret.

use axum::extract::{Request, State};
use axum::http::{HeaderName, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// The header internal callers present. Named for the tier, not the caller, because every
/// service in the tier shares one secret.
pub const INTERNAL_TOKEN_HEADER: HeaderName = HeaderName::from_static("x-internal-token");

/// The configured secret, shared by the middleware and the outbound clients that present it.
///
/// `Debug` is redacted: this value ends up inside service `AppState`s, and one
/// `tracing::debug!(?state)` would otherwise put the key to the whole internal tier in the
/// log stream.
#[derive(Clone)]
pub struct InternalToken(Arc<str>);

impl InternalToken {
    /// Wrap a resolved token.
    #[must_use]
    pub fn new(token: impl Into<Arc<str>>) -> Self {
        Self(token.into())
    }

    /// The raw value, for attaching to an outbound request.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Constant-time equality against a presented value.
    #[must_use]
    pub fn matches(&self, presented: &str) -> bool {
        // Lengths are compared in constant time too: `ct_eq` on unequal-length slices is
        // false, but `as_bytes()` lengths are public, so an early length check would leak
        // the secret's length. `subtle` handles the mismatch without branching on content.
        let expected = self.0.as_bytes();
        let got = presented.as_bytes();
        expected.len() == got.len() && bool::from(expected.ct_eq(got))
    }
}

impl std::fmt::Debug for InternalToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InternalToken(<redacted>)")
    }
}

/// Resolve the configured token for a service in the internal tier.
///
/// Absent outside the production profile is allowed — local `docker compose up` and the test
/// harness stay frictionless — but it is logged at `warn`, because a tier running open is a
/// fact an operator should see in the first ten lines of the log rather than discover later.
/// In the production profile, and for any token that is present but too short,
/// [`tankovault_config::InternalAuthConfig::resolve`] refuses and the service does not boot.
///
/// A **published placeholder is refused in every profile**, not only production. That is the
/// reasoning `services/api/src/main.rs::validate_auth_secrets` already records for the JWT
/// secret: a check a deployment can skip by forgetting one environment variable is not a
/// check. The placeholder this rejects was the compose file's silent default, so it is exactly
/// what an operator who never created `deploy/local.env` was running with — on the credential
/// that authorizes "read or rewrite any user's sync state" between tiers. The comment above
/// that default claimed production already refused it; production refused a *missing* token,
/// not a well-known one, and nothing connected the two files until `xtask repo-lint`.
///
/// # Errors
/// [`tankovault_config::ConfigError::Invalid`] when the token is missing in production, fails
/// the length floor, or is one of [`KNOWN_PLACEHOLDERS`].
pub fn resolve(
    cfg: &tankovault_config::InternalAuthConfig,
) -> Result<Option<InternalToken>, tankovault_config::ConfigError> {
    let resolved = cfg.resolve(tankovault_config::is_production())?;

    if let Some(token) = &resolved {
        if let Some(name) = known_placeholder(token) {
            return Err(tankovault_config::ConfigError::Invalid(format!(
                "refusing to start: internal.token is the well-known {name}, which is published \
                 in this repository. Anything that has read deploy/docker-compose.yml can call \
                 this service's privileged routes with it. Set TANKOVAULT_INTERNAL__TOKEN."
            )));
        }
    } else {
        tracing::warn!(
            "no internal.token configured: this service's privileged routes are reachable by \
             anything that can open a socket to it. Set TANKOVAULT_INTERNAL__TOKEN."
        );
    }

    Ok(resolved.map(InternalToken::new))
}

/// Placeholder tokens published in this repository, refused wherever they appear.
///
/// The counterpart of `services/api/src/main.rs::KNOWN_PLACEHOLDERS`, which does the same job
/// for the auth secrets. Kept next to the resolver every internal service calls, so all five
/// tiers get the refusal from one place rather than each remembering to check.
///
/// `xtask repo-lint` derives the required contents of this list from the defaults in
/// `deploy/docker-compose.yml` and fails if a published secret is missing from it — the entry
/// below was, for exactly as long as nothing connected the two files.
const KNOWN_PLACEHOLDERS: [(&str, &str); 1] = [(
    "dev-internal-token-not-for-production-use",
    "development internal-tier token",
)];

/// The name of the published placeholder `value` is, if it is one.
fn known_placeholder(value: &str) -> Option<&'static str> {
    let trimmed = value.trim();
    KNOWN_PLACEHOLDERS
        .iter()
        .find(|(placeholder, _)| *placeholder == trimmed)
        .map(|(_, name)| *name)
}

/// Reject any request that does not present the internal token.
///
/// Mount with `axum::middleware::from_fn_with_state(token, enforce)` inside the service's
/// [`crate::http::HttpStack`], so rejections still carry the security headers and request id.
pub async fn enforce(State(token): State<InternalToken>, req: Request, next: Next) -> Response {
    let authorized = req
        .headers()
        .get(INTERNAL_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|presented| token.matches(presented));

    if authorized {
        return next.run(req).await;
    }

    // Deliberately terse and identical for "absent" and "wrong": the caller is not a human
    // debugging their integration, it is a service with a static config. The path is logged
    // but never the query string — `/v1/me/stream` carries a token there, and this must not
    // be the place that starts recording them.
    tracing::warn!(
        path = %req.uri().path(),
        "rejected an internal request with a missing or invalid token"
    );
    StatusCode::UNAUTHORIZED.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The internal token authorizes "read or rewrite any user's sync state" between tiers.
    /// `deploy/docker-compose.yml` published a default for it, and the comment above that
    /// default claimed production refused it — production refused a *missing* token, so a
    /// deployment inheriting the file booted with a credential printed in this repository.
    ///
    /// Refused in **every** profile, not only production, for the reason
    /// `services/api/src/main.rs::validate_auth_secrets` records for the JWT secret: a check a
    /// deployment can skip by forgetting one environment variable is not a check.
    #[test]
    fn the_published_placeholder_is_refused_in_every_profile() {
        for (placeholder, _) in KNOWN_PLACEHOLDERS {
            assert!(
                known_placeholder(placeholder).is_some(),
                "{placeholder} must be recognised"
            );
            // Surrounding whitespace is how a placeholder survives a copy-paste through YAML.
            assert!(known_placeholder(&format!("  {placeholder}  ")).is_some());
        }
        assert!(known_placeholder("a-real-token-from-openssl-rand-hex-32").is_none());
    }

    #[test]
    fn matches_only_the_exact_token() {
        let token = InternalToken::new("s3cret-value-of-sufficient-length");
        assert!(token.matches("s3cret-value-of-sufficient-length"));
        assert!(!token.matches("s3cret-value-of-sufficient-lengtH"));
        assert!(!token.matches("s3cret-value-of-sufficient-lengt"));
        assert!(!token.matches("s3cret-value-of-sufficient-length "));
        assert!(!token.matches(""));
    }

    /// The whole point of the type is that it cannot be printed by accident.
    #[test]
    fn debug_is_redacted() {
        let token = InternalToken::new("do-not-print-me-do-not-print-me-1");
        assert_eq!(format!("{token:?}"), "InternalToken(<redacted>)");
        assert!(!format!("{token:?}").contains("do-not-print"));
    }
}
