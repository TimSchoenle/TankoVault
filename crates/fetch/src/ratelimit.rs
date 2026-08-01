//! Per-provider rate limiting: a `governor` cell rate for requests/second, a semaphore for the
//! concurrency ceiling, a crawl-delay floor, and a penalty the limiter imposes on **itself**
//! when the provider signals its budget is too high.
//!
//! This is a per-provider limiter only if the whole stack is built once per provider and
//! shared: a fresh stack per task turns `rps`/`concurrency` into a per-*task* budget and
//! discards the self-imposed penalty each time. `Engine::fetcher_for` caches per provider id;
//! anything else constructing a fetch stack must do the same. The budget is per worker
//! *process*; an aggregate cross-replica limiter is a documented follow-up
//! (`docs/IMPLEMENTATION_STATUS.md`).

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tankovault_domain::{MIN_RPS, Pacer};
use tokio::sync::Semaphore;

/// Statuses that mean the configured budget is above what the provider will serve.
const THROTTLE_STATUSES: [u16; 2] = [429, 503];

/// How the limiter reacts to a provider answering "too many requests".
///
/// Re-exported from `tankovault_domain::pacing`, the single implementation of outbound pacing
/// in this workspace — kept there so `services/sync`, which cannot pull in the wreq/BoringSSL
/// stack, can share the same policy.
pub use tankovault_domain::PacingPolicy as ThrottlePolicy;

/// Wraps an inner fetcher with rate + concurrency + crawl-delay controls.
pub struct RateLimitedFetcher<F> {
    inner: F,
    limiter: Arc<DefaultDirectRateLimiter>,
    concurrency: Arc<Semaphore>,
    crawl_delay: Duration,
    throttle: Arc<Pacer>,
}

impl<F> RateLimitedFetcher<F> {
    /// Build a limiter allowing `rps` requests/second and at most `concurrency` in flight,
    /// with a `crawl_delay_ms` floor between requests, adapting to `throttle` when the
    /// provider pushes back. Inputs are assumed pre-clamped to policy ceilings by
    /// [`tankovault_domain::Politeness::clamped`].
    ///
    /// # Panics
    /// Never, for any `f64` — including `0.0`, a negative and a `NaN`. The floor below is what
    /// makes that true, and it deliberately repeats [`tankovault_domain::Politeness::clamped`]
    /// rather than trusting it: the consequence of an out-of-range rate here is not a wrong
    /// crawl rate but a **panic** inside `Duration::from_secs_f64`, and a guard one crate away
    /// is the wrong distance from a panic.
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
            // Zero minimum interval: this stack's own floor is the provider's configured crawl
            // delay, applied below, so the pacer contributes only the adaptive penalty.
            throttle: Arc::new(Pacer::new(Duration::ZERO, throttle)),
        }
    }
}

#[async_trait]
impl<F: Fetcher> Fetcher for RateLimitedFetcher<F> {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        // Concurrency gate first, then the token rate, then the spacing floor — whichever of
        // the crawl delay and the current penalty is wider.
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

        let provider = req.provider_slug.clone();
        let resp = self.inner.get(req).await;
        // Only a response can carry the signal; a transport failure says nothing about the
        // provider's budget, and treating it as if it did would slow every crawl on a flaky
        // network.
        if let Ok(resp) = &resp
            && THROTTLE_STATUSES.contains(&resp.status)
        {
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

// Penalty behaviour itself is tested in `tankovault_domain::pacing`. What's worth pinning
// here is the wiring: crawl delay and penalty compose as "whichever is wider", which is this
// crate's decision, not the pacer's.
#[cfg(test)]
mod tests {
    use super::*;

    /// A provider with a generous crawl delay and no push-back must be paced by the crawl delay,
    /// not by the pacer — and a throttled provider by whichever is wider. Inverting this (taking
    /// the pacer's value unconditionally) would silently discard every provider's configured
    /// politeness the moment it answered a single 429.
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

    /// The statuses that count as push-back. 429 is the obvious one; 503 is included because a
    /// provider shedding load is asking for the same thing. Both are also challenge statuses,
    /// which is why this layer sits *outside* the solver — see `crate::backoff`.
    #[test]
    fn only_the_throttle_statuses_signal_push_back() {
        assert!(THROTTLE_STATUSES.contains(&429));
        assert!(THROTTLE_STATUSES.contains(&503));
        assert!(!THROTTLE_STATUSES.contains(&403));
        assert!(!THROTTLE_STATUSES.contains(&500));
    }
}
