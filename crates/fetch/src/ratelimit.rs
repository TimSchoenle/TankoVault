//! Per-provider rate limiting: a `governor` cell rate for requests/second, a semaphore
//! for the concurrency ceiling, and an optional crawl-delay floor between requests.
//!
//! The fetch stack is built per provider, so a single direct limiter is exactly the
//! per-provider limiter the design calls for. (An aggregate cross-replica token bucket
//! in Redis is a documented follow-up — see `docs/IMPLEMENTATION_STATUS.md`.)

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Wraps an inner fetcher with rate + concurrency + crawl-delay controls.
pub struct RateLimitedFetcher<F> {
    inner: F,
    limiter: Arc<DefaultDirectRateLimiter>,
    concurrency: Arc<Semaphore>,
    crawl_delay: Duration,
}

impl<F> RateLimitedFetcher<F> {
    /// Build a limiter allowing `rps` requests/second and at most `concurrency` in flight,
    /// with a `crawl_delay_ms` floor between requests. Inputs are assumed pre-clamped to
    /// policy ceilings by [`tankovault_domain::Politeness::clamped`].
    #[must_use]
    pub fn new(inner: F, rps: f64, concurrency: u32, crawl_delay_ms: u64) -> Self {
        let period = Duration::from_secs_f64(1.0 / rps.max(f64::MIN_POSITIVE));
        let quota = Quota::with_period(period).expect("rps > 0 yields a positive period");
        Self {
            inner,
            limiter: Arc::new(RateLimiter::direct(quota)),
            concurrency: Arc::new(Semaphore::new(concurrency.max(1) as usize)),
            crawl_delay: Duration::from_millis(crawl_delay_ms),
        }
    }
}

#[async_trait]
impl<F: Fetcher> Fetcher for RateLimitedFetcher<F> {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        // Concurrency gate first, then the token rate, then the crawl-delay floor.
        let _permit = self
            .concurrency
            .acquire()
            .await
            .expect("semaphore is never closed");
        self.limiter.until_ready().await;
        if !self.crawl_delay.is_zero() {
            tokio::time::sleep(self.crawl_delay).await;
        }
        self.inner.get(req).await
    }
}
