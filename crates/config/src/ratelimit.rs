//! Inbound request rate limiting for a service's HTTP edge.

use serde::Deserialize;

/// Where rate-limit counters live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateLimitBackend {
    /// Process-local counters. Correct for a single replica and for tests; with `N`
    /// replicas behind a load balancer the effective limit is `N` times the configured one.
    #[default]
    Memory,
    /// Shared counters in Redis, so the limit holds across every replica. Requires a
    /// `redis` block; the limiter fails **open** (allows the request) if Redis is
    /// unreachable, since a counter-store outage must not take the edge down.
    Redis,
}

/// A single token-bucket policy: sustained refill rate plus bucket depth.
///
/// The two numbers control different things and are deliberately independent:
/// [`Self::per_minute`] is how fast the bucket refills, [`Self::burst`] is how deep it is.
/// A burst *below* the sustained rate is the normal case, not a misconfiguration — the
/// default global policy allows 300 requests/minute but at most 60 back-to-back, which
/// absorbs a page load without letting a client spend a whole minute's budget instantly.
#[derive(Debug, Clone, Copy, Deserialize)]
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

    /// Bucket capacity: the most requests a client can make in one instant.
    ///
    /// Clamped to at least 1 — a zero-depth bucket would reject every request forever,
    /// which is never what a misconfigured `0` is meant to express.
    #[must_use]
    pub const fn capacity(&self) -> u32 {
        if self.burst == 0 { 1 } else { self.burst }
    }
}

/// Inbound request rate limiting for a service's HTTP edge.
///
/// Distinct from the *outbound* crawl politeness in `tankovault_domain::pacing`, which paces
/// requests this system makes to third-party providers.
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    /// Enforce limits. When `false` the layer is not mounted at all (no per-request cost).
    #[serde(default = "crate::default_true")]
    pub enabled: bool,
    /// Counter store. See [`RateLimitBackend`].
    #[serde(default)]
    pub backend: RateLimitBackend,
    /// Applies to any route without a stricter class below.
    #[serde(default = "RateLimitConfig::default_global")]
    pub global: RateLimitPolicy,
    /// Credential-handling routes (login, register, password reset, token refresh). Tight
    /// by design — this is the online-guessing control, so it is deliberately far below
    /// [`Self::global`].
    #[serde(default = "RateLimitConfig::default_auth")]
    pub auth: RateLimitPolicy,
    /// Routes that are cheap to call and expensive to serve (data export, scan triggers,
    /// sync push/pull).
    #[serde(default = "RateLimitConfig::default_expensive")]
    pub expensive: RateLimitPolicy,
    /// Trust `X-Forwarded-For` / `X-Real-IP` when deriving the client key.
    ///
    /// **Only enable behind a reverse proxy that overwrites these headers.** With this on
    /// and no such proxy, any client can forge a fresh identity per request and bypass the
    /// limiter entirely — hence the safe default of `false`.
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
        // Cheap-to-ask, costly-to-serve routes. Kept well below `global`, but the previous
        // `6/min, burst 2` was tight enough that an operator triggering a couple of scans
        // back-to-back — or an account page firing its export alongside a sync — tripped it.
        // A shallow double still throttles abuse while leaving room for legitimate bursts.
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
