//! Service-to-service authentication for the internal tier.
//!
//! `sync`, `control-plane`, `render` and `challenge-solver` require `X-Internal-Token`,
//! compared in constant time; a mismatch is a bare `401` with no body. Health and readiness
//! probes are mounted outside this stack, so an orchestrator never needs the secret.

use axum::extract::{Request, State};
use axum::http::{HeaderName, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// The header internal callers present. Named for the tier, not the caller, because every
/// service in the tier shares one secret.
pub const INTERNAL_TOKEN_HEADER: HeaderName = HeaderName::from_static("x-internal-token");

/// The configured secret, shared by the middleware and the outbound clients that present it.
///
/// `Debug` is redacted via [`secrecy`], so a stray `tracing::debug!(?state)` cannot leak the
/// key to the whole internal tier. `Arc<SecretString>` because this value is cloned into an
/// `AppState` per request; the `Arc` keeps exactly one heap copy, which is the copy zeroized.
#[derive(Clone, Debug)]
pub struct InternalToken(Arc<SecretString>);

impl InternalToken {
    /// Wrap a resolved token.
    #[must_use]
    pub fn new(token: impl Into<SecretString>) -> Self {
        Self(Arc::new(token.into()))
    }

    /// Constant-time equality against a presented value.
    #[must_use]
    pub fn matches(&self, presented: &str) -> bool {
        // The plain `len()` check is safe pre-filtering, not a leak: lengths are already
        // public, and `ct_eq` would reject a length mismatch anyway.
        let expected = self.0.expose_secret().as_bytes();
        let got = presented.as_bytes();
        expected.len() == got.len() && bool::from(expected.ct_eq(got))
    }
}

/// Reading the token is [`ExposeSecret`], not a bespoke method, so every secret read in the
/// workspace is the same greppable call.
impl ExposeSecret<str> for InternalToken {
    fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

/// Resolve the configured token for a service in the internal tier.
///
/// Absent outside the production profile is allowed (local dev, tests) but logged at `warn`.
/// In production, or for any token that is present but too short,
/// [`tankovault_config::InternalAuthConfig::resolve`] refuses and the service does not boot.
///
/// A **published placeholder is refused in every profile**, not only production: a check a
/// deployment can skip by forgetting one environment variable is not a check.
///
/// # Errors
/// [`tankovault_config::ConfigError::Invalid`] when the token is missing in production, fails
/// the length floor, or is one of [`KNOWN_PLACEHOLDERS`].
pub fn resolve(
    cfg: &tankovault_config::InternalAuthConfig,
) -> Result<Option<InternalToken>, tankovault_config::ConfigError> {
    let resolved = cfg.resolve(tankovault_config::is_production())?;

    if let Some(token) = &resolved {
        if let Some(name) = known_placeholder(token.expose_secret()) {
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
/// Counterpart of `services/api/src/main.rs::KNOWN_PLACEHOLDERS`. `xtask repo-lint` derives
/// this list's required contents from `deploy/docker-compose.yml` defaults and fails if a
/// published secret is missing from it.
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

    // Terse and identical for "absent" and "wrong": the caller is a service with a static
    // config, not a human debugging. Only the path is logged, never the query string —
    // `/v1/me/stream` carries a token there.
    tracing::warn!(
        path = %req.uri().path(),
        "rejected an internal request with a missing or invalid token"
    );
    StatusCode::UNAUTHORIZED.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `deploy/docker-compose.yml` published a default token that production only refused
    /// when *missing*, not when equal to this one — refused in every profile now.
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

    /// Asserted as "the secret does not appear" rather than an exact rendering, so a
    /// cosmetic change in `secrecy`'s redaction format doesn't fail a test that shouldn't care.
    #[test]
    fn debug_is_redacted() {
        let token = InternalToken::new("do-not-print-me-do-not-print-me-1");
        let rendered = format!("{token:?}");
        assert!(!rendered.contains("do-not-print"), "{rendered}");
        assert!(rendered.contains("REDACTED"), "{rendered}");
    }
}
