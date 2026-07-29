//! Per-provider rate limiting: a `governor` cell rate for requests/second, a semaphore
//! for the concurrency ceiling, a crawl-delay floor between requests, and a penalty the
//! limiter imposes on **itself** when the provider says the budget is too high.
//!
//! A single direct limiter is exactly the per-provider limiter the design calls for **as
//! long as the whole stack is built once per provider and shared**. That is the caller's
//! responsibility and it is easy to get wrong: the worker used to build a fresh stack for
//! every scan task, which quietly turned `rps` and `concurrency` into a per-*task* budget —
//! N concurrent tasks then offered N × rps to the provider, and the self-imposed penalty
//! below was discarded each time. `Engine::fetcher_for` caches per provider id for that
//! reason. Anything else constructing a fetch stack must do the same.
//!
//! (An aggregate cross-replica token bucket in Redis is a documented follow-up — see
//! `docs/IMPLEMENTATION_STATUS.md`. Until then the budget is per worker *process*.)
//!
//! The configured budget is a guess made before the crawl: a provider's HTML pages and its
//! JSON API rarely share a limit, and neither is published. Without feedback the crawler
//! keeps offering the same rate a provider is already refusing, and a large scan spends
//! itself on `429`s — which is a slower way to fail than simply crawling slower.

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// Statuses that mean the configured budget is above what the provider will serve.
const THROTTLE_STATUSES: [u16; 2] = [429, 503];

/// How the limiter reacts to a provider answering "too many requests".
///
/// Additive-then-multiplicative on the way up, halving on the way down: a crawl that trips a
/// limit backs well off it quickly, and returns to full speed slowly enough not to trip it
/// again on the way.
#[derive(Debug, Clone, Copy)]
pub struct ThrottlePolicy {
    /// Spacing added by the first throttle signal, and the floor for any non-zero penalty.
    pub step: Duration,
    /// Ceiling on the added spacing, so a provider that answers `429` unconditionally
    /// (or a misread body) cannot park a worker indefinitely.
    pub max: Duration,
    /// Throttle-free time after which the penalty halves.
    pub recovery: Duration,
}

impl Default for ThrottlePolicy {
    fn default() -> Self {
        Self {
            // A half-second of extra spacing is a large correction at crawl rates of a few
            // rps, and small enough that a single stray 429 costs almost nothing.
            step: Duration::from_millis(500),
            // 8s spacing is ~0.125 rps: slow, but still progressing. Past this the provider
            // is not rate-limiting us, it is refusing us, and that is the backoff layer's
            // and the scheduler's problem, not the limiter's.
            max: Duration::from_secs(8),
            recovery: Duration::from_secs(60),
        }
    }
}

/// The self-imposed spacing penalty, shared by every request on this provider's stack.
struct Throttle {
    policy: ThrottlePolicy,
    state: Mutex<PenaltyState>,
}

#[derive(Debug, Clone, Copy)]
struct PenaltyState {
    penalty: Duration,
    /// When the penalty may next halve. Meaningless while `penalty` is zero.
    next_decay: Instant,
}

impl Throttle {
    fn new(policy: ThrottlePolicy) -> Self {
        Self {
            policy,
            state: Mutex::new(PenaltyState {
                penalty: Duration::ZERO,
                next_decay: Instant::now(),
            }),
        }
    }

    /// Lock the penalty state.
    ///
    /// Poisoning is recovered from rather than propagated: the state is two plain values with
    /// no invariant a panicking thread could have broken, and refusing to fetch for the rest
    /// of a process' life is a far worse failure than resuming with a stale penalty.
    fn state(&self) -> std::sync::MutexGuard<'_, PenaltyState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The spacing to apply before the next request, decaying the penalty if the provider has
    /// been quiet for a full recovery window.
    fn spacing(&self, now: Instant) -> Duration {
        let mut state = self.state();
        if !state.penalty.is_zero() && now >= state.next_decay {
            let halved = state.penalty / 2;
            state.penalty = if halved < self.policy.step {
                Duration::ZERO
            } else {
                halved
            };
            state.next_decay = now + self.policy.recovery;
        }
        state.penalty
    }

    /// Record a throttle signal, widening the spacing.
    fn penalise(&self, now: Instant) -> Duration {
        let mut state = self.state();
        state.penalty = if state.penalty.is_zero() {
            self.policy.step
        } else {
            (state.penalty * 2).min(self.policy.max)
        };
        state.next_decay = now + self.policy.recovery;
        state.penalty
    }
}

/// Wraps an inner fetcher with rate + concurrency + crawl-delay controls.
pub struct RateLimitedFetcher<F> {
    inner: F,
    limiter: Arc<DefaultDirectRateLimiter>,
    concurrency: Arc<Semaphore>,
    crawl_delay: Duration,
    throttle: Arc<Throttle>,
}

impl<F> RateLimitedFetcher<F> {
    /// Build a limiter allowing `rps` requests/second and at most `concurrency` in flight,
    /// with a `crawl_delay_ms` floor between requests, adapting to `throttle` when the
    /// provider pushes back. Inputs are assumed pre-clamped to policy ceilings by
    /// [`tankovault_domain::Politeness::clamped`].
    #[must_use]
    pub fn new(
        inner: F,
        rps: f64,
        concurrency: u32,
        crawl_delay_ms: u64,
        throttle: ThrottlePolicy,
    ) -> Self {
        let period = Duration::from_secs_f64(1.0 / rps.max(f64::MIN_POSITIVE));
        let quota = Quota::with_period(period).expect("rps > 0 yields a positive period");
        Self {
            inner,
            limiter: Arc::new(RateLimiter::direct(quota)),
            concurrency: Arc::new(Semaphore::new(concurrency.max(1) as usize)),
            crawl_delay: Duration::from_millis(crawl_delay_ms),
            throttle: Arc::new(Throttle::new(throttle)),
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
        let spacing = self.crawl_delay.max(self.throttle.spacing(Instant::now()));
        if !spacing.is_zero() {
            tokio::time::sleep(spacing).await;
        }

        let provider = req.provider_slug.clone();
        let resp = self.inner.get(req).await;
        // Only a response can carry the signal; a transport failure says nothing about the
        // provider's budget, and treating it as if it did would slow every crawl on a flaky
        // network.
        if let Ok(resp) = &resp {
            if THROTTLE_STATUSES.contains(&resp.status) {
                let penalty = self.throttle.penalise(Instant::now());
                tracing::info!(
                    %provider,
                    status = resp.status,
                    url = %resp.url,
                    spacing_ms = penalty.as_millis(),
                    "provider signalled rate limiting; widening this provider's request spacing"
                );
            }
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn throttle() -> Throttle {
        Throttle::new(ThrottlePolicy {
            step: Duration::from_millis(500),
            max: Duration::from_secs(2),
            recovery: Duration::from_secs(60),
        })
    }

    #[test]
    fn an_unthrottled_provider_is_not_slowed() {
        assert_eq!(throttle().spacing(Instant::now()), Duration::ZERO);
    }

    #[test]
    fn spacing_widens_per_signal_and_stops_at_the_ceiling() {
        let t = throttle();
        let now = Instant::now();
        assert_eq!(t.penalise(now), Duration::from_millis(500));
        assert_eq!(t.penalise(now), Duration::from_secs(1));
        assert_eq!(t.penalise(now), Duration::from_secs(2));
        assert_eq!(t.penalise(now), Duration::from_secs(2), "capped at max");
    }

    #[test]
    fn spacing_holds_until_a_quiet_recovery_window_has_passed() {
        let t = throttle();
        let now = Instant::now();
        t.penalise(now);
        t.penalise(now);
        assert_eq!(
            t.spacing(now + Duration::from_secs(59)),
            Duration::from_secs(1)
        );
        assert_eq!(
            t.spacing(now + Duration::from_secs(61)),
            Duration::from_millis(500),
            "one window of quiet halves the penalty"
        );
    }

    #[test]
    fn spacing_returns_to_zero_rather_than_decaying_forever() {
        let t = throttle();
        let mut now = Instant::now();
        t.penalise(now);
        // A penalty that would halve below the step is dropped outright: sub-step spacing is
        // indistinguishable from none and would keep the provider marked as throttled.
        now += Duration::from_secs(61);
        assert_eq!(t.spacing(now), Duration::ZERO);
    }
}
