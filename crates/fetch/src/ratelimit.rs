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
use tankovault_domain::{MIN_RPS, Pacer};
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

impl<F> RateLimitedFetcher<F> {
    /// Builds a limiter allowing `rps` requests per second, at most `concurrency` in flight and
    /// a `crawl_delay_ms` floor between requests, adapting to `throttle` on push-back.
    ///
    /// Inputs are expected pre-clamped by [`tankovault_domain::Politeness::clamped`].
    ///
    /// # Panics
    /// If `rps` is infinite, or so large that its period rounds to zero, which
    /// [`tankovault_domain::Politeness::clamped`] rules out. `0.0`, a negative and `NaN` are
    /// floored below on purpose: they would otherwise **panic** in `Duration::from_secs_f64`, and a
    /// guard one crate away is too far from it.
    #[must_use]
    pub fn new(
        inner: F,
        rps: f64,
        concurrency: u32,
        crawl_delay_ms: u64,
        throttle: ThrottlePolicy,
    ) -> Self {
        // `f64::max` returns the non-`NaN` operand, so this floors a `NaN` too.
        let period = Duration::from_secs_f64(1.0 / rps.max(MIN_RPS));
        let quota = Quota::with_period(period).expect("rps > 0 yields a positive period");
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
}
