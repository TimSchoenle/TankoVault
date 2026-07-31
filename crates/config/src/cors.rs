//! Cross-origin resource sharing.

use serde::Deserialize;

/// Cross-origin resource sharing.
///
/// The default is an **empty allowlist**, which rejects every cross-origin request. The
/// reference deployment serves the frontend and the API from one origin (the `frontend`
/// server proxies `/v1/*` to the API), so no CORS hop exists; a split-origin deployment must
/// name its origins explicitly.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CorsConfig {
    /// Exact origins allowed, e.g. `https://app.example.com`. Empty disables CORS entirely.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Allow credentialed cross-origin requests (cookies, `Authorization`). Cannot be
    /// combined with a wildcard origin, which is one more reason the allowlist is explicit.
    #[serde(default)]
    pub allow_credentials: bool,
    /// `Access-Control-Max-Age`, seconds.
    #[serde(default = "CorsConfig::default_max_age_secs")]
    pub max_age_secs: u64,
}

impl CorsConfig {
    fn default_max_age_secs() -> u64 {
        3600
    }

    /// Whether any cross-origin request should be permitted.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.allowed_origins.is_empty()
    }
}
