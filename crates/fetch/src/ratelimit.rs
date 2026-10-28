//! Owns per-provider rate limiting: a `governor` cell rate for requests per second, a semaphore
//! for the concurrency ceiling, a crawl-delay floor, and a penalty the limiter imposes on
//! **itself** when the provider signals its budget is too high.
//!
//! It limits per provider only if the stack is built once per provider and shared: a fresh
//! stack per task makes `rps` and `concurrency` a per-task budget and discards the penalty each
//! time. `Engine::fetcher_for` caches per provider id, and anything else building a fetch stack
//! must too. The budget is per worker process, so N replicas crawl a provider at N times the
//! configured rate.

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tankovault_domain::{MAX_RPS, MIN_RPS, Pacer};
use tokio::sync::Semaphore;

/// The statuses that mean the configured budget is above what the provider will serve.
const THROTTLE_STATUSES: [u16; 2] = [429, 503];

/// Sets how the limiter reacts to a provider answering "too many requests".
///
/// Re-exported from `tankovault_domain::pacing`, the one implementation of outbound pacing, which
/// lives there so `services/sync` can share it without the `wreq` and `BoringSSL` stack.
pub use tankovault_domain::PacingPolicy as ThrottlePolicy;

/// Wraps an inner fetcher with rate, concurrency and crawl-delay controls.
pub struct RateLimitedFetcher<F> {
    inner: F,
    limiter: Arc<DefaultDirectRateLimiter>,
    concurrency: Arc<Semaphore>,
    crawl_delay: Duration,
    throttle: Arc<Pacer>,
}

/// The interval between requests at `rps`, after clamping it to `[MIN_RPS, MAX_RPS]`.
fn period_for(rps: f64) -> Duration {
    // The ceiling keeps the period above zero, which `Quota::with_period` refuses.
    #[expect(
        clippy::manual_clamp,
        reason = "`f64::clamp` returns a NaN input unchanged; `f64::max` floors it to MIN_RPS"
    )]
    Duration::from_secs_f64(1.0 / rps.max(MIN_RPS).min(MAX_RPS))
}

impl<F> RateLimitedFetcher<F> {
    /// Builds a limiter allowing `rps` requests per second, at most `concurrency` in flight and
    /// a `crawl_delay_ms` floor between requests, adapting to `throttle` on push-back.
    ///
    /// Inputs are expected pre-clamped by [`tankovault_domain::Politeness::clamped`].
    ///
    /// # Panics
    /// Never, for any `f64`, including `0.0`, a negative, `NaN` and infinity. `period_for`
    /// repeats the clamp in [`tankovault_domain::Politeness::clamped`] on purpose: out of range
    /// here is a **panic** in `Duration::from_secs_f64` or the zero-period `expect`, not a
    /// wrong crawl rate, and a guard one crate away is too far from it.
    #[must_use]
    pub fn new(
        inner: F,
        rps: f64,
        concurrency: u32,
        crawl_delay_ms: u64,
        throttle: ThrottlePolicy,
    ) -> Self {
        let quota = Quota::with_period(period_for(rps)).expect("rps > 0 yields a positive period");
        Self {
            inner,
            limiter: Arc::new(RateLimiter::direct(quota)),
            concurrency: Arc::new(Semaphore::new(concurrency.max(1) as usize)),
            crawl_delay: Duration::from_millis(crawl_delay_ms),
            // A zero minimum interval: the floor is the provider's crawl delay, applied below, so
            // the pacer contributes only the adaptive penalty.
            throttle: Arc::new(Pacer::new(Duration::ZERO, throttle)),
        }
    }
}

#[async_trait]
impl<F: Fetcher> Fetcher for RateLimitedFetcher<F> {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        // The concurrency gate first, then the token rate, then the spacing floor: the wider of
        // the crawl delay and the current penalty.
        //
        // Measured as one span: everything until the send is waiting for permission, not for
        // the provider, and that total is what tells a polite scan from a stuck one.
        let gate_entered = Instant::now();
        let _permit = self
            .concurrency
            .acquire()
            .await
            .expect("semaphore is never closed");
        self.limiter.until_ready().await;
        let spacing = self.crawl_delay.max(self.throttle.penalty(Instant::now()));
        if !spacing.is_zero() {
            tokio::time::sleep(spacing).await;
        }
        let waited = gate_entered.elapsed();
        crate::accounting::record(crate::accounting::Metered::PaceWait(waited));
        metrics::histogram!(
            "provider_pace_wait_seconds",
            "provider" => req.provider_slug.clone(),
        )
        .record(waited.as_secs_f64());

        let provider = req.provider_slug.clone();
        let resp = self.inner.get(req).await;
        // Only a response carries the signal: a transport failure says nothing about the
        // provider's budget, and counting it would slow every crawl on a flaky network.
        if let Ok(resp) = &resp
            && THROTTLE_STATUSES.contains(&resp.status)
        {
            crate::accounting::record(crate::accounting::Metered::Throttled);
            let now = Instant::now();
            self.throttle.penalise(now, None);
            let penalty = self.throttle.penalty(now);
            tracing::info!(
                %provider,
                status = resp.status,
                url = %resp.url,
                spacing_ms = penalty.as_millis(),
                "provider signalled rate limiting; widening this provider's request spacing"
            );
        }
        resp
    }
}

// The penalty itself is tested in `tankovault_domain::pacing`. This pins the wiring: crawl
// delay and penalty compose as the wider of the two, which is this crate's decision.
#[cfg(test)]
mod tests {
    use super::*;

    /// The crawl delay and the penalty compose as the wider of the two.
    ///
    /// Taking the pacer's value unconditionally would discard a provider's configured politeness
    /// the moment it answered a single 429.
    #[test]
    fn the_crawl_delay_and_the_penalty_compose_as_the_wider_of_the_two() {
        let crawl_delay = Duration::from_secs(3);
        let pacer = Pacer::new(Duration::ZERO, ThrottlePolicy::default());
        let now = Instant::now();

        assert_eq!(
            crawl_delay.max(pacer.penalty(now)),
            crawl_delay,
            "an unthrottled provider is paced by its own crawl delay"
        );

        // Enough signals to grow the penalty past the crawl delay.
        for _ in 0..4 {
            pacer.penalise(now, None);
        }
        assert!(
            crawl_delay.max(pacer.penalty(now)) > crawl_delay,
            "once the penalty exceeds the crawl delay it takes over"
        );
    }

    /// Only 429 and 503 count as push-back: a provider shedding load asks for the same thing as
    /// one rate-limiting. Both are also challenge statuses, which is why this layer sits outside
    /// the solver.
    #[test]
    fn only_the_throttle_statuses_signal_push_back() {
        assert!(THROTTLE_STATUSES.contains(&429));
        assert!(THROTTLE_STATUSES.contains(&503));
        assert!(!THROTTLE_STATUSES.contains(&403));
        assert!(!THROTTLE_STATUSES.contains(&500));
    }

    /// An infinite rate panicked: `1.0 / inf` is a zero period, which `Quota::with_period`
    /// refuses, and the `expect` on it fired while the doc promised no `f64` could panic.
    #[test]
    fn no_rate_panics_the_constructor() {
        for rps in [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            1e300,
        ] {
            let _ = RateLimitedFetcher::new((), rps, 1, 0, ThrottlePolicy::default());
        }
    }

    /// The constructor's clamp has to reach the limiter, not only keep it from panicking: an
    /// unclamped or unwired period would crawl at the wrong rate with nothing failing.
    #[test]
    fn the_limiter_period_is_the_clamped_rate() {
        let floor = Duration::from_secs_f64(1.0 / MIN_RPS);
        let ceiling = Duration::from_secs_f64(1.0 / MAX_RPS);
        for rps in [0.0, -1.0, f64::NAN, f64::NEG_INFINITY] {
            assert_eq!(period_for(rps), floor, "rps {rps} must floor to MIN_RPS");
        }
        for rps in [f64::INFINITY, f64::MAX, 1e300] {
            assert_eq!(period_for(rps), ceiling, "rps {rps} must cap at MAX_RPS");
        }
        assert_eq!(period_for(4.0), Duration::from_millis(250));
    }
}
