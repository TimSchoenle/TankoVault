//! Edge hardening applied by the shared middleware stack.

use serde::Deserialize;
use terrace_config::schema::Describe;

use crate::cors::CorsConfig;
use crate::loader::is_production;

/// Edge hardening applied by the shared middleware stack.
#[derive(Debug, Clone, Deserialize, Describe)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent operator toggles, one per TANKOVAULT_SECURITY__* variable"
)]
pub struct SecurityConfig {
    /// Cross-origin policy. See [`CorsConfig`].
    #[config(nested)]
    #[serde(default)]
    pub cors: CorsConfig,
    /// Reject request bodies larger than this, in bytes, before they are buffered.
    #[serde(default = "SecurityConfig::default_max_body_bytes")]
    pub max_body_bytes: usize,
    /// Abort a request that has not produced a response within this many seconds.
    #[serde(default = "SecurityConfig::default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Emit `Strict-Transport-Security`. Browsers ignore it over plain HTTP.
    #[serde(default)]
    pub hsts: bool,
    /// `max-age` for the HSTS header, in seconds. The default is two years, the preload minimum.
    #[serde(default = "SecurityConfig::default_hsts_max_age_secs")]
    pub hsts_max_age_secs: u64,
    /// Send the baseline set of hardening headers (`X-Content-Type-Options`,
    /// `X-Frame-Options`, `Referrer-Policy`, `Cross-Origin-Resource-Policy`).
    #[serde(default = "crate::default_true")]
    pub security_headers: bool,
    /// Accept an inbound `X-Request-Id` instead of minting one. Enable it only behind a trusted
    /// proxy, since a client-supplied id can poison log correlation.
    #[serde(default)]
    pub trust_request_id: bool,
    /// Serve the browsable API docs (`/scalar`) and the `OpenAPI` document.
    ///
    /// Off by default in production and on elsewhere. Left on in production, it gives an
    /// attacker every `/v1/admin/*` path, the permission vocabulary and the exact request bodies
    /// without a failed probe.
    #[serde(default = "SecurityConfig::default_expose_api_docs")]
    pub expose_api_docs: bool,
}

impl SecurityConfig {
    fn default_max_body_bytes() -> usize {
        1024 * 1024 // 1 MiB: the largest legitimate body here is a provider config blob.
    }
    fn default_request_timeout_secs() -> u64 {
        30
    }
    fn default_hsts_max_age_secs() -> u64 {
        63_072_000
    }
    fn default_expose_api_docs() -> bool {
        !is_production()
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            cors: CorsConfig::default(),
            max_body_bytes: Self::default_max_body_bytes(),
            request_timeout_secs: Self::default_request_timeout_secs(),
            hsts: false,
            hsts_max_age_secs: Self::default_hsts_max_age_secs(),
            security_headers: true,
            trust_request_id: false,
            expose_api_docs: Self::default_expose_api_docs(),
        }
    }
}
