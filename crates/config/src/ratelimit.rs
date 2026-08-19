//! Inbound request rate limiting for a service's HTTP edge.

use serde::Deserialize;
use terrace_config::schema::Describe;

/// Where rate-limit counters live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Describe)]
#[serde(rename_all = "lowercase")]
pub enum RateLimitBackend {
    /// Process-local counters; correct for one replica, but the effective limit multiplies
    /// by replica count behind a load balancer.
    #[default]
    Memory,
    /// Shared counters in Redis, so the limit holds across every replica. Fails **open** if
    /// Redis is unreachable — a counter-store outage must not take the edge down.
    Redis,
}

/// A token-bucket policy: sustained refill rate ([`Self::per_minute`]) plus bucket depth
/// ([`Self::burst`]). A burst below the sustained rate is normal, not a misconfiguration.
#[derive(Debug, Clone, Copy, Deserialize, Describe)]
pub struct RateLimitPolicy {
    /// Sustained requests allowed per minute per client key; the bucket's refill rate.
    pub per_minute: u32,
    /// Bucket depth: the most requests a client may spend back-to-back.
    pub burst: u32,
}

impl RateLimitPolicy {
    /// Construct a policy directly (used for defaults and in tests).
    #[must_use]
    pub const fn new(per_minute: u32, burst: u32) -> Self {
        Self { per_minute, burst }
    }

    /// Bucket capacity, clamped to at least 1 so a misconfigured `0` doesn't reject every
    /// request forever.
    #[must_use]
    pub const fn capacity(&self) -> u32 {
        if self.burst == 0 { 1 } else { self.burst }
    }
}

/// Inbound request rate limiting for a service's HTTP edge.
///
/// Distinct from the *outbound* crawl politeness in `tankovault_domain::pacing`, which paces
/// requests this system makes to third-party providers.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct RateLimitConfig {
    /// Enforce limits. When `false` the layer is not mounted at all (no per-request cost).
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Counter store. See [`RateLimitBackend`].
    #[config(values)]
    #[serde(default)]
    pub backend: RateLimitBackend,
    /// Applies to any route without a stricter class below.
    #[config(nested)]
    #[serde(default = "RateLimitConfig::default_global")]
    pub global: RateLimitPolicy,
    /// Credential-handling routes (login, register, reset, refresh); the online-guessing
    /// control, deliberately far below [`Self::global`].
    #[config(nested)]
    #[serde(default = "RateLimitConfig::default_auth")]
    pub auth: RateLimitPolicy,
    /// Routes that are cheap to call and expensive to serve (data export, scan triggers,
    /// sync push/pull).
    #[config(nested)]
    #[serde(default = "RateLimitConfig::default_expensive")]
    pub expensive: RateLimitPolicy,
    /// Trust `X-Forwarded-For`/`X-Real-IP` for the client key. **Only behind a reverse proxy
    /// that overwrites these** — otherwise any client can forge a fresh identity and bypass
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
        // Kept well below `global`, but loose enough that a couple of back-to-back scans or
        // an export alongside a sync do not trip it.
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
