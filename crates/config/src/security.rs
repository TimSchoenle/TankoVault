//! Edge hardening applied by the shared middleware stack.

use serde::Deserialize;

use crate::cors::CorsConfig;
use crate::loader::is_production;

/// Edge hardening applied by the shared middleware stack.
///
/// `struct_excessive_bools` allowed: these are independent operator toggles mapped 1:1 onto
/// `TANKOVAULT_SECURITY__*` env vars, not boolean parameters that should be an enum.
#[derive(Debug, Clone, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent operator toggles, one per TANKOVAULT_SECURITY__* variable"
)]
pub struct SecurityConfig {
    /// Cross-origin policy. See [`CorsConfig`].
    #[serde(default)]
    pub cors: CorsConfig,
    /// Reject request bodies larger than this, in bytes, before they are buffered.
    #[serde(default = "SecurityConfig::default_max_body_bytes")]
    pub max_body_bytes: usize,
    /// Abort a request that has not produced a response within this many seconds.
    #[serde(default = "SecurityConfig::default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Emit `Strict-Transport-Security`; meaningless without TLS (browsers ignore it over
    /// plain HTTP).
    #[serde(default)]
    pub hsts: bool,
    /// `max-age` for the HSTS header, seconds (default: two years, the preload minimum).
    #[serde(default = "SecurityConfig::default_hsts_max_age_secs")]
    pub hsts_max_age_secs: u64,
    /// Send the baseline set of hardening headers (`X-Content-Type-Options`,
    /// `X-Frame-Options`, `Referrer-Policy`, `Cross-Origin-Resource-Policy`).
    #[serde(default = "crate::default_true")]
    pub security_headers: bool,
    /// Accept an inbound `X-Request-Id` instead of minting one. Requires a trusted proxy —
    /// a client-supplied id could otherwise poison log correlation.
    #[serde(default)]
    pub trust_request_id: bool,
    /// Serve the browsable API docs (`/scalar`) and the `OpenAPI` document.
    ///
    /// Defaults off in production, on elsewhere (hence a function, not a literal): left on,
    /// it hands an attacker the whole admin surface — every `/v1/admin/*` path, the
    /// permission vocabulary, exact request bodies — with no failed probe.
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
