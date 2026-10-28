//! Cross-origin resource sharing.

use serde::Deserialize;
use terrace_config::schema::Describe;

/// Cross-origin resource sharing.
///
/// The default is an **empty allowlist**, which adds no CORS layer: browsers withhold every
/// cross-origin response from the page that asked, but a simple request still reaches its
/// handler, so this is not a request filter. A split-origin deployment names its origins.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct CorsConfig {
    /// Exact origins allowed, such as `https://app.example.com`. Empty disables CORS.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Allow credentialed cross-origin requests, carrying cookies or `Authorization`. Cannot be
    /// combined with a wildcard origin.
    #[serde(default)]
    pub allow_credentials: bool,
    /// `Access-Control-Max-Age`, in seconds.
    #[serde(default = "CorsConfig::default_max_age_secs")]
    pub max_age_secs: u64,
}

impl CorsConfig {
    fn default_max_age_secs() -> u64 {
        3600
    }

    /// Returns whether any cross-origin request is permitted.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.allowed_origins.is_empty()
    }
}
