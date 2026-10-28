//! Composes the provider fetch stack.
//!
//! The stack is built **per provider**, since each carries its own politeness and browser
//! identity. Outer to inner: `Backoff`, `RateLimited`, `Solving`, `Retry`, `Base`.
//!
//! The two waiting layers answer different questions and stay separate. `RateLimited` enforces
//! the budget **we** chose (rps, concurrency, crawl delay); `Backoff` honours the one the
//! **provider** asks for (`429` or `503` with `Retry-After`). `Backoff` is outside the limiter
//! so a retry re-acquires a token, and outside `Solving` because 429 and 503 are also challenge
//! statuses: below the solver it could not tell "slow down" from "solve this".

use crate::backoff::BackoffFetcher;
use crate::base::BaseHttpFetcher;
use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::ratelimit::{RateLimitedFetcher, ThrottlePolicy};
use crate::retry::RetryingFetcher;
use crate::solving::{SessionStore, SolvingFetcher};
use std::sync::Arc;
use std::time::Duration;
use tankovault_domain::BrowserEmulation;
use tankovault_solver::ChallengeSolver;

/// Holds everything needed to build a provider's fetch stack.
pub struct ProviderFetchConfig {
    /// The default user agent for ordinary requests.
    ///
    /// Ignored while `emulation` is `Some`, where only the profile's user agent matches the
    /// TLS and HTTP/2 fingerprint.
    pub user_agent: String,
    /// The browser to impersonate, or `None` to crawl as an identifiable bot.
    pub emulation: Option<BrowserEmulation>,
    /// Requests per second, already clamped to policy.
    pub rps: f64,
    /// The most concurrent requests.
    pub concurrency: u32,
    /// The crawl-delay floor between requests, in milliseconds.
    pub crawl_delay_ms: u64,
    /// The TCP connect timeout.
    pub connect_timeout: Duration,
    /// The whole-request timeout.
    pub request_timeout: Duration,
    /// The most attempts for transient transport and 5xx errors, the inner retry cap.
    pub max_attempts: u32,
    /// The most attempts when the provider answers `429` or `503`, for server-directed
    /// backoff.
    ///
    /// Small and separate from `max_attempts`: these waits run to seconds or minutes, and each
    /// holds a concurrency permit throughout.
    pub max_backoff_attempts: u32,
    /// The first wait after a `429` or `503` with no usable `Retry-After`; doubles per
    /// attempt.
    pub backoff_base: Duration,
    /// The ceiling on any single backoff wait, a server-supplied `Retry-After` included.
    pub backoff_max: Duration,
    /// How the rate limiter narrows its own budget when the provider answers `429` or `503`.
    ///
    /// `backoff_*` waits out one throttled request; this changes the pace of every later
    /// request, so a scan that tripped a limit stops re-tripping it.
    pub throttle: ThrottlePolicy,
    /// The challenge solver back-end: the HTTP client to the service, or a fake in tests.
    pub solver: Arc<dyn ChallengeSolver>,
    /// The solved-session cache.
    pub session_store: Arc<dyn SessionStore>,
}

impl ProviderFetchConfig {
    /// Builds the default config for a provider from its user agent, solver and session
    /// store.
    #[must_use]
    pub fn new(
        user_agent: impl Into<String>,
        solver: Arc<dyn ChallengeSolver>,
        session_store: Arc<dyn SessionStore>,
    ) -> Self {
        Self {
            user_agent: user_agent.into(),
            emulation: Some(BrowserEmulation::Chrome),
            rps: 1.0,
            concurrency: 2,
            crawl_delay_ms: 0,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            max_attempts: 4,
            // Three tries span about a minute of server-directed waiting before the task fails
            // and is rescheduled: enough to ride out a rate-limit window, short enough not to
            // pin a worker on a host that is down.
            max_backoff_attempts: 3,
            backoff_base: Duration::from_secs(2),
            backoff_max: Duration::from_secs(60),
            throttle: ThrottlePolicy::default(),
            solver,
            session_store,
        }
    }
}

/// Builds the composed, provider-scoped fetch stack.
///
/// # Errors
/// [`FetchError::Transport`] if the base HTTP client cannot be built.
pub fn build_provider_fetcher(cfg: ProviderFetchConfig) -> Result<Arc<dyn Fetcher>, FetchError> {
    let base = BaseHttpFetcher::new(
        cfg.user_agent,
        cfg.emulation,
        cfg.connect_timeout,
        cfg.request_timeout,
    )?;
    let retry = RetryingFetcher::new(
        base,
        cfg.max_attempts,
        Duration::from_millis(500),
        Duration::from_secs(30),
    );
    let solving = SolvingFetcher::new(retry, cfg.solver, cfg.session_store);
    let rated = RateLimitedFetcher::new(
        solving,
        cfg.rps,
        cfg.concurrency,
        cfg.crawl_delay_ms,
        cfg.throttle,
    );
    let backoff = BackoffFetcher::new(
        rated,
        cfg.max_backoff_attempts,
        cfg.backoff_base,
        cfg.backoff_max,
    );
    Ok(Arc::new(backoff))
}
