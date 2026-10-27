//! Inbound request rate limiting for a service's HTTP edge.

use serde::Deserialize;
use terrace_config::schema::Describe;

/// Where rate-limit counters live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Describe)]
#[serde(rename_all = "lowercase")]
pub enum RateLimitBackend {
    /// Process-local counters. Behind a load balancer the effective limit multiplies by the
    /// replica count.
    #[default]
    Memory,
    /// Shared counters in Redis, so the limit holds across every replica. Fails **open** while
    /// Redis is unreachable, so a counter-store outage does not take the edge down.
    Redis,
}

/// A token-bucket policy: a sustained refill rate ([`Self::per_minute`]) and a bucket depth
/// ([`Self::burst`]). A burst below the sustained rate is normal.
#[derive(Debug, Clone, Copy, Deserialize, Describe)]
pub struct RateLimitPolicy {
    /// Sustained requests allowed per minute per client key, which is the bucket's refill rate.
    pub per_minute: u32,
    /// The most requests a client may spend back-to-back.
    pub burst: u32,
}

impl RateLimitPolicy {
    /// Returns a policy refilling `per_minute` with a bucket of `burst`.
    #[must_use]
    pub const fn new(per_minute: u32, burst: u32) -> Self {
        Self { per_minute, burst }
    }

    /// Returns the bucket capacity, at least 1, so a `burst` of `0` does not reject every request.
    #[must_use]
    pub const fn capacity(&self) -> u32 {
        if self.burst == 0 { 1 } else { self.burst }
    }
}

/// Inbound request rate limiting for a service's HTTP edge.
///
/// The outbound pacing of requests to providers is `tankovault_domain::pacing`.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct RateLimitConfig {
    /// Enforce limits. When `false` the layer is not mounted and costs nothing per request.
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Where the counters live.
    #[config(values)]
    #[serde(default)]
    pub backend: RateLimitBackend,
    /// Applies to any route without a stricter class below.
    #[config(nested)]
    #[serde(default = "RateLimitConfig::default_global")]
    pub global: RateLimitPolicy,
    /// Credential-handling routes: login, register, reset and refresh. It is the control on
    /// online guessing, so it sits far below [`Self::global`].
    #[config(nested)]
    #[serde(default = "RateLimitConfig::default_auth")]
    pub auth: RateLimitPolicy,
    /// Routes that are cheap to call and expensive to serve: data export, scan triggers, sync
    /// push and pull.
    #[config(nested)]
    #[serde(default = "RateLimitConfig::default_expensive")]
    pub expensive: RateLimitPolicy,
    /// Take the client key from `X-Forwarded-For` or `X-Real-IP`. Enable it **only behind a
    /// reverse proxy that overwrites both**; otherwise any client forges a fresh key and bypasses
    /// the limiter.
    #[serde(default)]
    pub trust_forwarded_for: bool,
}

impl RateLimitConfig {
    fn default_global() -> RateLimitPolicy {
        RateLimitPolicy::new(300, 60)
    }
    fn default_auth() -> RateLimitPolicy {
        RateLimitPolicy::new(10, 5)
    }
    fn default_expensive() -> RateLimitPolicy {
        // Loose enough for two back-to-back scans, or an export alongside a sync.
        RateLimitPolicy::new(30, 10)
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: RateLimitBackend::default(),
            global: Self::default_global(),
            auth: Self::default_auth(),
            expensive: Self::default_expensive(),
            trust_forwarded_for: false,
        }
    }
}
