//! Process-local rate-limit counters, backed by `governor`.
//!
//! Correct for a single replica and for tests. With `N` replicas behind a load balancer
//! the effective limit is `N` times the configured one — acceptable for the reference
//! single-node deployment, and the reason [`super::redis::RedisStore`] exists.

use super::{RateLimitDecision, RateLimitStore, RouteClass};
use async_trait::async_trait;
use governor::clock::{Clock, DefaultClock};
use governor::middleware::{NoOpMiddleware, StateInformationMiddleware};
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

type KeyedLimiter<C> =
    RateLimiter<String, DefaultKeyedStateStore<String>, C, StateInformationMiddleware>;

/// One `governor` keyed limiter per [`RouteClass`].
///
/// Separate limiters rather than one with a per-call quota because a `governor` quota is
/// fixed at construction — which is also what lets each class have genuinely independent
/// buckets instead of sharing one budget.
///
/// # The clock is a parameter
///
/// `C` defaults to [`DefaultClock`], so every production call site writes `MemoryStore` and is
/// unaffected. It exists because the two behaviours that are *only* expressible in terms of
/// elapsed time — a spent bucket refilling, and [`SWEEP_EVERY_N_CHECKS`] reclaiming keys whose
/// buckets have — cannot otherwise be asserted without sleeping for a minute in a unit test,
/// which is why neither was asserted at all. `governor` already models the clock as a trait
/// ([`governor::clock::FakeRelativeClock`] is its test implementation), so this needs no port of
/// our own: TESTING F-09's "rate-limit windows" axis is a type parameter, not a `Clock` service.
pub struct MemoryStore<C: Clock = DefaultClock> {
    limiters: [KeyedLimiter<C>; RouteClass::COUNT],
    limits: [u32; RouteClass::COUNT],
    checks_since_sweep: AtomicU32,
    clock: C,
}

impl MemoryStore {
    /// Build a limiter per class from `cfg`, against the real monotonic clock.
    #[must_use]
    pub fn new(cfg: &RateLimitConfig) -> Self {
        Self::with_clock(cfg, DefaultClock::default())
    }
}

impl<C: Clock + Clone> MemoryStore<C> {
    /// As [`MemoryStore::new`], against a caller-supplied clock.
    fn with_clock(cfg: &RateLimitConfig, clock: C) -> Self {
        let limiters = RouteClass::ALL.map(|class| build_limiter(class.policy(cfg), clock.clone()));
        let limits = RouteClass::ALL.map(|class| class.policy(cfg).capacity());
        Self {
            limiters,
            limits,
            checks_since_sweep: AtomicU32::new(0),
            clock,
        }
    }
}

impl<C: Clock> MemoryStore<C> {
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

    /// How many client keys each class is currently holding state for.
    ///
    /// Test-only, and the reason it exists is that the sweep's whole purpose is invisible from
    /// the outside: an unswept store answers every request identically to a swept one and
    /// simply grows.
    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.limiters.iter().map(RateLimiter::len).sum()
    }
}

/// A `governor` quota from a policy.
///
/// `per_minute` sets the sustained refill rate and `capacity` the burst the bucket
/// tolerates. Both are clamped to at least 1: a zero-rate limiter would reject every
/// request forever, which is never what a misconfigured number is meant to express.
fn build_limiter<C: Clock>(policy: RateLimitPolicy, clock: C) -> KeyedLimiter<C> {
    let per_minute = NonZeroU32::new(policy.per_minute.max(1)).expect("clamped to >= 1");
    let capacity = NonZeroU32::new(policy.capacity().max(1)).expect("clamped to >= 1");
    let quota = Quota::per_minute(per_minute).allow_burst(capacity);
    // `RateLimiter`'s default middleware parameter is `NoOpMiddleware<QuantaInstant>`, which is
    // only inhabitable for the default clock — hence the explicit `NoOpMiddleware<C::Instant>`
    // before swapping in the one that reports remaining capacity.
    let limiter: RateLimiter<
        String,
        DefaultKeyedStateStore<String>,
        C,
        NoOpMiddleware<C::Instant>,
    > = RateLimiter::new(quota, DefaultKeyedStateStore::default(), clock);
    limiter.with_middleware::<StateInformationMiddleware>()
}

#[async_trait]
impl<C: Clock + Send + Sync + 'static> RateLimitStore for MemoryStore<C> {
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
    use governor::clock::FakeRelativeClock;
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

    /// A spent bucket refills at the configured rate — the half of a rate limit that only
    /// exists in time, and the half nothing asserted on.
    ///
    /// Every test above proves the limiter says *no*; none proved it ever says yes again. A
    /// limiter that denied forever after the first burst would pass all of them, and the symptom
    /// in production is a client locked out until the process restarts. 60/minute is one token
    /// per second, so the assertions below are exactly the quota arithmetic.
    #[tokio::test]
    async fn a_spent_bucket_refills_at_the_configured_rate() {
        let clock = FakeRelativeClock::default();
        let store = MemoryStore::with_clock(&config(60, 2), clock.clone());
        let key = "ip:203.0.113.7";

        assert!(store.check(RouteClass::Global, key).await.allowed);
        assert!(store.check(RouteClass::Global, key).await.allowed);
        let denied = store.check(RouteClass::Global, key).await;
        assert!(!denied.allowed, "the two-deep burst is spent");
        assert!(
            denied.retry_after <= Duration::from_secs(1),
            "one token per second, so the wait is at most a second: {:?}",
            denied.retry_after
        );

        // Half a token's worth of time buys nothing…
        clock.advance(Duration::from_millis(400));
        assert!(!store.check(RouteClass::Global, key).await.allowed);
        // …a whole one buys exactly one request back.
        clock.advance(Duration::from_millis(600));
        assert!(store.check(RouteClass::Global, key).await.allowed);
        assert!(
            !store.check(RouteClass::Global, key).await.allowed,
            "one second of refill is one token, not a reset to full burst"
        );
    }

    /// The sweep reclaims keys whose buckets have fully refilled.
    ///
    /// This is the memory bound, and it was asserted by nothing: a store that never swept
    /// answers every request identically to one that does and simply grows, one entry per
    /// distinct client key, for the life of the process. The clock is what makes it observable
    /// — `retain_recent` keeps any key still mid-refill, so without advancing time the sweep
    /// runs and correctly drops nothing, which is indistinguishable from not running.
    #[tokio::test]
    async fn the_sweep_reclaims_keys_whose_buckets_have_refilled() {
        let clock = FakeRelativeClock::default();
        let store = MemoryStore::with_clock(&config(60, 1), clock.clone());

        for i in 0..100 {
            let _ = store
                .check(RouteClass::Global, &format!("ip:198.51.100.{i}"))
                .await;
        }
        assert_eq!(store.tracked_keys(), 100, "one entry per distinct client");

        // Long enough for every bucket to be back at full capacity, so all 100 are reclaimable.
        clock.advance(Duration::from_secs(120));
        for _ in 0..=SWEEP_EVERY_N_CHECKS {
            let _ = store.check(RouteClass::Global, "ip:192.0.2.55").await;
        }

        assert_eq!(
            store.tracked_keys(),
            1,
            "only the key still being exercised should survive the sweep"
        );
    }

    /// …and the sweep must **not** drop a key that is still rate-limited, which is the way to
    /// get this wrong that has a security consequence rather than a memory one: reclaiming a
    /// live bucket hands the client a fresh burst on the spot, so a caller could evade the limit
    /// indefinitely by staying in the map long enough to be swept.
    #[tokio::test]
    async fn the_sweep_keeps_a_bucket_that_is_still_spent() {
        let clock = FakeRelativeClock::default();
        let store = MemoryStore::with_clock(&config(60, 1), clock.clone());
        let throttled = "ip:203.0.113.200";

        assert!(store.check(RouteClass::Global, throttled).await.allowed);
        assert!(!store.check(RouteClass::Global, throttled).await.allowed);

        // Sweep without advancing the clock: the bucket is mid-refill and must be kept.
        for _ in 0..=SWEEP_EVERY_N_CHECKS {
            let _ = store.check(RouteClass::Auth, "ip:192.0.2.56").await;
        }

        assert!(
            !store.check(RouteClass::Global, throttled).await.allowed,
            "a swept-away bucket would come back empty, i.e. with a full burst"
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
