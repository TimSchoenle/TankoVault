//! Authentication for service-to-service calls on the internal network.

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::error::ConfigError;

/// Authentication for service-to-service calls on the internal network.
///
/// Privileged internal routes (sync state, scan triggers, arbitrary-URL fetch) are reachable
/// by service name from anywhere on the network, so calls carry a shared secret in
/// `X-Internal-Token`, required in the production profile. See
/// [`InternalAuthConfig::resolve`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InternalAuthConfig {
    /// Shared secret presented by internal callers. Generate with `openssl rand -hex 32`.
    #[serde(default)]
    pub token: Option<SecretString>,
}

/// The shortest token accepted. 32 bytes of hex is the documented recipe; anything
/// materially shorter is guessable at internal-network request rates.
pub const MIN_INTERNAL_TOKEN_LEN: usize = 32;

impl InternalAuthConfig {
    /// The configured token, or an error the service must refuse to start on.
    ///
    /// Missing is allowed outside production; a present token is always length-checked.
    ///
    /// # Errors
    /// When the token is absent in the production profile, or shorter than
    /// [`MIN_INTERNAL_TOKEN_LEN`].
    pub fn resolve(&self, production: bool) -> Result<Option<SecretString>, ConfigError> {
        match self
            .token
            .as_ref()
            .map(|t| t.expose_secret().trim())
            .filter(|t| !t.is_empty())
        {
            None if production => Err(ConfigError::Invalid(
                "internal.token is required when TANKOVAULT_PROFILE=production; generate one \
                 with `openssl rand -hex 32` and set TANKOVAULT_INTERNAL__TOKEN on every \
                 service that talks to another"
                    .to_owned(),
            )),
            None => Ok(None),
            // The *length* of a secret is not itself secret — it is what the operator must
            // change — so it stays in the error message. The value never does.
            Some(t) if t.len() < MIN_INTERNAL_TOKEN_LEN => Err(ConfigError::Invalid(format!(
                "internal.token must be at least {MIN_INTERNAL_TOKEN_LEN} characters, got {}",
                t.len()
            ))),
            Some(t) => Ok(Some(SecretString::from(t))),
        }
    }
}
