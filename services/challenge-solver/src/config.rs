//! What the `challenge-solver` binary reads from its configuration.
//!
//! Public, and in a library rather than beside `main`, because it is the root
//! `config-contract` describes for this image: the contract has to be generated from the very
//! type the binary deserialises, or it is a claim about something else.

use serde::Deserialize;
use tankovault_config::TelemetryConfig;
use terrace_config::schema::Describe;

/// Top-level challenge-solver config.
#[derive(Debug, Deserialize, Describe)]
pub struct Config {
    /// Listen address for `/v1/solve` and the probes. Internal-tier only.
    #[serde(default = "default_bind")]
    pub bind_addr: String,
    /// Log filter, log format and Sentry reporting.
    #[config(nested)]
    pub telemetry: TelemetryConfig,
    /// Which back-end solves an interstitial, and how long it is given.
    #[config(nested)]
    pub solver: SolverBackendConfig,
    /// Edge hardening: body cap, timeout, security headers. CORS stays off — nothing
    /// browser-originated calls this service.
    #[serde(default)]
    #[config(nested)]
    pub security: tankovault_config::SecurityConfig,
    /// Inbound rate limiting. On by default: a runaway worker retry loop can exhaust the
    /// solver pool as easily as a hostile client.
    #[serde(default)]
    #[config(nested)]
    pub rate_limit: tankovault_config::RateLimitConfig,
    /// Prometheus metrics; disabling installs no recorder.
    #[serde(default)]
    #[config(nested)]
    pub metrics: tankovault_config::MetricsConfig,
    /// Shared secret every caller must present. `/v1/solve` fetches a caller-supplied URL
    /// and returns the body — an SSRF primitive for anyone who can reach the port.
    #[serde(default)]
    #[config(nested)]
    pub internal: tankovault_config::InternalAuthConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8090".to_owned()
}

/// Which back-end solves an interstitial, and how long it is given.
#[derive(Debug, Deserialize, Describe)]
pub struct SolverBackendConfig {
    /// Back-end selector; only `trawl` is wired today.
    #[serde(default = "default_backend")]
    pub backend: String,
    /// TRAWL base endpoint, e.g. `http://trawl:8191`.
    pub trawl_endpoint: String,
    /// Ceiling on one solve attempt, in milliseconds. A challenge still standing at the end of
    /// it is reported unsolved, which the caller treats as retryable.
    #[serde(default = "default_timeout")]
    pub max_timeout_ms: u64,
    /// How long *this* deployment caches a solved session for. Independent of TRAWL's own
    /// `SESSION_TTL_SECONDS`, which governs the cookie jar it replays internally.
    #[serde(default = "default_ttl")]
    pub session_ttl_secs: u64,
}

fn default_backend() -> String {
    "trawl".to_owned()
}

fn default_timeout() -> u64 {
    60_000
}

fn default_ttl() -> u64 {
    900
}
