//! Edge hardening applied by the shared middleware stack.

use serde::Deserialize;

use crate::cors::CorsConfig;
use crate::loader::is_production;

/// Edge hardening applied by the shared middleware stack.
///
/// `struct_excessive_bools` is allowed deliberately. The lint exists to catch boolean
/// *parameters* that should have been an enum; these are independent operator toggles that
/// map one-to-one onto `TANKOVAULT_SECURITY__*` environment variables. Collapsing them into
/// an enum or a bitflag would make the config surface harder to write, not easier.
#[derive(Debug, Clone, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
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
    /// Emit `Strict-Transport-Security`. Only meaningful when the edge is reached over
    /// TLS; sending it over plain HTTP is ignored by browsers but still misleading.
    #[serde(default)]
    pub hsts: bool,
    /// `max-age` for the HSTS header, seconds (default: two years, the preload minimum).
    #[serde(default = "SecurityConfig::default_hsts_max_age_secs")]
    pub hsts_max_age_secs: u64,
    /// Send the baseline set of hardening headers (`X-Content-Type-Options`,
    /// `X-Frame-Options`, `Referrer-Policy`, `Cross-Origin-Resource-Policy`).
    #[serde(default = "crate::default_true")]
    pub security_headers: bool,
    /// Accept and echo an inbound `X-Request-Id` instead of always minting a fresh one.
    /// Requires a trusted proxy for the same reason as
    /// [`crate::RateLimitConfig::trust_forwarded_for`] — a client-supplied id can otherwise
    /// be used to collide or poison log correlation.
    #[serde(default)]
    pub trust_request_id: bool,
    /// Serve the browsable API documentation (`/scalar`) and the `OpenAPI` document.
    ///
    /// **Defaults to off in the production profile and on everywhere else**, which is why
    /// this default is a function that reads the environment rather than a literal: the
    /// useful behaviour differs between the two, and requiring an operator to remember a
    /// third variable to switch it off is how it stays on.
    ///
    /// Unauthenticated, it hands an attacker the complete admin surface — every
    /// `/v1/admin/*` path, the permission vocabulary and exact request bodies — without a
    /// single failed probe. That is reconnaissance rather than compromise, but it removes
    /// the discovery cost of every other weakness.
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
