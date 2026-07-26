//! Process-local rate-limit counters, backed by `governor`.
//!
//! Correct for a single replica and for tests. With `N` replicas behind a load balancer
//! the effective limit is `N` times the configured one — acceptable for the reference
//! single-node deployment, and the reason [`super::redis::RedisStore`] exists.

use super::{RateLimitDecision, RateLimitStore, RouteClass};
use async_trait::async_trait;
use governor::clock::{Clock, DefaultClock};
use governor::middleware::StateInformationMiddleware;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, Ordering};
use tankovault_config::{RateLimitConfig, RateLimitPolicy};

/// Checks between sweeps of keys whose buckets have fully refilled.
///
/// The keyed store allocates one entry per distinct client key and never reclaims them on
/// its own, so a long-lived process facing many source addresses would grow without
/// bound. Sweeping on a request counter rather than a timer keeps this dependency-free
/// and self-tuning: a busy edge sweeps often, an idle one holds a handful of stale keys.
const SWEEP_EVERY_N_CHECKS: u32 = 10_000;

type KeyedLimiter =
    RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock, StateInformationMiddleware>;

/// One `governor` keyed limiter per [`RouteClass`].
///
/// Separate limiters rather than one with a per-call quota because a `governor` quota is
/// fixed at construction — which is also what lets each class have genuinely independent
/// buckets instead of sharing one budget.
pub struct MemoryStore {
    limiters: [KeyedLimiter; RouteClass::COUNT],
    limits: [u32; RouteClass::COUNT],
    checks_since_sweep: AtomicU32,
    clock: DefaultClock,
}

impl MemoryStore {
    /// Build a limiter per class from `cfg`.
    #[must_use]
    pub fn new(cfg: &RateLimitConfig) -> Self {
        let limiters = RouteClass::ALL.map(|class| build_limiter(class.policy(cfg)));
        let limits = RouteClass::ALL.map(|class| class.policy(cfg).capacity());
        Self {
            limiters,
            limits,
            checks_since_sweep: AtomicU32::new(0),
            clock: DefaultClock::default(),
        }
    }

    /// Drop keys whose buckets have fully refilled, every [`SWEEP_EVERY_N_CHECKS`] checks.
    fn maybe_sweep(&self) {
        let count = self.checks_since_sweep.fetch_add(1, Ordering::Relaxed);
        if count < SWEEP_EVERY_N_CHECKS {
            return;
        }
        self.checks_since_sweep.store(0, Ordering::Relaxed);
        for limiter in &self.limiters {
            limiter.retain_recent();
        }
    }
}

/// A `governor` quota from a policy.
///
/// `per_minute` sets the sustained refill rate and `capacity` the burst the bucket
/// tolerates. Both are clamped to at least 1: a zero-rate limiter would reject every
/// request forever, which is never what a misconfigured number is meant to express.
fn build_limiter(policy: RateLimitPolicy) -> KeyedLimiter {
    let per_minute = NonZeroU32::new(policy.per_minute.max(1)).expect("clamped to >= 1");
    let capacity = NonZeroU32::new(policy.capacity().max(1)).expect("clamped to >= 1");
    let quota = Quota::per_minute(per_minute).allow_burst(capacity);
    RateLimiter::keyed(quota).with_middleware::<StateInformationMiddleware>()
}

#[async_trait]
impl RateLimitStore for MemoryStore {
    async fn check(&self, class: RouteClass, key: &str) -> RateLimitDecision {
        self.maybe_sweep();

        let index = class.index();
        let limit = self.limits[index];
        match self.limiters[index].check_key(&key.to_owned()) {
            Ok(snapshot) => RateLimitDecision::allow(limit, snapshot.remaining_burst_capacity()),
            Err(not_until) => {
                let wait = not_until.wait_time_from(self.clock.now());
                RateLimitDecision::deny(limit, wait)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn config(per_minute: u32, burst: u32) -> RateLimitConfig {
        RateLimitConfig {
            global: RateLimitPolicy::new(per_minute, burst),
            auth: RateLimitPolicy::new(per_minute, burst),
            expensive: RateLimitPolicy::new(per_minute, burst),
            ..RateLimitConfig::default()
        }
    }

    #[tokio::test]
    async fn requests_within_the_burst_are_allowed_then_denied() {
        let store = MemoryStore::new(&config(60, 3));

        for i in 0..3 {
            let decision = store.check(RouteClass::Global, "ip:203.0.113.1").await;
            assert!(decision.allowed, "request {i} should fit inside the burst");
        }

        let denied = store.check(RouteClass::Global, "ip:203.0.113.1").await;
        assert!(!denied.allowed, "the burst is spent");
        assert_eq!(denied.remaining, 0);
        assert!(denied.retry_after > Duration::ZERO);
    }

    #[tokio::test]
    async fn keys_have_independent_buckets() {
        let store = MemoryStore::new(&config(60, 1));

        assert!(
            store
                .check(RouteClass::Global, "ip:198.51.100.1")
                .await
                .allowed
        );
        assert!(
            !store
                .check(RouteClass::Global, "ip:198.51.100.1")
                .await
                .allowed,
            "the first client has spent its burst"
        );
        assert!(
            store
                .check(RouteClass::Global, "ip:198.51.100.2")
                .await
                .allowed,
            "a different client must not inherit another's exhausted bucket"
        );
    }

    #[tokio::test]
    async fn classes_have_independent_buckets() {
        let store = MemoryStore::new(&config(60, 1));

        assert!(
            store
                .check(RouteClass::Auth, "ip:203.0.113.9")
                .await
                .allowed
        );
        assert!(
            !store
                .check(RouteClass::Auth, "ip:203.0.113.9")
                .await
                .allowed
        );
        assert!(
            store
                .check(RouteClass::Global, "ip:203.0.113.9")
                .await
                .allowed,
            "exhausting the auth budget must not block ordinary reads"
        );
    }

    #[tokio::test]
    async fn a_zero_rate_policy_does_not_deny_everything() {
        // A misconfigured `0` reads as "I did not think about this", not "reject all
        // traffic"; clamping to 1/min keeps the service usable and visibly throttled.
        let store = MemoryStore::new(&config(0, 0));
        assert!(
            store
                .check(RouteClass::Global, "ip:192.0.2.1")
                .await
                .allowed
        );
    }

    #[test]
    fn capacity_is_the_burst_depth_independent_of_the_refill_rate() {
        // Regression: capacity once clamped the burst *up* to `per_minute`, so the
        // shipped default of 300/min with a 60-deep bucket actually allowed 300
        // back-to-back requests — the burst setting had no effect at all.
        assert_eq!(RateLimitPolicy::new(300, 60).capacity(), 60);
        assert_eq!(RateLimitPolicy::new(60, 90).capacity(), 90);
        assert_eq!(
            RateLimitPolicy::new(60, 0).capacity(),
            1,
            "a zero-depth bucket must not reject everything forever"
        );
    }
}
