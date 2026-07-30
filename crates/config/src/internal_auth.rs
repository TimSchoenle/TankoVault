//! Authentication for service-to-service calls on the internal network.

use serde::Deserialize;

use crate::error::ConfigError;

/// Authentication for service-to-service calls on the internal network.
///
/// `sync`, `control-plane`, `render` and `challenge-solver` expose privileged operations —
/// reading any user's sync state, triggering scans, fetching an arbitrary URL — and are
/// reachable by service name from anywhere on the compose/cluster network. Network
/// placement alone is one misconfiguration away from exposing them, so the calls carry a
/// shared secret in `X-Internal-Token`.
///
/// The token is deliberately **required** in the production profile: a service that starts
/// happily without one silently downgrades to the unauthenticated behaviour this exists to
/// remove. See [`InternalAuthConfig::resolve`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InternalAuthConfig {
    /// Shared secret presented by internal callers. Generate with `openssl rand -hex 32`.
    #[serde(default)]
    pub token: Option<String>,
}

/// The shortest token accepted. 32 bytes of hex is the documented recipe; anything
/// materially shorter is guessable at internal-network request rates.
pub const MIN_INTERNAL_TOKEN_LEN: usize = 32;

impl InternalAuthConfig {
    /// The configured token, or an error explaining why the service must not start.
    ///
    /// Outside the production profile a missing token is allowed and reported as `None`, so
    /// `docker compose up` and the test harness stay frictionless. A token that is *present*
    /// is always length-checked, in every profile — a weak-secret check a deployment can skip
    /// by forgetting one variable is not a check.
    ///
    /// # Errors
    /// When the token is absent in the production profile, or shorter than
    /// [`MIN_INTERNAL_TOKEN_LEN`].
    pub fn resolve(&self, production: bool) -> Result<Option<String>, ConfigError> {
        match self
            .token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            None if production => Err(ConfigError::Invalid(
                "internal.token is required when TANKOVAULT_PROFILE=production; generate one \
                 with `openssl rand -hex 32` and set TANKOVAULT_INTERNAL__TOKEN on every \
                 service that talks to another"
                    .to_owned(),
            )),
            None => Ok(None),
            Some(t) if t.len() < MIN_INTERNAL_TOKEN_LEN => Err(ConfigError::Invalid(format!(
                "internal.token must be at least {MIN_INTERNAL_TOKEN_LEN} characters, got {}",
                t.len()
            ))),
            Some(t) => Ok(Some(t.to_owned())),
        }
    }
}
